use super::*;
fn rust_test_attribute(s: &Syntax, first: usize) -> bool {
    let test = match s.w(first) {
        "test" => first,
        "tokio" | "async_std" | "actix_rt" if s.seq(first + 1, &[":", ":", "test"]) => first + 3,
        _ => return false,
    };
    matches!(s.w(test + 1), "(" | "]")
}

fn attribute_evidence(s: &Syntax, first: usize) -> Option<super::super::Evidence> {
    if rust_test_attribute(s, first) {
        return Some(Test.evidence("test attribute"));
    }
    s.seq(first, &["cfg", "(", "test", ")", "]"])
        .then(|| Support.evidence("test-only compilation"))
}

pub(super) fn rust(s: &Syntax, res: &mut Roles) {
    let attributes = s
        .tokens
        .iter()
        .enumerate()
        .filter(|(i, _)| s.seq(*i, &["#", "["]))
        .filter_map(|(i, _)| Some((i, s.close(i + 1)?, attribute_evidence(s, i + 2)?)));
    for (i, close, evidence) in attributes {
        s.add(res, i, s.body(close + 1), evidence);
    }
}
