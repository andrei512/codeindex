use super::*;
use crate::domain::code::Token;

fn test_signature(name: &str, params: &[Token]) -> bool {
    let expected = [("Test", "T"), ("Benchmark", "B"), ("Fuzz", "F")]
        .into_iter()
        .find_map(|(prefix, ty)| {
            name.strip_prefix(prefix)
                .filter(|suffix| !suffix.starts_with(char::is_lowercase))
                .map(|_| ty)
        });
    let Some(expected) = expected else {
        return false;
    };
    let params = match params {
        [args @ .., comma] if comma.text == "," => args,
        args => args,
    };
    match params {
        [star, _, dot, ty] | [_, star, _, dot, ty] => {
            star.text == "*" && dot.text == "." && ty.text == expected
        }
        _ => false,
    }
}

fn example_output(body: &str) -> bool {
    body.lines().any(|line| {
        line.trim_start().strip_prefix("//").is_some_and(|comment| {
            let comment = comment.trim_start().to_ascii_lowercase();
            comment.starts_with("output:") || comment.starts_with("unordered output:")
        })
    })
}

fn entry_point(s: &Syntax, i: usize) -> Option<Option<usize>> {
    let close = s.close(i + 2)?;
    let name = s.w(i + 1);
    if let Some(suffix) = name.strip_prefix("Example") {
        let end = s.body(close + 1)?;
        let body = &s.text[s.tokens[close].span.end..s.tokens[end].span.start];
        let valid_signature = close == i + 3 && !suffix.starts_with(char::is_lowercase);
        return (valid_signature && example_output(body)).then_some(Some(end));
    }
    (s.w(close + 1) == "{" && test_signature(name, &s.tokens[i + 3..close]))
        .then(|| s.body(close + 1))
}

pub(super) fn go(path: &str, s: &Syntax, res: &mut Roles) {
    if !path.ends_with("_test.go") {
        return;
    }
    let functions = s
        .tokens
        .iter()
        .enumerate()
        .filter(|(i, token)| token.text == "func" && s.w(i + 2) == "(");
    for (i, _) in functions {
        if let Some(end) = entry_point(s, i) {
            s.add(res, i, end, Test.evidence("Go testing entry point"));
        }
    }
}
