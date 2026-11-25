//! Test runners embedded in host documents use the guest language's control flow.
use super::{Language, Rule, Score};
use crate::domain::code::{roles, scored};
use roles::{Context, Role};

pub(super) fn adjust(lang: Language, path: &str, text: &str, original: &Score) -> Option<Score> {
    let guest = match lang {
        Language::Html if text.contains("testharness.js") => Language::Js,
        Language::Nix if text.contains("testScript") => Language::Python,
        _ => return None,
    };
    let regions = roles::classify_with(path, text, &mut Context::default(), |_| Ok(None))
        .ok()?
        .regions;
    let mut after = 0;
    let mut line = 1;
    let mut additions = Vec::new();
    for region in regions.iter().filter(|r| r.evidence.0 == Role::Test) {
        if region.start < after {
            continue;
        }
        let src = &text[region.start..region.end];
        let start = if lang == Language::Nix {
            let (lhs, rhs) = src.split_once('=')?;
            if lhs.trim() != "testScript" {
                continue;
            }
            let literal = rhs.trim();
            if !literal.starts_with("''") || !literal.ends_with("''") || literal.len() < 4 {
                continue;
            }
            region.start + src.find(literal)? + 2
        } else {
            region.start
        };
        let end = if lang == Language::Nix {
            region.start + src.trim_end().len() - 2
        } else {
            region.end
        };
        line += text.as_bytes()[after..start]
            .iter()
            .filter(|&&c| c == b'\n')
            .count();
        after = start;
        if let Some(guest_score) = scored(guest, &text[start..end], path, None).1 {
            for f in guest_score
                .findings
                .iter()
                .filter(|f| matches!(f.rule, Rule::Bumps | Rule::Nesting))
            {
                let mut finding = f.clone();
                finding.line += line - 1;
                additions.push(finding);
            }
        }
        line += text.as_bytes()[after..end]
            .iter()
            .filter(|&&c| c == b'\n')
            .count();
        after = end;
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
