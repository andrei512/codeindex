//! Dart and PHP registration callbacks enclosed by a call argument list.
use super::Role;
use crate::domain::code::roles::suite_bodies::BodySyntax;
pub(super) fn case_role(name: &str) -> Option<Role> {
    match name {
        "test" | "it" | "testWidgets" => Some(Role::Test),
        "beforeEach" | "afterEach" | "beforeAll" | "afterAll" | "setUp" | "tearDown"
        | "setUpAll" | "tearDownAll" => Some(Role::Support),
        _ => None,
    }
}

pub(in crate::domain::code::roles) fn callback_body(
    s: &BodySyntax,
    i: usize,
) -> Option<(usize, usize)> {
    let next = i + 1;
    if !s.unqualified(i) || !callback_name(s.word(i)) {
        return None;
    }
    let end = s.pairs.get(next).copied()?;
    if s.word(next) != "(" || end >= s.tokens.len() {
        return None;
    }
    let mut j = next + 1;
    while j < end {
        if let Some(open) = body_after_parameters(s, j, end) {
            return Some((open, s.pairs[open]));
        }
        j = next_argument_token(s, j, end);
    }
    None
}

fn callback_name(name: &str) -> bool {
    matches!(name, "group" | "describe" | "context") || case_role(name).is_some()
}

fn body_after_parameters(s: &BodySyntax, j: usize, end: usize) -> Option<usize> {
    if s.word(j) != "(" || s.pairs[j] >= end {
        return None;
    }
    let first = s.pairs[j] + 1;
    let open = first + usize::from(matches!(s.word(first), "async" | "sync"));
    (s.word(open) == "{" && s.pairs[open] < end).then_some(open)
}

fn next_argument_token(s: &BodySyntax, j: usize, end: usize) -> usize {
    if matches!(s.word(j), "(" | "[" | "{") && s.pairs[j] < end {
        s.pairs[j] + 1
    } else {
        j + 1
    }
}
