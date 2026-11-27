//! Native comment starts paired with the occupied lines in a score.
use super::source_lines::LineCursor;
use super::{Kind, Language, extended, lexer, score};
use std::ops::Range;

pub(crate) struct CommentEvidence {
    positions: CommentPositions,
    lines: Vec<usize>,
}

enum CommentPositions {
    Starts(Vec<usize>),
    OutsideStrings(Vec<Range<usize>>),
}

impl CommentEvidence {
    pub(super) fn allows(&self, at: usize) -> bool {
        match &self.positions {
            CommentPositions::Starts(offsets) => offsets.binary_search(&at).is_ok(),
            CommentPositions::OutsideStrings(strings) => {
                let next = strings.partition_point(|s| s.end <= at);
                !strings.get(next).is_some_and(|s| s.contains(&at))
            }
        }
    }

    pub(super) fn into_lines(self) -> Vec<usize> {
        self.lines
    }

    fn starts(comments: Vec<usize>, lines: impl Iterator<Item = usize>) -> Self {
        Self {
            positions: CommentPositions::Starts(comments),
            lines: occupied_lines(lines),
        }
    }

    pub(super) fn shared(lang: Language, text: &str) -> Self {
        let mut comments = Vec::new();
        let tokens = lexer::tokens_with_comments(text, lang, |at| comments.push(at));
        Self::starts(comments, tokens.iter().map(|t| t.line))
    }

    /// Some analyzers score the shared tokens while their native parsers
    /// distinguish comments from literal bodies.
    pub(super) fn native(
        lang: Language,
        text: &str,
        collect: impl FnOnce(&str, &mut dyn FnMut(usize)),
    ) -> Self {
        let mut comments = Vec::new();
        collect(text, &mut |at| comments.push(at));
        let tokens = lexer::tokens(text, lang);
        Self::starts(comments, tokens.iter().map(|t| t.line))
    }

    pub(super) fn objects(lang: Language, text: &str) -> Self {
        let mut comments = Vec::new();
        let tokens = extended::object_comments(text, lang, |at| comments.push(at));
        Self::starts(comments, tokens.iter().map(|t| t.line))
    }

    pub(super) fn scripts(lang: Language, text: &str) -> Self {
        let mut comments = Vec::new();
        let tokens = extended::script_comments(text, lang, |at| comments.push(at));
        if lang == Language::PlPgSql {
            extended::body_comments(text, &tokens, |at| comments.push(at));
            comments.sort_unstable();
        }
        let mut lines = LineCursor::new(text);
        Self::starts(comments, tokens.iter().map(|token| lines.at(token.at)))
    }

    pub(super) fn sql(text: &str) -> Self {
        let mut comments = Vec::new();
        let native = extended::script_comments(text, Language::PlPgSql, |at| comments.push(at));
        let routines = extended::body_comments(text, &native, |at| comments.push(at));
        comments.sort_unstable();
        let tokens = lexer::tokens(text, Language::Sql);
        let scored = if routines {
            score::expanded_sql(&tokens)
        } else {
            std::borrow::Cow::Borrowed(tokens.as_slice())
        };
        Self::starts(comments, scored.iter().map(|t| t.line))
    }

    /// These directives have already passed their line-oriented prefix check.
    pub(super) fn line_oriented(lang: Language, text: &str) -> Self {
        let tokens = lexer::tokens(text, lang);
        let mut strings = Vec::new();
        let lines = occupied_lines(tokens.into_iter().map(|t| {
            if t.kind == Kind::String {
                strings.push(t.span);
            }
            t.line
        }));
        Self {
            positions: CommentPositions::OutsideStrings(strings),
            lines,
        }
    }
}

/// Several token starts on one line contribute one line to scoring coverage.
fn occupied_lines(lines: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut occupied = Vec::new();
    for line in lines {
        if occupied.last() != Some(&line) {
            occupied.push(line);
        }
    }
    occupied
}
