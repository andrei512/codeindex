use super::*;
use crate::domain::code::roles::suite_bodies::BodySyntax;
pub(super) fn subtests(s: &Syntax, res: &mut Roles) {
    if res.base.0 == Support {
        for (i, token) in s.tokens.iter().enumerate() {
            if token.text == "subtest" {
                subtest(s, res, i);
            }
        }
    }
}

pub(in crate::domain::code::roles) fn subtest(s: &Syntax, res: &mut Roles, i: usize) {
    if s.string(i + 1) {
        s.add(res, i, s.body(i + 2), Test.evidence("Test::More subtest"));
    }
}

pub(in crate::domain::code::roles) fn perl_body(
    s: &BodySyntax,
    i: usize,
) -> Option<(usize, usize)> {
    let t = s.tokens;
    if t[i].text != "subtest" || qualified(s, i) {
        return None;
    }
    find_body(s, i + 1)
}

fn qualified(s: &BodySyntax, i: usize) -> bool {
    i > 0 && matches!(s.word(i - 1), ">" | "&" | "sub")
}

fn find_body(s: &BodySyntax, first: usize) -> Option<(usize, usize)> {
    let t = s.tokens;
    let pairs = s.pairs;
    let mut j = first;
    while j < t.len() {
        let prev = t[j - 1].text;
        match t[j].text {
            "{" => {
                return nested_subroutine(s, j, prev);
            }
            "(" if j == first => (),
            "(" | "[" => j = pairs[j].min(t.len()),
            ";" | "}" | ")" => return None,
            _ => (),
        }
        j += 1;
    }
    None
}

fn nested_subroutine(s: &BodySyntax, j: usize, previous: &str) -> Option<(usize, usize)> {
    let end = s.pairs[j];
    (end < s.tokens.len() && previous == "sub").then_some((j, end))
}
