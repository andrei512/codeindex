//! Written names in script, template, assembly and procedural SQL sources.
//! Dynamic dispatch and database search paths cannot be resolved from a file.
use crate::domain::code::comment_evidence::CommentEvidence;
use crate::domain::code::record_unique;
use crate::domain::code::{
    Analysis, Document, Language, Measured, analyzer::FileAnalyzer, score::PairedSyntax,
    script::paired_for_score,
};
use lex::{Token, tokens};
use std::{collections::HashMap, ops::Range};

mod lex;
pub(in crate::domain::code) use lex::tokens_with_comments;
mod lua;
mod mako;
pub(in crate::domain::code) use mako::analyze as mako_comments;
mod perl;
mod postgres;
pub(in crate::domain::code) use postgres::{body_comments, sql};
mod ruby;

pub(in crate::domain::code) fn analyze(lang: Language, source: &str) -> Analysis {
    if lang == Language::Mako {
        return mako::analyze(source, |_| {});
    }
    analyze_tokens(lang, source, &tokens(source, lang))
}

pub(in crate::domain::code) fn scored(
    lang: Language,
    source: &str,
) -> (Analysis, crate::domain::code::score::Saved) {
    let tokens = tokens(source, lang);
    let facts = analyze_tokens(lang, source, &tokens);
    let shared = source_tokens::<false>(source, &tokens);
    let result = crate::domain::code::score::with_tokens(lang, source, &shared, &facts);
    (facts, result)
}

pub(in crate::domain::code) struct Ruby;

impl FileAnalyzer for Ruby {
    fn comment_evidence(&self, _: Language, text: &str) -> CommentEvidence {
        CommentEvidence::scripts(Language::Ruby, text)
    }

    fn analyze(&self, doc: &Document) -> Analysis {
        ruby::analyze(doc.text, &tokens(doc.text, Language::Ruby))
    }

    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        let (facts, written) = {
            let lexed = tokens(doc.text, Language::Ruby);
            (
                ruby::analyze(doc.text, &lexed),
                source_tokens::<true>(doc.text, &lexed),
            )
        };
        PairedSyntax {
            tokens: &written,
            pairs: &paired_for_score(&written),
        }
        .measure(Language::Ruby, doc, facts, tests)
    }
}

/// Reuse Ruby literal, comment, and heredoc boundaries when locating case bodies.
pub(in crate::domain::code) fn ruby_role_tokens(
    source: &str,
) -> Vec<crate::domain::code::Token<'_>> {
    source_tokens::<true>(source, &tokens(source, Language::Ruby))
}

fn source_tokens<'a, const ROLE_LITERALS: bool>(
    source: &'a str,
    tokens: &[Token<'a>],
) -> Vec<crate::domain::code::Token<'a>> {
    use crate::domain::code::{Kind, Token as SourceToken};
    let mut previous = 0;
    let mut line = 1;
    tokens
        .iter()
        .map(|t| {
            line += source.as_bytes()[previous..t.at]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            previous = t.at;
            SourceToken {
                text: t.text,
                span: t.span(),
                line,
                kind: if t.name() {
                    Kind::Word
                } else if ROLE_LITERALS && t.text.starts_with(['\'', '"', '`']) {
                    Kind::String
                } else {
                    Kind::Punct
                },
            }
        })
        .collect()
}
fn analyze_tokens(lang: Language, source: &str, t: &[Token]) -> Analysis {
    match lang {
        Language::Ruby => ruby::analyze(source, &t),
        Language::Lua => lua::analyze(&t),
        Language::Perl => perl::analyze(source, &t),
        Language::Assembly => assembly(source, &t),
        Language::PlPgSql => postgres::analyze(source, &t),
        _ => unreachable!("unsupported script parser"),
    }
}

fn declare(out: &mut Analysis, span: Range<usize>, kind: &'static str) -> usize {
    out.declare_at(span, kind)
}

fn call(out: &mut Analysis, span: Range<usize>, owner: Option<usize>, target: usize) {
    out.direct_edge(target, span, owner, true);
}

