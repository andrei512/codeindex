use super::{Language, Role, Roles, Syntax};
use Language::*;
use Role::{Support, Test};

pub(super) fn has_syntax(lang: Language) -> bool {
    match lang {
        Rust | Js | Python | Go | Java | Kotlin | Swift | Csharp | ObjectiveC | Php | Hack | C
        | Cpp | Powershell | Shell | Ruby | Lua | Dart | CMake | Starlark | Hcl | Perl | Sql
        | PlPgSql | Html | Makefile | Nix | Batchfile | Proto => true,
        Css | Dockerfile | Procfile | Assembly | Mako => false,
    }
}

fn test_name(name: &str) -> bool {
    name.strip_prefix("test")
        .is_some_and(|s| s.starts_with('_') || s.starts_with(char::is_uppercase))
}

pub(super) fn collect(lang: Language, path: &str, s: &Syntax, res: &mut Roles) {
    if lang == Python {
        return super::python::collect(path, s, res);
    }
    let test_file = res.base.0 == Support;
    let has_word = |word: &str| s.tokens.iter().any(|t| t.text == word);
    let suites = super::suites::suites(lang, s, test_file);
    let in_suite = |i| suites.iter().any(|suite| suite.contains(i));
    let dart_framework = lang == Dart
        && s.tokens.windows(2).any(|t| {
            t[0].text == "import"
                && matches!(
                    t[1].text.trim_matches(['\'', '"']),
                    "package:test/test.dart" | "package:flutter_test/flutter_test.dart"
                )
        });
    let framework_annotations = match lang {
        Java | Kotlin => {
            has_word("junit")
                || has_word("testng")
                || has_word("kotest")
                || s.tokens
                    .windows(3)
                    .any(|w| w[0].text == "kotlin" && w[1].text == "." && w[2].text == "test")
        }
        Csharp => ["Xunit", "NUnit", "TestTools"].iter().any(|w| has_word(w)),
        Swift => has_word("Testing"),
        _ => false,
    };
    let test_context = test_file || !suites.is_empty() || dart_framework || framework_annotations;
    for (i, t) in s.tokens.iter().enumerate() {
        let w = t.text;
        let before = i.checked_sub(1).map_or("", |p| s.w(p));
        match lang {
            Rust if w == "#" && s.w(i + 1) == "[" => {
                let Some(close) = s.close(i + 1) else {
                    continue;
                };
                let attr = &s.tokens[i + 2..close];
                let head = attr
                    .iter()
                    .take_while(|x| x.text != "(")
                    .map(|x| x.text)
                    .collect::<Vec<_>>();
                let test = matches!(
                    head.as_slice(),
                    ["test"] | ["tokio" | "async_std" | "actix_rt", ":", ":", "test"]
                );
                let gated = s.seq(i + 2, &["cfg", "(", "test", ")", "]"]);
                if test || gated {
                    s.add(
                        res,
                        i,
                        s.body(close + 1),
                        if test { Test } else { Support },
                        if test {
                            "test attribute"
                        } else {
                            "test-only compilation"
                        },
                    );
                }
            }
            Go if w == "func" && path.ends_with("_test.go") => {
                let name = s.w(i + 1);
                if s.w(i + 2) != "(" {
                    continue;
                }
                let Some(close) = s.close(i + 2) else {
                    continue;
                };
                let valid = ["Test", "Benchmark", "Fuzz", "Example"]
                    .into_iter()
                    .any(|prefix| {
                        name.strip_prefix(prefix)
                            .is_some_and(|rest| !rest.starts_with(char::is_lowercase))
                            && (if prefix == "Example" {
                                close == i + 3
                            } else {
                                let expected = match prefix {
                                    "Test" => "T",
                                    "Benchmark" => "B",
                                    _ => "F",
                                };
                                let params = &s.tokens[i + 3..close];
                                let params = if params.last().is_some_and(|t| t.text == ",") {
                                    &params[..params.len() - 1]
                                } else {
                                    params
                                };
                                let ptr = if params.len() == 5 { 1 } else { 0 };
                                params.len() == ptr + 4
                                    && params[ptr].text == "*"
                                    && params[ptr + 2].text == "."
                                    && params[ptr + 3].text == expected
                                    && s.w(close + 1) == "{"
                            })
                    });
                if name.starts_with("Example") {
                    let Some(end) = s.body(close + 1) else {
                        continue;
                    };
                    let body = &s.text[s.tokens[close].span.end..s.tokens[end].span.start];
                    if !body.lines().any(|line| {
                        line.trim_start().strip_prefix("//").is_some_and(|c| {
                            let c = c.trim_start().to_ascii_lowercase();
                            c.starts_with("output:") || c.starts_with("unordered output:")
                        })
                    }) {
                        continue;
                    }
                }
                if valid {
                    s.add(res, i, s.body(close + 1), Test, "Go testing entry point");
                }
            }
            Java | Kotlin | Swift if w == "@" && test_context => {
                let mut j = i + 1;
                while s.w(j + 1) == "." {
                    j += 2;
                }
                let role = match s.w(j) {
                    "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory"
                    | "TestTemplate" => Test,
                    "Before" | "After" | "BeforeEach" | "AfterEach" | "BeforeAll" | "AfterAll" => {
                        Support
                    }
                    _ => continue,
                };
                let end = if lang == Kotlin {
                    s.kotlin_body(j + 1)
                } else {
                    s.body(j + 1)
                };
                s.add(res, i, end, role, "test framework annotation");
            }
            Csharp if w == "[" && test_context => {
                let Some(end) = s.close(i) else { continue };
                let role = s.tokens[i + 1..end].iter().find_map(|t| match t.text {
                    "Fact" | "Theory" | "Test" | "TestCase" | "TestMethod" | "DataTestMethod" => {
                        Some(Test)
                    }
                    "SetUp" | "TearDown" | "TestInitialize" | "TestCleanup" => Some(Support),
                    _ => None,
                });
                if let Some(role) = role {
                    s.add(res, i, s.body(end + 1), role, "test framework attribute");
                }
            }
            Swift | Java | ObjectiveC | Php | Hack
                if test_name(w)
                    && in_suite(i)
                    && (before == "func"
                        || before == "function"
                        || before == "void"
                        || lang == ObjectiveC && before == ")") =>
            {
                s.add(res, i, s.body(i + 1), Test, "test-case method");
            }
            Php if w == "function" && in_suite(i) => {
                let mut start = i;
                while start > 0
                    && matches!(
                        s.w(start - 1),
                        "public" | "static" | "final" | "protected" | "private"
                    )
                {
                    start -= 1;
                }
                let leading = &s.text[s.tokens[start.saturating_sub(1)]
                    .span
                    .end
                    .min(s.tokens[start].span.start)
                    ..s.tokens[start].span.start];
                if let Some(comment) = leading
                    .trim_end()
                    .strip_suffix("*/")
                    .and_then(|l| l.rsplit_once("/**"))
                    .map(|(_, c)| c)
                    && comment.split_whitespace().any(|x| x == "@test")
                {
                    s.add(res, start, s.body(i + 2), Test, "PHPUnit test annotation");
                }
            }
            Lua if test_file
                && matches!(w, "assert" | "asserteq" | "assertne")
                && s.w(i + 1) == "("
                && before != "." =>
            {
                s.add(res, i, s.close(i + 1), Test, "test-script assertion")
            }
            Dart if test_context
                && matches!(
                    w,
                    "it" | "test"
                        | "testWidgets"
                        | "beforeEach"
                        | "afterEach"
                        | "beforeAll"
                        | "afterAll"
                        | "setUp"
                        | "tearDown"
                )
                && before != "." =>
            {
                let mut j = i + 1;
                while s.w(j) == "."
                    && matches!(s.w(j + 1), "skip" | "only" | "todo" | "concurrent" | "each")
                {
                    j += 2;
                }
                if s.w(j) != "(" {
                    continue;
                }
                let setup = matches!(
                    w,
                    "beforeEach" | "afterEach" | "beforeAll" | "afterAll" | "setUp" | "tearDown"
                );
                if setup || s.string(j + 1) {
                    s.add(
                        res,
                        i,
                        s.close(j),
                        if setup { Support } else { Test },
                        "test framework registration",
                    );
                }
            }
            Powershell if w.eq_ignore_ascii_case("It") && test_file && s.string(i + 1) => {
                s.add(res, i, s.body(i + 2), Test, "Pester case")
            }
            Shell if w == "@" && s.w(i + 1) == "test" && s.string(i + 2) => {
                s.add(res, i, s.body(i + 3), Test, "Bats case")
            }
            Ruby if test_file && matches!(w, "it" | "specify" | "test") && s.string(i + 1) => {
                // Ruby block delimiters cannot use the brace pairing table.
                let end = (i + 2..s.tokens.len()).find(|&j| {
                    s.w(j) == "end"
                        && super::indentation(s.text, s.tokens[j].span.start)
                            <= super::indentation(s.text, t.span.start)
                });
                s.add(res, i, end, Test, "Ruby test block");
            }
            Ruby if test_file && w == "def" && s.w(i + 1).starts_with("test_") => {
                let end = (i + 2..s.tokens.len()).find(|&j| {
                    s.w(j) == "end"
                        && super::indentation(s.text, s.tokens[j].span.start)
                            <= super::indentation(s.text, t.span.start)
                });
                s.add(res, i, end, Test, "Ruby test method");
            }
            Lua if test_file && w == "it" && s.w(i + 1) == "(" && s.string(i + 2) => {
                s.add(res, i, s.close(i + 1), Test, "Busted case")
            }
            Hcl if path.ends_with(".tftest.hcl") && w == "run" && s.string(i + 1) => {
                s.add(res, i, s.body(i + 2), Test, "Terraform test run")
            }
            Perl if test_file && w == "subtest" && s.string(i + 1) => {
                s.add(res, i, s.body(i + 2), Test, "Test::More subtest")
            }
            _ => {}
        }
    }
}
