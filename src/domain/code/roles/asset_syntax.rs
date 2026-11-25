use super::{Evidence, Kind, Language, Role, Roles, Syntax, lexer, script};
use std::{io, path::Path};

pub(super) fn html(text: &str, res: &mut Roles) {
    use super::super::markup::{HtmlPart, html_parts};
    let mut harness = false;
    let mut pos = 0;
    let mut line = 1;
    html_parts(text, |part| match part {
        HtmlPart::ScriptSrc(src)
            if src
                .split(['?', '#'])
                .next()
                .is_some_and(|p| p.rsplit('/').next() == Some("testharness.js")) =>
        {
            harness = true
        }
        HtmlPart::Inline { body, script: true } if harness => {
            line += text.as_bytes()[pos..body.start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            pos = body.start;
            let mut tokens = lexer::tokens(&text[body.clone()], Language::Js);
            for t in &mut tokens {
                t.span.start += body.start;
                t.span.end += body.start;
                t.line += line - 1;
            }
            let pairs = script::paired_for_score(&tokens);
            let s = Syntax {
                tokens: &tokens,
                pairs: &pairs,
                text,
            };
            for (i, t) in tokens.iter().enumerate() {
                if matches!(
                    t.text,
                    "test" | "async_test" | "promise_test" | "subsetTest"
                ) && s.w(i + 1) == "("
                    && (i == 0 || !matches!(s.w(i - 1), "." | "function"))
                {
                    s.add(
                        res,
                        i,
                        s.close(i + 1),
                        Role::Test,
                        "web-platform testharness registration",
                    );
                }
            }
        }
        _ => {}
    });
}

pub(super) fn collect(
    lang: Language,
    path: &str,
    s: &Syntax,
    res: &mut Roles,
    read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
) -> io::Result<()> {
    match lang {
        Language::Proto => {
            for i in 0..s.tokens.len() {
                if s.w(i) != "package" {
                    continue;
                }
                let mut j = i + 1;
                while !matches!(s.w(j), ";" | "") {
                    j += 1;
                }
                if s.tokens[i + 1..j]
                    .iter()
                    .any(|t| matches!(t.text, "test" | "testing" | "testdata"))
                    && path.split('/').any(|p| {
                        p.split(['-', '_'])
                            .any(|piece| matches!(piece, "test" | "testing" | "testdata"))
                    })
                {
                    res.base = Evidence(Role::Support, "test schema namespace and path");
                }
            }
        }
        Language::Nix => {
            for i in 0..s.tokens.len() {
                if (s.seq(i, &["nmt", ".", "script", "="]) || s.seq(i, &["testScript", "="]))
                    && (res.base.0 == Role::Support
                        || s.tokens.iter().any(|t| t.text == "runNixOSTest"))
                {
                    let mut j = if s.w(i) == "nmt" { i + 4 } else { i + 2 };
                    // The generic lexer splits Nix indented strings into tokens;
                    // locate the delimiter before looking for the terminating semicolon.
                    let start = s.tokens[i].span.start;
                    if let Some(value) = s.tokens.get(j) {
                        let rest = &s.text[value.span.start..];
                        if let Some(body) = rest.strip_prefix("''") {
                            let mut at = 0;
                            while let Some(next) = body[at..].find("''") {
                                let end = at + next + 2;
                                if body[end..].trim_start().starts_with(';') {
                                    let end = value.span.start + 2 + end;
                                    res.regions.push(super::Region {
                                        first: s.tokens[i].line,
                                        last: s.tokens[i].line
                                            + s.text[start..end]
                                                .bytes()
                                                .filter(|&b| b == b'\n')
                                                .count(),
                                        start,
                                        end,
                                        evidence: Evidence(Role::Test, "Nix test script"),
                                    });
                                    break;
                                }
                                at = end;
                            }
                        } else if value.kind == Kind::String {
                            s.add(res, i, Some(j), Role::Test, "Nix test script");
                        } else {
                            while j < s.tokens.len() && s.w(j) != ";" {
                                j = s.close(j).unwrap_or(j) + 1;
                            }
                            if j < s.tokens.len() {
                                s.add(res, i, Some(j), Role::Test, "Nix test script");
                            }
                        }
                    }
                }
            }
        }
        Language::Perl if res.base.0 == Role::Support => {
            let tap = s.tokens.windows(4).any(|w| {
                w[0].text == "Test"
                    && w[1].text == ":"
                    && w[2].text == ":"
                    && matches!(w[3].text, "More" | "Simple" | "V0")
            });
            for (i, t) in s.tokens.iter().enumerate() {
                if tap
                    && matches!(
                        t.text,
                        "ok" | "is"
                            | "isnt"
                            | "like"
                            | "unlike"
                            | "is_deeply"
                            | "cmp_ok"
                            | "pass"
                            | "fail"
                    )
                    && s.w(i + 1) == "("
                    && (i == 0 || !matches!(s.w(i - 1), ">" | "sub" | "::"))
                {
                    s.add(res, i, s.close(i + 1), Role::Test, "TAP assertion");
                }
            }
        }
        Language::Batchfile => {
            if batch_runner(s.text) {
                res.base = Evidence(Role::Support, "test runner launcher");
            } else {
                for line in s.text.lines() {
                    let line = line.trim().trim_start_matches('@');
                    if !line
                        .get(..5)
                        .is_some_and(|p| p.eq_ignore_ascii_case("call "))
                    {
                        continue;
                    }
                    let call = &line[5..];
                    let operand = if let Some(o) = call.strip_prefix('"') {
                        o.split('"').next().unwrap_or("")
                    } else {
                        call.split_whitespace().next().unwrap_or("")
                    };
                    let rest = operand.strip_prefix("%~dp0").or_else(|| {
                        let end = operand.strip_prefix('%')?.find('%')? + 2;
                        let var = &operand[1..end - 1];
                        s.text
                            .lines()
                            .any(|l| {
                                l.trim()
                                    .trim_matches('"')
                                    .eq_ignore_ascii_case(&format!("set {var}=%~dp0"))
                            })
                            .then_some(&operand[end..])
                    });
                    if let Some(rel) = rest
                        && (rel.ends_with(".bat") || rel.ends_with(".cmd"))
                    {
                        let mut parts = path.split('/').collect::<Vec<_>>();
                        parts.pop();
                        let mut valid = true;
                        for p in rel.split(['\\', '/']) {
                            match p {
                                "" | "." => {}
                                ".." => {
                                    if parts.pop().is_none() {
                                        valid = false;
                                        break;
                                    }
                                }
                                _ => parts.push(p),
                            }
                        }
                        if valid {
                            let candidate = parts.join("/");
                            if read(Path::new(&candidate))?.is_some_and(|code| batch_runner(&code))
                            {
                                res.base =
                                    Evidence(Role::Support, "delegates to a test runner launcher");
                                break;
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn batch_runner(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim().trim_start_matches('@').to_ascii_lowercase();
        if line.starts_with("rem ") || line.starts_with("::") || line.starts_with("echo ") {
            return false;
        }
        let args: Vec<_> = line.split_ascii_whitespace().collect();
        args.windows(2)
            .any(|w| w[0] == "-m" && matches!(w[1], "test" | "pytest" | "unittest"))
    })
}
