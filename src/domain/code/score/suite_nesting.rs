//! Pester and Perl subtests pass executable blocks to the runner. The generic
//! scorer sees their delimiters as nested braces. Each registration owns its
//! own decisions, including decisions written directly in the suite body.
use super::{Language, Rule, Score, Token, scan};
use crate::domain::code::{analyze, lexer, roles, script};

pub(crate) fn candidate(path: &str) -> Option<Language> {
    if path.ends_with(".t") {
        return Some(Language::Perl);
    }
    let lang = Language::for_path(path)?;
    match lang {
        Language::Powershell
        | Language::Dart
        | Language::Ruby
        | Language::Php
        | Language::Kotlin
            if roles::on_test_path(path, lang) =>
        {
            Some(lang)
        }
        Language::Perl | Language::Html | Language::Nix => Some(lang),
        _ => None,
    }
}

pub(crate) fn perl_source(text: &str) -> bool {
    text.lines()
        .next()
        .is_some_and(|l| l.starts_with("#!") && l.contains("perl"))
        || text.lines().any(|l| {
            l.trim_start().starts_with("use Test::") || l.trim_start().starts_with("use Test2::")
        })
}

fn block(lang: Language, t: &[Token], pairs: &[usize], i: usize) -> Option<usize> {
    let name = t[i].text;
    if lang == Language::Perl {
        if name != "subtest" || i > 0 && matches!(t[i - 1].text, ">" | "&" | "sub") {
            return None;
        }
    } else {
        if ![
            "Describe",
            "Context",
            "It",
            "BeforeAll",
            "BeforeEach",
            "AfterAll",
            "AfterEach",
            "BeforeDiscovery",
        ]
        .iter()
        .any(|s| name.eq_ignore_ascii_case(s))
        {
            return None;
        }
        if i > 0 && t[i - 1].line == t[i].line && !matches!(t[i - 1].text, ";" | "{" | "}") {
            return None;
        }
    }
    let mut j = i + 1;
    while j < t.len() {
        let prev = t[j - 1].text;
        match t[j].text {
            "{" if lang == Language::Powershell && prev == "@" => j = pairs[j].min(t.len()),
            "{" => {
                return (pairs[j] < t.len() && (lang != Language::Perl || prev == "sub"))
                    .then_some(j);
            }
            "(" if lang == Language::Perl && j == i + 1 => (),
            "(" | "[" => j = pairs[j].min(t.len()),
            ";" | "}" | ")" => return None,
            _ if lang == Language::Powershell && t[j].line != t[j - 1].line && prev != "`" => {
                return None;
            }
            _ => (),
        }
        j += 1;
    }
    None
}

// Inspect the header attached to a brace. A hash initializer, variable lookup,
// registration body or assertion callback has no control header.
fn control_body(t: &[Token], pairs: &[usize], i: usize) -> bool {
    let Some(prev) = i.checked_sub(1) else {
        return false;
    };
    let header = if t[prev].text == ")" {
        let Some(k) = pairs[prev].checked_sub(1).filter(|&k| k < prev) else {
            return false;
        };
        k
    } else {
        prev
    };
    let keyword = |word: &str| {
        [
            "if", "else", "elsif", "elseif", "unless", "for", "foreach", "while", "until", "do",
            "switch", "given", "when", "default", "try", "catch", "finally", "continue", "eval",
            "trap",
        ]
        .iter()
        .any(|x| word.eq_ignore_ascii_case(x))
    };
    if keyword(t[header].text) {
        return true;
    }
    // Perl permits an explicit iteration variable before the list.
    let mut k = header;
    while k > 0
        && header - k < 4
        && (t[k].kind == crate::domain::code::Kind::Word || t[k].text == "$")
    {
        k -= 1;
        if matches!(t[k].text, "foreach" | "for") {
            return true;
        }
    }
    false
}

