use super::*;

fn arguments(s: &Syntax, mut j: usize) -> Option<usize> {
    while s.w(j) == "." && matches!(s.w(j + 1), "skip" | "only" | "todo" | "concurrent" | "each") {
        j += 2;
    }
    (s.w(j) == "(").then_some(j)
}

pub(super) fn dart(s: &Syntax, res: &mut Roles) {
    if res.base.0 != Support && !has_test_framework(Dart, s) {
        return;
    }
    let registrations = s.tokens.iter().enumerate().filter_map(|(i, token)| {
        let before = i.checked_sub(1).map_or("", |p| s.w(p));
        let role = super::callback::case_role(token.text)?;
        (before != ".").then_some((i, role))
    });
    let calls = registrations.filter_map(|(i, role)| {
        let arguments = arguments(s, i + 1)?;
        (role == Support || s.string(arguments + 1)).then_some((i, arguments, role))
    });
    for (i, arguments, role) in calls {
        s.add(
            res,
            i,
            s.close(arguments),
            role.evidence("test framework registration"),
        );
    }
}
