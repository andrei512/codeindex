use super::{Evidence, Kind, Language, Role, Roles, Syntax, lexer, script};
use std::{io, path::Path};

/// A script registers web-platform tests only after a harness script was seen.
#[derive(Default)]
pub(in crate::domain::code) struct WebHarness(bool);

impl WebHarness {
    pub(in crate::domain::code) fn observe<'a>(
        &mut self,
        part: &'a super::super::markup::HtmlPart,
    ) -> Option<&'a std::ops::Range<usize>> {
        use super::super::markup::HtmlPart;
        match part {
            HtmlPart::InlineScript(body) if self.0 => Some(body),
            HtmlPart::ScriptSrc(src) => {
                self.0 |= src
                    .split(['?', '#'])
                    .next()
                    .and_then(|p| p.rsplit('/').next())
                    == Some("testharness.js");
                None
            }
            _ => None,
        }
    }
}

pub(in crate::domain::code) fn harness_registrations(
    source: &str,
    tokens: &[super::Token],
    pairs: &[usize],
    mut emit: impl FnMut(&super::Token, &super::Token),
) {
    let syntax = Syntax {
        tokens,
        pairs,
        text: source,
    };
    super::javascript::web_harness_calls(&syntax, |i, close| {
        emit(&tokens[i], &tokens[close]);
    });
}

pub(super) fn html(text: &str, res: &mut Roles) {
    use super::super::markup::html_parts;
    let mut harness = WebHarness::default();
    let mut pos = 0;
    let mut line = 1;
    html_parts(text, |part| {
        let Some(body) = harness.observe(&part) else {
            return;
        };
        line += text.as_bytes()[pos..body.start]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        pos = body.start;
        let tokens = lexer::tokens(&text[body.clone()], Language::Js);
        let pairs = script::paired_for_score(&tokens);
        harness_registrations(&text[body.clone()], &tokens, &pairs, |first, last| {
            let region = Role::Test
                .evidence("web-platform testharness registration")
                .between(first, last);
            res.regions.push(region.in_host(body.start, line - 1));
        });
    });
}
pub(super) fn schema(path: &str, s: &Syntax, res: &mut Roles) {
    let test_word = |w: &str| matches!(w, "test" | "testing" | "testdata");
    if !path.split(['/', '-', '_']).any(test_word) {
        return;
    }
    let mut package = false;
    let namespace = s.tokens.iter().any(|token| {
        match token.text {
            "package" => package = true,
            ";" | "" => package = false,
            word => return package && test_word(word),
        }
        false
    });
    if namespace {
        res.base = Evidence(Role::Support, "test schema namespace and path");
    }
}

pub(super) fn nix(s: &Syntax, res: &mut Roles) {
    if res.base.0 != Role::Support && !s.tokens.iter().any(|token| token.text == "runNixOSTest") {
        return;
    }
    for i in 0..s.tokens.len() {
        if let Some(value) = nix_script_start(s, i) {
            nix_script(s, res, i, value);
        }
    }
}

