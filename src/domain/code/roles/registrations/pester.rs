use super::*;
use crate::domain::code::roles::suite_bodies::BodySyntax;
fn hook(name: &str) -> bool {
    ["BeforeAll", "BeforeEach", "AfterEach", "AfterAll"]
        .iter()
        .any(|h| name.eq_ignore_ascii_case(h))
}
pub(super) fn collect(s: &Syntax, res: &mut Roles) {
    if res.base.0 != Support {
        return;
    }
    for (i, t) in s.tokens.iter().enumerate() {
        add_registration(s, res, i, t.text);
    }
}

fn add_registration(s: &Syntax, res: &mut Roles, i: usize, name: &str) {
    if hook(name) && s.w(i + 1) == "{" {
        s.add(
            res,
            i,
            s.close(i + 1),
            Support.evidence("Pester setup or teardown"),
        );
    }
    if name.eq_ignore_ascii_case("It") && s.string(i + 1) {
        s.add(res, i, s.body(i + 2), Test.evidence("Pester case"));
    }
}
pub(in crate::domain::code::roles) fn pester_body(
    s: &BodySyntax,
    i: usize,
) -> Option<(usize, usize)> {
    if !registration_name(s.word(i)) || !statement_head(s, i) {
        return None;
    }
    find_body(s, i + 1)
}

fn registration_name(name: &str) -> bool {
    hook(name)
        || ["Describe", "Context", "It", "BeforeDiscovery"]
            .iter()
            .any(|expected| name.eq_ignore_ascii_case(expected))
}

fn statement_head(s: &BodySyntax, i: usize) -> bool {
    let Some(prior) = i.checked_sub(1) else {
        return true;
    };
    let previous = &s.tokens[prior];
    previous.line != s.tokens[i].line || matches!(previous.text, ";" | "{" | "}")
}

fn find_body(s: &BodySyntax, mut j: usize) -> Option<(usize, usize)> {
    let t = s.tokens;
    let pairs = s.pairs;
    while j < t.len() {
        match (t[j].text, t[j - 1].text) {
            ("{", "@") | ("(" | "[", _) => j = pairs[j].min(t.len()),
            ("{", _) => return (pairs[j] < t.len()).then_some((j, pairs[j])),
            (";" | "}" | ")", _) => return None,
            (_, previous) if line_break(s, j, previous) => return None,
            _ => (),
        }
        j += 1;
    }
    None
}

fn line_break(s: &BodySyntax, j: usize, previous: &str) -> bool {
    previous != "`" && s.tokens[j].line != s.tokens[j - 1].line
}
