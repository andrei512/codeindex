use super::*;
fn lua_reason(s: &Syntax, i: usize) -> Option<&'static str> {
    let unqualified = i == 0 || s.w(i - 1) != ".";
    match s.w(i) {
        "assert" | "asserteq" | "assertne" if unqualified => Some("test-script assertion"),
        "it" if s.string(i + 2) => Some("Busted case"),
        _ => None,
    }
}

pub(super) fn lua(s: &Syntax, res: &mut Roles) {
    if res.base.0 != Support {
        return;
    }
    for (i, _) in s
        .tokens
        .iter()
        .enumerate()
        .filter(|(i, _)| s.w(i + 1) == "(")
    {
        let Some(reason) = lua_reason(s, i) else {
            continue;
        };
        s.add(res, i, s.close(i + 1), Test.evidence(reason));
    }
}
pub(super) fn shell(s: &Syntax, res: &mut Roles) {
    for (i, t) in s.tokens.iter().enumerate() {
        let w = t.text;
        if w == "@" && bats_case(s, i + 1) {
            s.add(res, i, s.body(i + 3), Test.evidence("Bats case"))
        }
    }
}
fn bats_case(s: &Syntax, test: usize) -> bool {
    s.w(test) == "test" && s.string(test + 1)
}

pub(super) fn hcl(path: &str, s: &Syntax, res: &mut Roles) {
    if !path.ends_with(".tftest.hcl") {
        return;
    }
    for (i, t) in s.tokens.iter().enumerate() {
        let w = t.text;
        if w == "run" && s.string(i + 1) {
            s.add(res, i, s.body(i + 2), Test.evidence("Terraform test run"))
        }
    }
}