fn word<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |v| v.text)
}
fn before<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    i.checked_sub(1).map_or("", |j| word(t, j))
}
/// Position after the optional parenthesized parameter list, including an incomplete list at EOF.
fn after_parameters(tokens: &[Token], at: usize) -> usize {
    if word(tokens, at) != "(" {
        return at;
    }
    let mut depth = 1usize;
    let mut cursor = at + 1;
    while cursor < tokens.len() && depth > 0 {
        match tokens[cursor].text {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        cursor += 1;
    }
    cursor
}

fn colon_pair(t: &[Token<'_>], i: usize) -> bool {
    word(t, i) == ":" && word(t, i + 1) == ":"
}

fn colon_name(t: &[Token<'_>], mut i: usize) -> Option<usize> {
    if colon_pair(t, i) {
        i += 2;
    }
    if !t.get(i)?.name() {
        return None;
    }
    while colon_pair(t, i + 1) && t.get(i + 3).is_some_and(|n| n.name()) {
        i += 3;
    }
    Some(i)
}

/// A local assembly label includes its leading dot only when it touches the name.
fn assembly_label(line: &[Token], at: usize) -> Option<(Range<usize>, usize)> {
    let name = at + usize::from(word(line, at) == ".");
    let token = line.get(name)?;
    if !token.name() {
        return None;
    }
    if name != at && line[at].span().end != token.at {
        return None;
    }
    Some((line[at].at..token.span().end, name + 1))
}

fn assembly_call(source: &str, line: &[Token], op: usize) -> Option<Range<usize>> {
    let opcode = word(line, op);
    let mut n = op + 1;
    if opcode.eq_ignore_ascii_case("jal") {
        if word(line, n + 1) == "," {
            if !matches!(word(line, n), "ra" | "x1") {
                return None;
            }
            n += 2;
        }
    } else if !["call", "callq", "bl"]
        .iter()
        .any(|name| opcode.eq_ignore_ascii_case(name))
    {
        return None;
    }
    let (span, after) = assembly_label(line, n)?;
    // Relocated targets such as foo@PLT have no local label edge.
    (after == line.len() && !source[span.end..].starts_with('@')).then_some(span)
}

fn assembly_lines<'t, 's>(
    source: &'s str,
    mut tokens: &'t [Token<'s>],
) -> impl Iterator<Item = &'t [Token<'s>]> {
    std::iter::from_fn(move || {
        let start = tokens.first()?.at;
        let end = source[start..]
            .find('\n')
            .map_or(source.len(), |n| start + n);
        let count = tokens.iter().take_while(|t| t.at < end).count();
        let (line, rest) = tokens.split_at(count);
        tokens = rest;
        Some(line)
    })
}

#[derive(Default)]
struct Assembly<'a> {
    out: Analysis,
    labels: HashMap<&'a str, Option<usize>>,
    pending: Vec<(Range<usize>, Option<usize>)>,
    owner: Option<usize>,
}

impl<'a> Assembly<'a> {
    fn observe(&mut self, source: &'a str, line: &[Token]) {
        let op = self.observe_label(source, line).unwrap_or(0);
        if let Some(span) = assembly_call(source, line, op) {
            self.pending.push((span, self.owner));
        }
    }

    fn observe_label(&mut self, source: &'a str, line: &[Token]) -> Option<usize> {
        let (span, after) = assembly_label(line, 0)?;
        if word(line, after) != ":" {
            return None;
        }
        let id = declare(&mut self.out, span.clone(), "label");
        record_unique(&mut self.labels, &source[span], id);
        if after == 1 {
            self.owner = Some(id);
        }
        Some(after + 1)
    }

    fn finish(mut self, source: &str) -> Analysis {
        for (span, owner) in self.pending {
            if let Some(Some(id)) = self.labels.get(&source[span.clone()]) {
                call(&mut self.out, span, owner, *id);
            }
        }
        self.out
    }
}

fn assembly(source: &str, tokens: &[Token<'_>]) -> Analysis {
    let mut analysis = Assembly::default();
    for line in assembly_lines(source, tokens) {
        analysis.observe(source, line);
    }
    analysis.finish(source)
}

#[cfg(test)]
mod tests;
