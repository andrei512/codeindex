//! Test methods belong to their declaring type. Resolve local inheritance
//! before considering conventions; nested and anonymous types own their bodies.
use super::{Kind, Language, Role, Roles, Syntax, lexer, script};
use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
enum Base {
    Framework,
    Named(String),
}

struct Type<'a> {
    keyword: usize,
    open: usize,
    close: usize,
    name: &'a str,
    base: Option<Base>,
    extension: bool,
}

fn is_word(s: &Syntax, i: usize) -> bool {
    s.tokens.get(i).is_some_and(|t| t.kind == Kind::Word)
}

fn framework_base(lang: Language, name: &str, tokens: &[super::Token]) -> bool {
    let has = |seq: &[&str]| {
        tokens
            .windows(seq.len())
            .any(|w| w.iter().zip(seq).all(|(a, b)| a.text == *b))
    };
    match (lang, name) {
        (Language::Java, "TestCase") => {
            has(&["junit", ".", "framework", ".", "TestCase"])
                || has(&["import", "junit", ".", "framework", ".", "*"])
        }
        (Language::Swift, "XCTestCase") => {
            has(&["import", "XCTest"]) || has(&["XCTest", ".", "XCTestCase"])
        }
        (Language::Hack, "HackTest") => {
            has(&["Facebook", "\\", "HackTest"]) || has(&["MiniTest", "\\"])
        }
        _ => false,
    }
}

fn base(lang: Language, s: &Syntax, mut pos: usize) -> Option<Base> {
    if s.w(pos) == "\\" {
        pos += 1;
    }
    if !is_word(s, pos) {
        return None;
    }
    while matches!(s.w(pos + 1), "." | "\\") && is_word(s, pos + 2) {
        pos += 2;
    }
    let name = s.w(pos);
    Some(if framework_base(lang, name, s.tokens) {
        Base::Framework
    } else {
        Base::Named(name.into())
    })
}

fn types<'a>(lang: Language, s: &'a Syntax) -> Vec<Type<'a>> {
    let mut types = Vec::new();
    for (i, t) in s.tokens.iter().enumerate() {
        if lang == Language::Java && t.text == "new" {
            let mut open = i + 1;
            while open < s.tokens.len()
                && (is_word(s, open) || matches!(s.w(open), "." | "<" | ">" | "," | "?"))
            {
                open += 1;
            }
            if s.w(open) == "("
                && let Some(close) = s.close(open)
                && s.w(close + 1) == "{"
                && let Some(end) = s.close(close + 1)
            {
                types.push(Type {
                    keyword: i,
                    open: close + 1,
                    close: end,
                    name: "",
                    base: None,
                    extension: false,
                });
            }
            continue;
        }
        let extension = lang == Language::Swift && t.text == "extension";
        if !extension
            && !matches!(
                t.text,
                "class" | "interface" | "enum" | "struct" | "trait" | "actor" | "record"
            )
        {
            continue;
        }
        if !is_word(s, i + 1) || matches!(s.w(i + 1), "func" | "var" | "let" | "function") {
            continue;
        }
        let mut open = i + 2;
        let mut parent = None;
        let mut generics = 0usize;
        while open < s.tokens.len() && !matches!(s.w(open), "{" | "}" | ";" | "class") {
            match s.w(open) {
                "<" => generics += 1,
                ">" => generics = generics.saturating_sub(1),
                "extends" | ":" if parent.is_none() && generics == 0 => {
                    parent = base(lang, s, open + 1)
                }
                "(" if s.close(open).is_some() => open = s.close(open).unwrap(),
                _ => (),
            }
            open += 1;
        }
        if s.w(open) != "{" {
            continue;
        }
        let Some(close) = s.close(open) else {
            continue;
        };
        if lang == Language::Hack && t.text == "trait" {
            let mut j = open + 1;
            while j < close {
                if s.seq(j, &["require", "extends"]) {
                    parent = base(lang, s, j + 2);
                    break;
                }
                j = s.close(j).unwrap_or(j) + 1;
            }
        }
        types.push(Type {
            keyword: i,
            open,
            close,
            name: s.w(i + 1),
            base: parent,
            extension,
        });
    }
    types
}