fn nix_script_start(s: &Syntax, i: usize) -> Option<usize> {
    match s.w(i) {
        "testScript" if s.w(i + 1) == "=" => Some(i + 2),
        "nmt" if s.seq(i + 1, &[".", "script", "="]) => Some(i + 4),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tap {
    Simple,
    More,
    Test2,
}

impl Tap {
    fn module(s: &Syntax, i: usize) -> Option<(Self, usize)> {
        if i >= 2 && s.seq(i - 2, &[":", ":"]) {
            return None;
        }
        let module = match (s.w(i), s.w(i + 1), s.w(i + 2), s.w(i + 3)) {
            ("Test", ":", ":", "Simple") => Self::Simple,
            ("Test", ":", ":", "More") => Self::More,
            ("Test2", ":", ":", "V0") => Self::Test2,
            _ => return None,
        };
        Some((module, i + 4))
    }

    fn import(s: &Syntax, i: usize, names: &mut std::collections::HashSet<&'static str>) {
        let Some((module, after)) = Self::module(s, i + 1) else {
            return;
        };
        if matches!((s.w(after), s.w(after + 1)), (":", ":") | ("(", ")")) {
            return;
        }
        let selection = (after..s.tokens.len())
            .take_while(|&j| s.w(j) != ";")
            .find(|&j| s.seq(j, &["import", "=", ">"]));
        let specs = match selection {
            Some(at) => match import_specs(s, at + 3) {
                Some(specs) => specs,
                None => return,
            },
            None => Vec::new(),
        };
        names.extend(
            module
                .names()
                .iter()
                .copied()
                .filter(|name| selected_export(name, &specs)),
        );
    }

    fn supports_call(s: &Syntax, call: usize, imported: &std::collections::HashSet<&str>) -> bool {
        let name = s.w(call);
        let previous = call.checked_sub(1).map(|i| s.w(i));
        match previous {
            Some(">" | "sub") => false,
            Some(":") if call >= 2 && s.w(call - 2) == ":" => call
                .checked_sub(6)
                .and_then(|at| Self::module(s, at))
                .is_some_and(|(module, _)| module.names().contains(&name)),
            _ => imported.contains(name),
        }
    }

    fn names(self) -> &'static [&'static str] {
        const NAMES: &[&str] = &[
            "ok",
            "is",
            "isnt",
            "like",
            "unlike",
            "cmp_ok",
            "pass",
            "fail",
            "is_deeply",
        ];
        match self {
            Self::Simple => &NAMES[..1],
            Self::Test2 => &NAMES[..NAMES.len() - 1],
            Self::More => NAMES,
        }
    }
}

/// Literal Test::More import lists follow Exporter's ordered selection rules.
fn import_specs<'a>(s: &'a Syntax, open: usize) -> Option<Vec<&'a str>> {
    let end = s.close(open).filter(|_| s.w(open) == "[")?;
    let mut specs = Vec::new();
    for arg in super::arguments(s, open + 1, end) {
        let tokens = &s.tokens[arg];
        let text = &s.text[tokens.first()?.span.start..tokens.last()?.span.end];
        append_import_spec(text, &mut specs)?;
    }
    Some(specs)
}

fn append_import_spec<'a>(text: &'a str, specs: &mut Vec<&'a str>) -> Option<()> {
    if let Some(words) = text.strip_prefix("qw") {
        let mut chars = words.trim_start().chars();
        let open = chars.next()?;
        let close = match open {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '<' => '>',
            c => c,
        };
        specs.extend(chars.as_str().strip_suffix(close)?.split_whitespace());
    } else {
        let quote = text.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
        specs.push(text.strip_prefix(quote)?.strip_suffix(quote)?);
    }
    Some(())
}

fn selected_export(name: &str, specs: &[&str]) -> bool {
    // Later selectors override earlier choices. A regex cannot prove membership.
    specs
        .iter()
        .rev()
        .find_map(|spec| {
            let candidate = spec.strip_prefix('!').unwrap_or(spec);
            if candidate == name || candidate == ":DEFAULT" {
                Some(!spec.starts_with('!'))
            } else {
                candidate.starts_with('/').then_some(false)
            }
        })
        .unwrap_or_else(|| specs.first().is_none_or(|spec| spec.starts_with('!')))
}

pub(super) fn perl(s: &Syntax, res: &mut Roles) {
    if res.base.0 != Role::Support {
        return;
    }
    let mut imported = std::collections::HashSet::new();
    for i in 0..s.tokens.len() {
        observe_perl(s, res, i, &mut imported);
    }
}

fn observe_perl(
    s: &Syntax,
    res: &mut Roles,
    i: usize,
    imported: &mut std::collections::HashSet<&'static str>,
) {
    match s.w(i) {
        "use" => Tap::import(s, i, imported),
        "subtest" => super::registrations::subtest(s, res, i),
        _ if s.w(i + 1) == "(" && Tap::supports_call(s, i, imported) => {
            s.add(res, i, s.close(i + 1), Role::Test.evidence("TAP assertion"));
        }
        _ => {}
    }
}

