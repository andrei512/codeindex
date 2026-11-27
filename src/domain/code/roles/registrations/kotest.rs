use crate::domain::code::roles::suite_bodies::BodySyntax;
pub(in crate::domain::code::roles) fn kotest_body(
    s: &BodySyntax,
    i: usize,
) -> Option<(usize, usize)> {
    let t = s.tokens;
    let pairs = s.pairs;
    let token = &t[i];
    let name = token.text;
    let next = i + 1;
    let at = |j| s.word(j);
    if !s.unqualified(i) {
        return None;
    }
    let string_case = name.starts_with('"');
    let j = body_start(s, next, string_case);
    let valid_body = at(j) == "{" && pairs[j] < t.len();
    if registration_name(name) && valid_body {
        return Some((j, pairs[j]));
    }
    None
}

fn registration_name(name: &str) -> bool {
    name.starts_with('"')
        || matches!(
            name,
            "describe"
                | "context"
                | "it"
                | "test"
                | "given"
                | "Given"
                | "When"
                | "Then"
                | "And"
                | "then"
                | "should"
                | "feature"
                | "scenario"
                | "beforeTest"
                | "afterTest"
                | "beforeEach"
                | "afterEach"
                | "beforeContainer"
                | "afterContainer"
                | "beforeSpec"
                | "afterSpec"
                | "beforeAny"
                | "afterAny"
        )
}

fn body_start(s: &BodySyntax, next: usize, string_case: bool) -> usize {
    match s.word(next) {
        "-" if string_case => next + 1,
        "(" if s.pairs[next] < s.tokens.len() => s.pairs[next] + 1,
        _ => next,
    }
}