type Parents = HashMap<String, Option<Base>>;
fn parents(types: &[Type]) -> Parents {
    let mut out = Parents::new();
    for t in types.iter().filter(|t| !t.extension && !t.name.is_empty()) {
        out.entry(t.name.to_owned())
            .and_modify(|v| *v = None)
            .or_insert_with(|| t.base.clone());
    }
    out
}

#[derive(Default)]
pub(crate) struct Classes {
    siblings: HashMap<PathBuf, Parents>,
}

impl Classes {
    fn suite(
        &mut self,
        lang: Language,
        path: &str,
        start: Option<&Base>,
        local: &Parents,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<bool> {
        let mut next = start.cloned();
        let mut seen = HashSet::new();
        while let Some(base) = next {
            let Base::Named(name) = base else {
                return Ok(true);
            };
            if !seen.insert(name.clone()) {
                break;
            }
            if let Some(b) = local.get(&name) {
                next = b.clone();
                continue;
            }
            let ext = match lang {
                Language::Swift => "swift",
                Language::Java => "java",
                Language::Hack => "hack",
                _ => unreachable!(),
            };
            let key = Path::new(path).with_file_name(format!("{name}.{ext}"));
            if !self.siblings.contains_key(&key) {
                let summary = if let Some(text) = read(&key)? {
                    let tokens = lexer::tokens(&text, lang);
                    let pairs = script::paired_for_score(&tokens);
                    let syntax = Syntax {
                        tokens: &tokens,
                        pairs: &pairs,
                        text: &text,
                    };
                    parents(&types(lang, &syntax))
                } else {
                    Parents::new()
                };
                self.siblings.insert(key.clone(), summary);
            }
            next = self.siblings[&key].get(&name).cloned().flatten();
        }
        Ok(false)
    }

    pub(super) fn collect(
        &mut self,
        lang: Language,
        path: &str,
        s: &Syntax,
        res: &mut Roles,
        mut read: impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<()> {
        let declarations = types(lang, s);
        let local = parents(&declarations);
        let mut suite_for = HashMap::new();
        for t in &declarations {
            let extension = Base::Named(t.name.into());
            let base = if t.extension {
                Some(&extension)
            } else {
                t.base.as_ref()
            };
            suite_for.insert(t.open, self.suite(lang, path, base, &local, &mut read)?);
        }
        let annotations = Annotations::new(lang, s);
        if lang == Language::Swift {
            // Swift Testing also registers free functions.
            members(lang, s, 0, s.tokens.len(), false, &annotations, res);
        }
        for t in &declarations {
            let nested_in_test = res
                .regions
                .iter()
                .rev()
                .find(|r| {
                    r.start <= s.tokens[t.keyword].span.start && s.tokens[t.close].span.end <= r.end
                })
                .is_some_and(|r| r.evidence.0 == Role::Test);
            if nested_in_test {
                s.add(
                    res,
                    t.keyword,
                    Some(t.close),
                    Role::Support,
                    "type declared inside a test",
                );
            }
            members(
                lang,
                s,
                t.open + 1,
                t.close,
                suite_for[&t.open],
                &annotations,
                res,
            );
        }
        Ok(())
    }
}

struct Annotations {
    names: HashSet<String>,
    wildcards: Vec<String>,
    swift_testing: bool,
}
impl Annotations {
    fn new(lang: Language, s: &Syntax) -> Self {
        let mut out = Self {
            names: HashSet::new(),
            wildcards: Vec::new(),
            swift_testing: false,
        };
        for (i, t) in s.tokens.iter().enumerate() {
            if t.text != "import" {
                continue;
            }
            if lang == Language::Swift && s.w(i + 1) == "Testing" {
                out.swift_testing = true;
            }
            if lang != Language::Java {
                continue;
            }
            let mut j = i + 1;
            let mut name = s.w(j).to_owned();
            while s.w(j + 1) == "." {
                j += 2;
                name.push('.');
                name.push_str(s.w(j));
            }
            if let Some(package) = name.strip_suffix(".*") {
                out.wildcards.push(package.into());
            } else {
                out.names.insert(name);
            }
        }
        out
    }
    fn role(&self, lang: Language, name: &str) -> Option<Role> {
        let simple = name.rsplit('.').next()?;
        let role = match simple {
            "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate" => {
                Role::Test
            }
            "Before" | "After" | "BeforeEach" | "AfterEach" | "BeforeAll" | "AfterAll" => {
                Role::Support
            }
            _ => return None,
        };
        if lang == Language::Swift {
            return (simple == "Test"
                && (name == "Testing.Test" || name == "Test" && self.swift_testing))
                .then_some(role);
        }
        if lang != Language::Java {
            return None;
        }
        let valid = |p: &str| {
            matches!(
                p,
                "org.junit"
                    | "org.junit.jupiter.api"
                    | "org.junit.jupiter.params"
                    | "org.testng.annotations"
            )
        };
        if let Some((p, _)) = name.rsplit_once('.') {
            return valid(p).then_some(role);
        }
        (self.names.iter().any(|n| {
            n.rsplit_once('.')
                .is_some_and(|(p, v)| v == name && valid(p))
        }) || self.wildcards.iter().any(|p| valid(p)))
        .then_some(role)
    }
}

fn members(
    lang: Language,
    s: &Syntax,
    mut i: usize,
    end: usize,
    suite: bool,
    anno: &Annotations,
    res: &mut Roles,
) {
    let mut annotation = None;
    let mut start = i;
    while i < end {
        if s.w(i) == "@" {
            let p = i;
            i += 1;
            let mut name = s.w(i).to_owned();
            while s.w(i + 1) == "." {
                i += 2;
                name.push('.');
                name.push_str(s.w(i));
            }
            i += 1;
            if s.w(i) == "("
                && let Some(c) = s.close(i)
            {
                i = c + 1;
            }
            if let Some(role) = anno.role(lang, &name) {
                annotation = Some((p, role));
            }
            continue;
        }
        let declared = match lang {
            Language::Java => {
                s.w(i + 1) == "("
                    && is_word(s, i)
                    && (annotation.is_some() || i > 0 && s.w(i - 1) == "void")
            }
            Language::Swift => i > 0 && s.w(i - 1) == "func" && s.w(i + 1) == "(",
            Language::Hack => i > 0 && s.w(i - 1) == "function",
            _ => false,
        };
        let params = if lang == Language::Hack && declared && s.w(i + 1) == "<" {
            script::angle_end(s.tokens, i + 1).map(|close| close + 1)
        } else {
            Some(i + 1)
        };
        if declared
            && let Some(open) = params
            && s.w(open) == "("
            && let Some(close) = s.close(open)
        {
            let modifiers = &s.tokens[start..i];
            let conventional = suite
                && s.w(i).starts_with("test")
                && match lang {
                    Language::Java => {
                        s.w(i - 1) == "void"
                            && close == i + 2
                            && modifiers.iter().any(|t| t.text == "public")
                    }
                    Language::Swift => {
                        close == i + 2
                            && !modifiers
                                .iter()
                                .any(|t| matches!(t.text, "static" | "private" | "fileprivate"))
                            && !s.seq(close + 1, &["-", ">"])
                    }
                    Language::Hack => !modifiers
                        .iter()
                        .any(|t| matches!(t.text, "protected" | "private" | "static")),
                    _ => false,
                };
            if let Some((pos, role)) = annotation
                .take()
                .or_else(|| conventional.then_some((i, Role::Test)))
                && let Some(last) = s.body(close + 1)
            {
                s.add(
                    res,
                    pos,
                    Some(last),
                    role,
                    if conventional {
                        "test-case method"
                    } else {
                        "test framework annotation"
                    },
                );
            }
        }
        if matches!(s.w(i), "{" | ";") {
            annotation = None;
            i = s.close(i).unwrap_or(i) + 1;
            start = i;
        } else {
            i = s.close(i).unwrap_or(i) + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(
        lang: Language,
        path: &str,
        source: &str,
        siblings: &[(&str, &str)],
        expected: &[(&str, Role)],
    ) {
        use super::super::{Evidence, Role::*};
        let tokens = lexer::tokens(source, lang);
        let pairs = script::paired_for_score(&tokens);
        let syntax = Syntax {
            tokens: &tokens,
            pairs: &pairs,
            text: source,
        };
        let mut output = Roles {
            base: Evidence(Support, "fixture"),
            regions: Vec::new(),
        };
        Classes::default()
            .collect(lang, path, &syntax, &mut output, |p| {
                Ok(siblings
                    .iter()
                    .find(|(name, _)| p == Path::new(name))
                    .map(|(_, text)| text.to_string()))
            })
            .unwrap();
        output
            .regions
            .sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
        for &(needle, role) in expected {
            let pos = source.find(needle).unwrap();
            assert_eq!(
                output
                    .regions
                    .iter()
                    .rev()
                    .find(|r| r.start <= pos && pos < r.end)
                    .map_or(output.base.0, |r| r.evidence.0),
                role,
                "{needle}"
            );
        }
    }
    #[test]
    fn junit_uses_framework_and_anonymous_class_owns_its_methods() {
        use Role::*;
        check(
            Language::Java,
            "Cases.java",
            "import junit.framework.TestCase; import org.junit.Test; class Cases extends TestCase { public void testlower() { run(); Object o = new Base() { public void testFake() { inside(); } }; } public void testWrong(int x) { wrong(); } } class Plain { @Test public void registered() { yes(); } public void testSerialization() { dormant(); } }",
            &[],
            &[
                ("run()", Test),
                ("inside()", Support),
                ("wrong()", Support),
                ("yes()", Test),
                ("dormant()", Support),
            ],
        );
        check(
            Language::Java,
            "Fake.java",
            "import custom.Test; class Fake { @Test public void labeled() { nope(); } }",
            &[],
            &[("nope()", Support)],
        );
    }
    #[test]
    fn swift_extension_inherits_proven_sibling_superclass() {
        use Role::*;
        let code = "import XCTest\nclass Suite: Base {}\nextension Suite { func testlower() throws { pass() } func testArg(x: Int) { ignored() } }\nextension Production { func testMode() { ordinary() } }";
        check(
            Language::Swift,
            "Cases/File.swift",
            code,
            &[(
                "Cases/Base.swift",
                "import XCTest\nclass Base: XCTestCase {}",
            )],
            &[
                ("pass()", Test),
                ("ignored()", Support),
                ("ordinary()", Support),
            ],
        );
        check(
            Language::Swift,
            "Cases/File.swift",
            code,
            &[("Cases/Base.swift", "class Other {}")],
            &[("pass()", Support)],
        );
    }
    #[test]
    fn hack_trait_requires_a_test_case() {
        use Role::*;
        let code = "trait Helpers { require extends Base; <<DataProvider('cases')>> public function testInput<T as num>(T $x): void { run(); } } trait Normal { public function testApi(): void { normal(); } }";
        check(
            Language::Hack,
            "tests/Something.hack",
            code,
            &[(
                "tests/Base.hack",
                "abstract class Base extends \\Facebook\\HackTest\\HackTest {}",
            )],
            &[("run()", Test), ("normal()", Support)],
        );
    }
}