pub(super) fn batch(
    path: &str,
    s: &Syntax,
    res: &mut Roles,
    read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
) -> io::Result<()> {
    if batch_runner(s.text) {
        res.base = Evidence(Role::Support, "test runner launcher");
        return Ok(());
    }
    let directories = std::cell::OnceCell::new();
    let targets = s.text.lines().filter_map(|line| {
        batch_target(path, line, || {
            directories.get_or_init(|| batch_directories(s.text))
        })
    });
    for candidate in targets {
        if read(Path::new(&candidate))?.is_some_and(|code| batch_runner(&code)) {
            res.base = Evidence(Role::Support, "delegates to a test runner launcher");
            break;
        }
    }
    Ok(())
}
fn batch_runner(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim().trim_start_matches('@').to_ascii_lowercase();
        if batch_inert(&line) {
            return false;
        }
        let words = line.split_ascii_whitespace();
        let mut previous = "";
        let module = words.clone().any(|arg| {
            let follows_option = previous == "-m";
            previous = arg;
            follows_option && matches!(arg, "test" | "pytest" | "unittest")
        });
        module && !git_owns_option(&line, words)
    })
}

fn batch_inert(line: &str) -> bool {
    ["rem ", "::", "echo "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

fn git_owns_option(line: &str, mut words: std::str::SplitAsciiWhitespace<'_>) -> bool {
    // These literal Git builtins own -m themselves. A command chain, expansion
    // or external Git subcommand can still dispatch a separate test launcher.
    matches!(words.next(), Some("git" | "git.exe"))
        && matches!(words.next(), Some("branch" | "commit" | "tag" | "-m"))
        && !line.contains(['&', '|', '<', '>', '(', ')', '^', '%', '!'])
}

fn nix_script(s: &Syntax, res: &mut Roles, i: usize, mut value: usize) {
    let Some(token) = s.tokens.get(value) else {
        return;
    };
    if s.text[token.span.start..].starts_with("''") {
        nix_indented_script(s, res, i, value);
        return;
    }
    if token.kind != Kind::String {
        value = nix_expression_end(s, value);
    }
    if value < s.tokens.len() {
        s.add(res, i, Some(value), Role::Test.evidence("Nix test script"));
    }
}

fn nix_indented_script(s: &Syntax, res: &mut Roles, i: usize, value: usize) {
    let value_start = s.tokens[value].span.start;
    let body = &s.text[value_start + 2..];
    let Some(end) = body
        .match_indices("''")
        .map(|(at, _)| at + 2)
        .find(|&end| body[end..].trim_start().starts_with(';'))
    else {
        return;
    };
    let start = s.tokens[i].span.start;
    let end = value_start + 2 + end;
    res.regions.push(super::Region {
        first: s.tokens[i].line,
        last: s.tokens[i].line + s.text[start..end].bytes().filter(|&b| b == b'\n').count(),
        start,
        end,
        evidence: Evidence(Role::Test, "Nix test script"),
    });
}

fn nix_expression_end(s: &Syntax, mut value: usize) -> usize {
    while value < s.tokens.len() && s.w(value) != ";" {
        value = s.close(value).unwrap_or(value) + 1;
    }
    value
}

fn batch_directories(text: &str) -> std::collections::HashSet<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim().trim_matches('"').to_ascii_lowercase();
            Some(
                line.strip_prefix("set ")?
                    .strip_suffix("=%~dp0")?
                    .to_owned(),
            )
        })
        .collect()
}

fn batch_target<'a>(
    path: &str,
    line: &str,
    directories: impl FnOnce() -> &'a std::collections::HashSet<String>,
) -> Option<String> {
    let line = line.trim().trim_start_matches('@');
    if !line.get(..5)?.eq_ignore_ascii_case("call ") {
        return None;
    }
    let call = &line[5..];
    let operand = if let Some(quoted) = call.strip_prefix('"') {
        quoted.split('"').next()?
    } else {
        call.split_whitespace().next()?
    };
    let relative = operand.strip_prefix("%~dp0").or_else(|| {
        let end = operand.strip_prefix('%')?.find('%')? + 2;
        directories()
            .contains(&operand[1..end - 1].to_ascii_lowercase())
            .then_some(&operand[end..])
    })?;
    if !relative.ends_with(".bat") && !relative.ends_with(".cmd") {
        return None;
    }
    let directory = Path::new(path).parent()?;
    let spelling = relative.trim_start_matches(['\\', '/']).replace('\\', "/");
    crate::domain::code::relative_path(directory, &spelling)?
        .into_os_string()
        .into_string()
        .ok()
}
