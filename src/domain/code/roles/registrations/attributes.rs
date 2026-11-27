use super::*;
pub(super) fn annotations(lang: Language, s: &Syntax, res: &mut Roles) {
    if res.base.0 != Support && !has_test_framework(lang, s) {
        return;
    }
    let sites = s
        .tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.text == "@");
    let annotations = sites.filter_map(|(at, _)| {
        let name = super::super::class_roles::annotation_end(s, at + 1);
        Some((at, name, annotation_role(s.w(name))?))
    });
    let body = if lang == Kotlin {
        Syntax::kotlin_body
    } else {
        Syntax::body
    };
    for (at, name, role) in annotations {
        let end = body(s, name + 1);
        s.add(res, at, end, role.evidence("test framework annotation"));
    }
}

fn annotation_role(name: &str) -> Option<Role> {
    match name {
        "Test" | "ParameterizedTest" | "RepeatedTest" | "TestFactory" | "TestTemplate" => {
            Some(Test)
        }
        "Before" | "After" | "BeforeEach" | "AfterEach" | "BeforeAll" | "AfterAll" => Some(Support),
        _ => None,
    }
}

pub(super) fn csharp(s: &Syntax, res: &mut Roles) {
    if res.base.0 != Support && !has_test_framework(Csharp, s) {
        return;
    }
    let brackets = s
        .tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.text == "[");
    let attributes = brackets.filter_map(|(i, _)| {
        let end = s.close(i)?;
        let role = s.tokens[i + 1..end]
            .iter()
            .find_map(|token| attribute_role(token.text))?;
        Some((i, end, role))
    });
    for (i, end, role) in attributes {
        s.add(
            res,
            i,
            s.body(end + 1),
            role.evidence("test framework attribute"),
        );
    }
}

fn attribute_role(spelling: &str) -> Option<Role> {
    match spelling {
        "Fact" | "Theory" | "Test" | "TestCase" | "TestMethod" | "DataTestMethod" => Some(Test),
        "SetUp" | "TearDown" | "TestInitialize" | "TestCleanup" => Some(Support),
        _ => None,
    }
}

pub(super) fn suite_methods(lang: Language, s: &Syntax, res: &mut Roles) {
    let suites = super::super::suites::suites(lang, s, res.base.0 == Support);
    if suites.is_empty() {
        return;
    }
    let methods = s
        .tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| test_name(token.text) || token.text == "function")
        .filter(|(i, _)| suites.iter().any(|suite| suite.contains(*i)));
    for (i, _) in methods {
        add_suite_method(lang, s, res, i);
    }
}

fn add_suite_method(lang: Language, s: &Syntax, res: &mut Roles, i: usize) {
    let name = s.w(i);
    let previous = i.checked_sub(1).map_or("", |prior| s.w(prior));
    if test_name(name) && method_introducer(lang, previous) {
        s.add(res, i, s.body(i + 1), Test.evidence("test-case method"));
    }
    if lang != Php || name != "function" {
        return;
    }
    if let Some(start) = phpunit_annotation(s, i) {
        s.add(
            res,
            start,
            s.body(i + 2),
            Test.evidence("PHPUnit test annotation"),
        );
    }
}

fn method_introducer(lang: Language, previous: &str) -> bool {
    matches!(previous, "func" | "function" | "void") || lang == ObjectiveC && previous == ")"
}

fn phpunit_annotation(s: &Syntax, function: usize) -> Option<usize> {
    let mut start = function;
    while start > 0
        && matches!(
            s.w(start - 1),
            "public" | "static" | "final" | "protected" | "private"
        )
    {
        start -= 1;
    }
    let end = s.tokens[start].span.start;
    let begin = s.tokens[start.saturating_sub(1)].span.end.min(end);
    let (_, comment) = s.text[begin..end]
        .trim_end()
        .strip_suffix("*/")?
        .rsplit_once("/**")?;
    comment
        .split_whitespace()
        .any(|word| word == "@test")
        .then_some(start)
}
