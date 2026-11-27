//! Test runners embedded in host documents use the guest language's control flow.
use super::{Language, Rule, Score};
use crate::domain::code::{Document, roles, scored, source_lines::LineCursor};
use roles::{Context, Role};
use std::ops::Range;

pub(super) fn adjust(lang: Language, path: &str, text: &str, original: &Score) -> Option<Score> {
    let (guest, body): (_, Body) = match lang {
        Language::Html if text.contains("testharness.js") => {
            (Language::Js, |_, span| Some(Some(span)))
        }
        Language::Nix if text.contains("testScript") => (Language::Python, nix_script),
        _ => return None,
    };
    let regions = roles::classify_with(path, text, &mut Context::default(), |_| Ok(None))
        .ok()?
        .regions;
    score_bodies(
        guest,
        &Document { path, text },
        original,
        guest_bodies(text, &regions, body)?,
    )
}

type Body = fn(&str, Range<usize>) -> Option<Option<Range<usize>>>;

fn guest_bodies(text: &str, regions: &[roles::Region], body: Body) -> Option<Vec<Range<usize>>> {
    let mut bodies = Vec::new();
    let mut previous_end = 0;
    for region in regions.iter().filter(|r| r.evidence.0 == Role::Test) {
        if let Some(body) = guest_span(text, region, previous_end, body)? {
            previous_end = body.end;
            bodies.push(body);
        }
    }
    Some(bodies)
}

fn guest_span(
    text: &str,
    region: &roles::Region,
    after: usize,
    body: Body,
) -> Option<Option<Range<usize>>> {
    if region.start < after {
        return Some(None);
    }
    body(text, region.start..region.end)
}

// A malformed assignment invalidates the host adjustment. Other Nix
// assignments have no guest body and allow the scan to continue.
fn nix_script(text: &str, region: Range<usize>) -> Option<Option<Range<usize>>> {
    let src = &text[region.clone()];
    let (lhs, rhs) = src.split_once('=')?;
    let literal = rhs.trim();
    if lhs.trim() != "testScript" {
        return Some(None);
    }
    let content = literal
        .strip_prefix("''")
        .and_then(|s| s.strip_suffix("''"));
    let Some(content) = content else {
        return Some(None);
    };
    let end = region.start + src.trim_end().len() - 2;
    Some(Some(end - content.len()..end))
}

pub(in crate::domain::code) fn score_bodies(
    guest: Language,
    doc: &Document,
    original: &Score,
    bodies: Vec<std::ops::Range<usize>>,
) -> Option<Score> {
    let text = doc.text;
    let mut after = 0;
    let mut lines = LineCursor::new(text);
    let mut additions = Vec::new();
    for std::ops::Range { start, end } in bodies {
        if start < after {
            continue;
        }
        after = end;
        let line = lines.at(start);
        let Some(guest_score) = scored(guest, &text[start..end], doc.path, None).1 else {
            continue;
        };
        additions.extend(guest_score.findings.iter().filter_map(|finding| {
            matches!(finding.rule, Rule::Bumps | Rule::Nesting).then(|| super::Finding {
                line: finding.line + line - 1,
                ..finding.clone()
            })
        }));
    }
    if additions.is_empty() {
        return None;
    }
    let mut findings = original.findings.clone();
    findings.extend(additions);
    Some(Score {
        lines: original.lines,
        findings,
    })
}
