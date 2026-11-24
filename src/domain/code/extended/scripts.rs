//! Written names in script, template, assembly and procedural SQL sources.
//! Dynamic dispatch and database search paths cannot be resolved from a file.
use crate::domain::code::{Analysis, Call, Declaration, Language};
use lex::{Token, tokens};
use std::{collections::HashMap, ops::Range};

mod lex;
mod lua;
mod mako;
mod perl;
mod postgres;
mod ruby;

pub(super) fn analyze(lang: Language, source: &str) -> Analysis {
    if lang == Language::Mako {
        return mako::analyze(source);
    }
    let t = tokens(source, lang);
    match lang {
        Language::Ruby => ruby::analyze(source, &t),
        Language::Lua => lua::analyze(&t),
        Language::Perl => perl::analyze(source, &t),
        Language::Assembly => assembly(source, &t),
        Language::PlPgSql => postgres::analyze(source, &t),
        _ => unreachable!("unsupported script parser"),
    }
}

pub(super) fn sql_routines(source: &str, out: &mut Analysis) {
    postgres::sql_routines(source, out);
}

fn declare(out: &mut Analysis, span: Range<usize>, kind: &'static str) -> usize {
    let id = out.declarations.len();
    out.declarations.push(Declaration { span, kind });
    id
}

fn call(out: &mut Analysis, span: Range<usize>, owner: Option<usize>, target: usize) {
    usage(out, span, owner, target, true)
}

fn usage(
    out: &mut Analysis,
    span: Range<usize>,
    owner: Option<usize>,
    target: usize,
    is_call: bool,
) {
    let id = out.calls.len();
    out.calls.push(Call {
        span,
        owner,
        is_call,
    });
    out.direct.push((id, vec![target]));
}

fn word<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |v| v.text)
}
fn before<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    i.checked_sub(1).map_or("", |j| word(t, j))
}
fn colon_name(t: &[Token<'_>], mut i: usize) -> Option<usize> {
    if word(t, i) == ":" && word(t, i + 1) == ":" {
        i += 2;
    }
    if !t.get(i)?.name() {
        return None;
    }
    while word(t, i + 1) == ":" && word(t, i + 2) == ":" && t.get(i + 3).is_some_and(|n| n.name()) {
        i += 3;
    }
    Some(i)
}

fn assembly(source: &str, t: &[Token<'_>]) -> Analysis {
    let mut out = Analysis::default();
    let mut labels: HashMap<&str, Option<usize>> = HashMap::new();
    let mut pending = Vec::new();
    let mut owner = None;
    let mut i = 0;
    while i < t.len() {
        let start = i;
        let line_stop = source[t[i].at..]
            .find('\n')
            .map_or(source.len(), |n| t[i].at + n);
        while i < t.len() && t[i].at < line_stop {
            i += 1;
        }
        let line = &t[start..i];
        let mut op = 0;
        let prefix = usize::from(word(line, 0) == ".");
        if line.get(prefix).is_some_and(|n| n.name())
            && word(line, prefix + 1) == ":"
            && (prefix == 0 || line[0].span().end == line[1].at)
        {
            let span = line[0].at..line[prefix].span().end;
            let id = declare(&mut out, span.clone(), "label");
            labels
                .entry(&source[span])
                .and_modify(|v| *v = None)
                .or_insert(Some(id));
            if prefix == 0 {
                owner = Some(id);
            }
            op = prefix + 2;
        }
        if !matches!(
            word(line, op).to_ascii_lowercase().as_str(),
            "call" | "callq" | "bl" | "jal"
        ) {
            continue;
        }
        let mut n = op + 1;
        if word(line, op).eq_ignore_ascii_case("jal") && word(line, n + 1) == "," {
            if !matches!(word(line, n), "ra" | "x1") {
                continue;
            }
            n += 2;
        }
        let local = usize::from(word(line, n) == ".");
        if n + local + 1 != line.len() || !line[n + local].name() {
            continue;
        }
        if local > 0 && line[n].span().end != line[n + local].at {
            continue;
        }
        let span = line[n].at..line[n + local].span().end;
        if source[span.end..].starts_with('@') {
            continue;
        } // e.g. foo@PLT
        pending.push((span, owner));
    }
    for (span, owner) in pending {
        if let Some(Some(id)) = labels.get(&source[span.clone()]) {
            call(&mut out, span, owner, *id);
        }
    }
    out
}

#[cfg(test)]
mod tests;