pub(crate) fn adjust(lang: Language, path: &str, text: &str, original: &Score) -> Option<Score> {
    if matches!(lang, Language::Html | Language::Nix) {
        return super::embedded_tests::adjust(lang, path, text, original);
    }
    if lang == Language::Perl
        && (!text.contains("subtest") || !(roles::on_test_path(path, lang) || perl_source(text)))
    {
        return None;
    }
    let t = lexer::tokens(text, lang);
    let pairs = script::paired_for_score(&t);
    if lang == Language::Kotlin && !text.contains("kotest") {
        return None;
    }
    let ruby_ends = if lang == Language::Ruby {
        super::registrations::ruby_ends(&t)
    } else {
        Vec::new()
    };
    let mut registrations = if matches!(lang, Language::Powershell | Language::Perl) {
        t.iter()
            .enumerate()
            .filter_map(|(i, token)| {
                block(lang, &t, &pairs, i)
                    .map(|open| (open, pairs[open] + 1, token.line, token.text))
            })
            .collect::<Vec<_>>()
    } else {
        super::registrations::bodies(lang, &t, &pairs, &ruby_ends)
    };
    if registrations.is_empty() {
        return None;
    }
    registrations.sort_unstable_by_key(|entry| entry.0);
    let facts = analyze(lang, text);
    let mut declarations = scan::regions(lang, text, &t, &pairs, &facts)
        .into_iter()
        .peekable();
    let mut registrations = registrations.into_iter().peekable();
    let mut stack = vec![Depth::new("<file>", 1, t.len())];
    let mut score = Score {
        lines: original.lines,
        findings: original
            .findings
            .iter()
            .filter(|f| {
                !matches!(f.rule, Rule::Bumps | Rule::Nesting)
                    && !(lang == Language::Powershell && f.rule == Rule::Complexity)
            })
            .cloned()
            .collect(),
    };
    for (i, token) in t.iter().enumerate() {
        while stack.len() > 1 && stack.last()?.end <= i {
            stack.pop()?.record(&mut score);
        }
        while declarations.peek().is_some_and(|r| r.start == i) {
            let r = declarations.next()?;
            stack.push(Depth::new(r.name, token.line, r.end));
        }
        if let Some(&(open, end, line, name)) = registrations.peek()
            && open == i
        {
            registrations.next();
            stack.push(Depth::new(name, line, end));
        }
        let u = stack.last_mut()?;
        if lang == Language::Powershell {
            u.branches += usize::from(scan::branch(lang, token.text));
            if u.name.eq_ignore_ascii_case("It")
                && token.text.eq_ignore_ascii_case("if")
                && scan::at(&t, i + 1) == "("
                && pairs[i + 1] < t.len()
            {
                let open = pairs[i + 1] + 1;
                if scan::at(&t, open) == "{"
                    && pairs[open] < t.len()
                    && scan::at(&t, open + 1).eq_ignore_ascii_case("throw")
                    && t.get(open + 2)
                        .is_some_and(|arg| arg.kind == crate::domain::code::Kind::String)
                    && (pairs[open] == open + 3
                        || pairs[open] == open + 4 && scan::at(&t, open + 3) == ";")
                    && !["else", "elseif"]
                        .iter()
                        .any(|w| scan::at(&t, pairs[open] + 1).eq_ignore_ascii_case(w))
                {
                    // Stored findings have line precision. Preserve a compound
                    // expression when another decision shares the guard's line.
                    let first = t[..i].partition_point(|v| v.line < token.line);
                    let after = i + t[i..].partition_point(|v| v.line == token.line);
                    if t[first..after]
                        .iter()
                        .filter(|v| scan::branch(lang, v.text))
                        .count()
                        == 1
                    {
                        score
                            .findings
                            .retain(|f| !(f.rule == Rule::Conditionals && f.line == token.line));
                    }
                }
            }
        }
        let depth = match lang {
            Language::Dart | Language::Php | Language::Kotlin => {
                u.suites.depth(lang, &t, &pairs, i, u.end)
            }
            Language::Ruby => {
                while u.ends.last().is_some_and(|&end| end <= i) {
                    u.ends.pop();
                }
                if ruby_ends[i] < t.len()
                    && matches!(
                        token.text,
                        "if" | "unless" | "for" | "while" | "until" | "case" | "begin"
                    )
                {
                    u.ends.push(ruby_ends[i]);
                }
                u.ends.len()
            }
            _ => {
                while u.ends.last().is_some_and(|&end| end <= i) {
                    u.ends.pop();
                }
                if token.text == "{" && pairs[i] < t.len() && control_body(&t, &pairs, i) {
                    u.ends.push(pairs[i]);
                }
                u.ends.len()
            }
        };
        u.peak = u.peak.max(depth);
        u.regions += usize::from(depth >= 2 && !u.deep);
        u.deep = depth >= 2;
    }
    for unit in stack.into_iter().rev() {
        unit.record(&mut score);
    }
    Some(score)
}

struct Depth<'a> {
    name: &'a str,
    line: usize,
    end: usize,
    branches: usize,
    regions: usize,
    peak: usize,
    deep: bool,
    ends: Vec<usize>,
    suites: super::control::Suites,
}
impl<'a> Depth<'a> {
    fn new(name: &'a str, line: usize, end: usize) -> Self {
        Self {
            name,
            line,
            end,
            branches: 0,
            regions: 0,
            peak: 0,
            deep: false,
            ends: Vec::new(),
            suites: Default::default(),
        }
    }
    fn record(&self, score: &mut Score) {
        score.record(Rule::Complexity, self.branches + 1, self.line, self.name);
        score.record(Rule::Bumps, self.regions, self.line, self.name);
        score.record(Rule::Nesting, self.peak, self.line, self.name);
    }
}
