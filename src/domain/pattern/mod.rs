//! Prioritized Thompson automaton shared by content regex and byte-oriented path glob queries.
mod captures;
mod glob;
mod parse;
pub(crate) use captures::{CaptureScratch, CaptureSpan};
mod sets;
mod unicode;
pub(crate) use glob::BasenameGlob;
pub use glob::Glob;
use std::ops::Range;
pub(crate) use unicode::{letter_mark_or_number, punctuation_or_symbol};

mod class;
use class::{Class, Member};
#[derive(Clone, Copy, Debug)]
enum Boundary {
    Either(bool),
    Start,
    End,
    StartHalf,
    EndHalf,
}
#[derive(Clone, Copy, Debug)]
enum Anchor {
    Text,
    Line,
    Crlf,
}

impl Anchor {
    fn line_pair(crlf: bool) -> (Assertion, Assertion) {
        let line = if crlf { Self::Crlf } else { Self::Line };
        (Assertion::Start(line), Assertion::End(line))
    }

    /// The empty edge is the beginning or end of the input. Neither side of
    /// a line can begin or end between the two bytes of a CRLF terminator.
    fn accepts(self, edge: Option<char>, between_crlf: bool) -> bool {
        match self {
            Self::Text => edge.is_none(),
            Self::Line => matches!(edge, None | Some('\n')),
            Self::Crlf => !between_crlf && matches!(edge, None | Some('\r' | '\n')),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Assertion {
    Start(Anchor),
    End(Anchor),
    Word(Boundary, bool),
}
impl Assertion {
    fn accepts(self, prev: Option<char>, next: Option<char>) -> bool {
        let between_crlf = (prev, next) == (Some('\r'), Some('\n'));
        match self {
            Self::Start(anchor) => anchor.accepts(prev, between_crlf),
            Self::End(anchor) => anchor.accepts(next, between_crlf),
            Self::Word(kind, ascii) => kind.accepts(prev, next, ascii),
        }
    }
}
/// Adjacent invalid bytes delimit valid UTF-8 runs. They occupy input but cannot
/// satisfy a Unicode scalar predicate or certify a Unicode non-word boundary.
#[derive(Clone, Copy, Default)]
pub(crate) struct ChunkEdges {
    pub left_invalid: bool,
    pub right_invalid: bool,
}

impl ChunkEdges {
    fn at(self, prev: Option<char>, next: Option<char>) -> Self {
        Self {
            left_invalid: self.left_invalid && prev.is_none(),
            right_invalid: self.right_invalid && next.is_none(),
        }
    }

    fn permits(self, assertion: Assertion) -> bool {
        match assertion {
            Assertion::Word(Boundary::Either(false), false) => {
                !(self.left_invalid || self.right_invalid)
            }
            Assertion::Word(Boundary::StartHalf, false) => !self.left_invalid,
            Assertion::Word(Boundary::EndHalf, false) => !self.right_invalid,
            _ => true,
        }
    }
}

impl Boundary {
    fn accepts(self, prev: Option<char>, next: Option<char>, ascii: bool) -> bool {
        // Bits cover neither, right only, left only, and both in that order.
        let table = match self {
            Self::Either(true) => 0b0110,
            Self::Either(false) => 0b1001,
            Self::Start => 0b0010,
            Self::End => 0b0100,
            Self::StartHalf => 0b0011,
            Self::EndHalf => 0b0101,
        };
        let left = prev.is_some_and(|c| unicode::word(c, ascii));
        let right = next.is_some_and(|c| unicode::word(c, ascii));
        let index = 2 * u8::from(left) + u8::from(right);
        table & (1 << index) != 0
    }
}
#[derive(Debug)]
enum Expr {
    Capture(Option<usize>, Box<Expr>),
    Scalar(char),
    Class(Class),
    Assert(Assertion),
    Sequence(Vec<Expr>),
    Alternatives(Vec<Expr>),
    Repeat(Repetition),
}

#[derive(Debug)]
struct Repetition {
    body: Box<Expr>,
    min: usize,
    max: Option<usize>,
    greedy: bool,
}
// Bare literals in an alternation can form a character class. A capture
// preserves the boundary between that class and a compound branch.
enum LinePolicy {
    Literal(char),
    Compound { newline: bool },
}

impl LinePolicy {
    fn prohibits_lf(self) -> bool {
        matches!(self, Self::Literal('\n') | Self::Compound { newline: true })
    }

    fn alternatives(items: &[Expr]) -> bool {
        let mut lf = false;
        let mut other = false;
        let mut compound = false;
        for item in items {
            match item.line_policy() {
                Self::Literal('\n') => lf = true,
                Self::Literal(_) => other = true,
                Self::Compound { newline: true } => return true,
                Self::Compound { newline: false } => compound = true,
            }
        }
        lf && (compound || !other)
    }
}

impl Expr {
    fn empty() -> Self {
        Self::Sequence(Vec::new())
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Sequence(items) if items.is_empty())
    }

    fn line_policy(&self) -> LinePolicy {
        if let Some(c) = self.singleton_class() {
            return LinePolicy::Literal(c);
        }
        let newline = match self {
            Self::Capture(_, body) => body.line_policy().prohibits_lf(),
            Self::Sequence(items) => items.iter().any(|item| item.line_policy().prohibits_lf()),
            Self::Alternatives(items) => LinePolicy::alternatives(items),
            Self::Repeat(repeat) => {
                repeat.max != Some(0) && repeat.body.line_policy().prohibits_lf()
            }
            _ => false,
        };
        LinePolicy::Compound { newline }
    }

    fn singleton_class(&self) -> Option<char> {
        match self {
            Self::Class(class) => class.singleton(),
            Self::Scalar(c) => Some(*c),
            _ => None,
        }
    }

    fn capture_body(&self) -> &Self {
        let mut expr = self;
        while let Self::Capture(_, body) = expr { expr = body; }
        expr
    }

}

#[derive(Default)]
struct RequiredLiteral<'a> {
    items: &'a [Expr],
    bytes: usize,
}

impl<'a> RequiredLiteral<'a> {
    fn keep(&mut self, items: &'a [Expr], bytes: usize) {
        if bytes > self.bytes {
            self.items = items;
            self.bytes = bytes;
        }
    }

    fn visit(&mut self, items: &'a [Expr]) {
        let mut start = 0;
        let mut bytes = 0;
        for (i, item) in items.iter().enumerate() {
            let body = item.capture_body();
            if let Some(c) = body.singleton_class() {
                bytes += c.len_utf8();
                continue;
            }
            self.keep(&items[start..i], bytes);
            self.visit_required_children(body);
            start = i + 1;
            bytes = 0;
        }
        self.keep(&items[start..], bytes);
    }

    fn visit_required_children(&mut self, body: &'a Expr) {
        match body {
            Expr::Sequence(parts) => self.visit(parts),
            Expr::Repeat(repeat) if repeat.min > 0 => self.visit(std::slice::from_ref(repeat.body.as_ref())),
            _ => {}
        }
    }

    fn text(expr: &'a Expr) -> String {
        let mut best = Self::default();
        best.visit(std::slice::from_ref(expr));
        let mut text = String::with_capacity(best.bytes);
        text.extend(best.items.iter().filter_map(|expr| expr.capture_body().singleton_class()));
        text
    }
}
#[derive(Debug)]
enum Inst {
    Save(usize, usize),
    Accept,
    Consume(Class, usize),
    Assert(Assertion, usize),
    Fork(usize, usize),
}

/// One transition at an input boundary. Register and thread storage belong to each runner.
enum Transition {
    Accept,
    Save(usize, usize),
    Fork(usize, usize),
    Follow(usize),
    Consume(usize),
    Dead,
}

impl Transition {
    #[inline]
    fn admit(self, valid: bool) -> Self {
        if valid { self } else { Self::Dead }
    }
}

#[derive(Clone, Copy)]
struct InputBoundary {
    prev: Option<char>,
    next: Option<char>,
    edges: ChunkEdges,
    can_consume: bool,
}

impl InputBoundary {
    fn search(prev: Option<char>, next: Option<char>, edges: ChunkEdges) -> Self {
        let edges = edges.at(prev, next);
        // Invalid bytes occupy a non-word, non-line position but cannot be consumed.
        Self {
            prev: prev.or(edges.left_invalid.then_some('!')),
            next: next.or(edges.right_invalid.then_some('!')),
            edges,
            can_consume: !edges.right_invalid,
        }
    }

    fn replay(prev: Option<char>, next: Option<char>, edges: ChunkEdges, can_consume: bool) -> Self {
        Self { prev, next, edges, can_consume }
    }
}

impl Inst {
    #[inline]
    fn transition(&self, input: InputBoundary) -> Transition {
        self.transition_when(
            |assertion| input.edges.permits(assertion) && assertion.accepts(input.prev, input.next),
            |class| input.can_consume && input.next.is_some_and(|c| class.accepts(c)),
        )
    }

    #[inline]
    fn transition_when(&self, assertion_holds: impl FnOnce(Assertion) -> bool,
                       matches_class: impl FnOnce(&Class) -> bool) -> Transition {
        match self {
            Self::Accept => Transition::Accept,
            Self::Save(slot, next) => Transition::Save(*slot, *next),
            Self::Fork(first, alternative) => Transition::Fork(*first, *alternative),
            Self::Assert(assertion, next) => Transition::Follow(*next).admit(assertion_holds(*assertion)),
            Self::Consume(class, next) => Transition::Consume(*next).admit(matches_class(class)),
        }
    }
}

#[derive(Debug)]
enum Starts {
    Anywhere,
    Absolute,
    Line,
    LiteralAscii(u8),
    // Non-ASCII bytes always fall back to the Unicode VM.
    Ascii([bool; 128]),
}

#[derive(Debug)]
pub struct Regex {
    start: usize,
    program: Vec<Inst>,
    names: Vec<Option<String>>,
    required: String,
    starts: Starts,
}
#[derive(Clone, Copy)]
pub(crate) enum EmptyPosition {
    InsideScalar,
    AfterCarriageReturn { followed_by_lf: bool },
}

impl EmptyPosition {
    fn accepts(self, assertion: Assertion) -> bool {
        match self {
            Self::InsideScalar => matches!(assertion, Assertion::Word(kind, true)
                if kind.accepts(Some('!'), Some('!'), true)),
            Self::AfterCarriageReturn { followed_by_lf } =>
                assertion.accepts(Some('\r'), followed_by_lf.then_some('\n')),
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchScope {
    #[default]
    Anywhere,
    Word,
    Line,
}

impl MatchScope {
    fn anchors(self, ascii: bool, crlf: bool) -> Option<(Assertion, Assertion)> {
        match self {
            Self::Anywhere => None,
            Self::Word => Some((
                Assertion::Word(Boundary::StartHalf, ascii),
                Assertion::Word(Boundary::EndHalf, ascii),
            )),
            Self::Line => Some(Anchor::line_pair(crlf)),
        }
    }
}
#[derive(Clone, Copy, Default)]
pub(crate) struct CompileOptions {
    pub fold: bool,
    pub capture: bool,
    pub exclude_newline: bool,
    pub scope: MatchScope,
    pub ascii: bool,
    pub crlf: bool,
}

impl Regex {
    pub(crate) const MAX_INSTRUCTIONS: usize = 65_536;
    pub(crate) fn instruction_bytes(&self) -> usize {
        std::mem::size_of_val(self.program.as_slice())
    }
    pub fn new(pattern: &str) -> Result<Self, String> {
        Self::with_case_folding(pattern, false)
    }
    pub(crate) fn with_case_folding(pattern: &str, fold: bool) -> Result<Self, String> {
        Self::parse_pattern(
            pattern,
            CompileOptions {
                fold,
                ..Default::default()
            },
            None,
        )
    }
    pub(crate) fn literal(text: &str, options: CompileOptions) -> Result<Self, String> {
        if text.len() > 128 * 1024 {
            return Err("pattern too long".into());
        }
        Self::compile(options, Vec::new(), false, |builder, exit| {
            Ok((builder.fixed(text, options.fold, exit)?, String::new()))
        })
    }

    fn compile(
        options: CompileOptions,
        names: Vec<Option<String>>,
        byte_input: bool,
        emit: impl FnOnce(&mut ProgramBuilder, usize) -> Result<(usize, String), String>,
    ) -> Result<Self, String> {
        let mut builder = ProgramBuilder { instructions: vec![Inst::Accept], byte_input };
        let (start, required) = match options.scope.anchors(options.ascii, options.crlf) {
            None => emit(&mut builder, 0)?,
            Some((left, right)) => {
                let exit = builder.push(Inst::Assert(right, 0))?;
                let (entry, required) = emit(&mut builder, exit)?;
                (builder.push(Inst::Assert(left, entry))?, required)
            }
        };
        let program = builder.instructions;
        let starts = Starts::for_program(&program, start);
        Ok(Self { starts, start, program, required, names })
    }
    // Indexed regex and glob queries consume the literal as a candidate filter.
    pub(crate) fn parse_pattern(
        pattern: &str,
        options: CompileOptions,
        detect_byte_input: Option<&mut bool>,
    ) -> Result<Self, String> {
        Self::compile_pattern(pattern, options, detect_byte_input, true)
    }

    // Line matching uses the literal only for the record-separator diagnostic.
    pub(crate) fn for_lines(
        pattern: &str,
        options: CompileOptions,
        byte_input: &mut bool,
    ) -> Result<Self, String> {
        Self::compile_pattern(pattern, options, Some(byte_input), options.exclude_newline)
    }

    fn compile_pattern(
        pattern: &str,
        options: CompileOptions,
        mut detect_byte_input: Option<&mut bool>,
        need_literal: bool,
    ) -> Result<Self, String> {
        let (expr, names) =
            parse::parse_groups(pattern, &options, detect_byte_input.as_deref_mut())?;
        Self::compile(
            options,
            names,
            detect_byte_input.is_some_and(|flag| *flag),
            |builder, exit| {
                let entry = builder.emit(&expr, exit)?;
                let required = if need_literal {
                    RequiredLiteral::text(&expr)
                } else {
                    String::new()
                };
                Ok((entry, required))
            },
        )
    }

    pub(crate) const LITERAL_NEWLINE_ERROR: &'static str = "the literal \"\\n\" is not allowed in a regex\n\nConsider enabling multiline mode with the --multiline flag (or -U for short).\nWhen multiline mode is enabled, new line characters can be matched.";
    pub(crate) fn capture_id(&self, name: &str) -> Option<usize> {
        name.parse().ok().or_else(|| {
            self.names
                .iter()
                .position(|v| v.as_deref() == Some(name))
                .map(|i| i + 1)
        })
    }
    /// A record-local matcher can inspect surrounding word characters but
    /// cannot consume the separator or distinguish file anchors from line anchors.
    pub(crate) fn needs_full_text(&self, separator: char) -> bool {
        self.program.iter().any(|instruction| match instruction {
            Inst::Consume(class, _) => class.accepts(separator),
            Inst::Assert(Assertion::Word(..), _) => false,
            Inst::Assert(..) => true,
            _ => false,
        })
    }
    pub fn required_literal(&self) -> &str {
        &self.required
    }
    pub fn matches<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        self.matches_using(
            text,
            Scratch::new(self.program.len()),
            ChunkEdges::default(),
        )
    }

    pub(crate) fn matches_with<'a>(
        &'a self,
        text: &'a str,
        scratch: &'a mut Scratch,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        scratch.prepare(self.program.len());
        self.matches_using(text, scratch, ChunkEdges::default())
    }

    pub(crate) fn matches_in_chunk<'a>(
        &'a self,
        text: &'a str,
        scratch: &'a mut Scratch,
        left_invalid: bool,
        right_invalid: bool,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        scratch.prepare(self.program.len());
        self.matches_using(
            text,
            scratch,
            ChunkEdges {
                left_invalid,
                right_invalid,
            },
        )
    }

    fn matches_using<'a>(
        &'a self,
        text: &'a str,
        mut scratch: impl std::borrow::BorrowMut<Scratch> + 'a,
        edges: ChunkEdges,
    ) -> impl Iterator<Item = Range<usize>> + 'a {
        let mut cursor = MatchCursor::new(0);
        std::iter::from_fn(move || {
            while cursor.offset <= text.len() {
                let hit = scratch.borrow_mut().find_with_edges::<false>(self, text, cursor.offset, edges)?;
                if cursor.consume(text, hit.start, hit.end) {
                    return Some(hit);
                }
            }
            None
        })
    }
}
/// Both regex engines advance empty matches by one scalar and suppress
/// an empty match immediately following a match at the same position.
pub(crate) struct MatchCursor {
    pub(crate) offset: usize,
    previous_end: Option<usize>,
}

impl MatchCursor {
    pub(crate) fn new(offset: usize) -> Self {
        Self { offset, previous_end: None }
    }

    pub(crate) fn consume(&mut self, text: &str, start: usize, end: usize) -> bool {
        let previous = self.previous_end.replace(end);
        self.offset = end;
        if start != end {
            return true;
        }
        self.offset += text[end..].chars().next().map_or(1, char::len_utf8);
        previous != Some(end)
    }
}

struct ProgramBuilder {
    instructions: Vec<Inst>,
    byte_input: bool,
}

impl ProgramBuilder {
    fn push(&mut self, instruction: Inst) -> Result<usize, String> {
        let pc = self.instructions.len();
        if pc >= Regex::MAX_INSTRUCTIONS {
            return Err("compiled pattern exceeds instruction limit".into());
        }
        self.instructions.push(instruction);
        Ok(pc)
    }

    /// Prepend a fixed string to its continuation in matching order.
    fn fixed(&mut self, text: &str, fold: bool, mut next: usize) -> Result<usize, String> {
        for c in text.chars().rev() {
            let class = Class::Range {
                start: c,
                end: c,
                fold,
            };
            next = self.push(Inst::Consume(class, next))?;
        }
        Ok(next)
    }

    fn literal(&mut self, c: char, next: usize) -> Result<usize, String> {
        let class = Class::Range { start: c, end: c, fold: false };
        self.push(Inst::Consume(class, next))
    }

    fn scalar(&mut self, c: char, next: usize) -> Result<usize, String> {
        if !self.byte_input {
            return self.literal(c, next);
        }
        c.encode_utf8(&mut [0; 4]).bytes().rev()
            .try_fold(next, |next, byte| self.literal(char::from(byte), next))
    }

    fn emit(&mut self, expr: &Expr, next: usize) -> Result<usize, String> {
        match expr {
            Expr::Capture(id, body) => self.capture(*id, body, next),
            Expr::Scalar(c) => self.scalar(*c, next),
            Expr::Class(c) => self.push(Inst::Consume(c.clone(), next)),
            Expr::Assert(a) => self.push(Inst::Assert(*a, next)),
            Expr::Sequence(items) => self.sequence(items, next),
            Expr::Alternatives(items) => self.alternatives(items, next),
            Expr::Repeat(repeat) => repeat.emit(next, self),
        }
    }

    fn capture(&mut self, id: Option<usize>, body: &Expr, next: usize) -> Result<usize, String> {
        let Some(id) = id else { return self.emit(body, next) };
        let end = self.push(Inst::Save(id * 2 + 1, next))?;
        let start = self.emit(body, end)?;
        self.push(Inst::Save(id * 2, start))
    }

    fn sequence(&mut self, items: &[Expr], next: usize) -> Result<usize, String> {
        items.iter().rev().try_fold(next, |next, item| self.emit(item, next))
    }

    fn alternatives(&mut self, items: &[Expr], next: usize) -> Result<usize, String> {
        let mut items = items.iter().rev();
        let mut start = self.emit(items.next().expect("nonempty alternatives"), next)?;
        for item in items {
            let alt = self.emit(item, next)?;
            start = self.push(Inst::Fork(alt, start))?;
        }
        Ok(start)
    }
}

impl Repetition {
    fn fork(&self, body: usize, exit: usize) -> Inst {
        if self.greedy {
            Inst::Fork(body, exit)
        } else {
            Inst::Fork(exit, body)
        }
    }

    fn emit(&self, next: usize, program: &mut ProgramBuilder) -> Result<usize, String> {
        let (mut start, required) = match self.max {
            Some(max) => {
                let exit = (self.min..max).try_fold(next, |exit, _| {
                    let body = program.emit(&self.body, exit)?;
                    program.push(self.fork(body, exit))
                })?;
                (exit, self.min)
            }
            None => self.loop_entry(next, program)?,
        };
        for _ in 0..required {
            start = program.emit(&self.body, start)?;
        }
        Ok(start)
    }

    /// Execution returns from the body to the ordered fork. A zero minimum
    /// also needs a distinct entry fork so an empty body preserves priority.
    fn loop_entry(&self, exit: usize, program: &mut ProgramBuilder) -> Result<(usize, usize), String> {
        let repeat = program.push(Inst::Accept)?;
        let body = program.emit(&self.body, repeat)?;
        program.instructions[repeat] = self.fork(body, exit);
        match self.min.checked_sub(1) {
            Some(rest) => Ok((body, rest)),
            None => Ok((program.push(self.fork(body, exit))?, 0)),
        }
    }
}
mod vm;
pub(crate) use vm::{EpsilonClosure, Scratch};

#[cfg(test)]
mod tests {
    use super::*;
    fn spans(pattern: &str, input: &str) -> Vec<Range<usize>> {
        Regex::new(pattern).unwrap().matches(input).collect()
    }
    #[test]
    fn capture_syntax_preserves_record_validation_and_required_literals() {
        for capture in [false, true] {
            let parse = |pattern| Regex::parse_pattern(
                pattern, CompileOptions { capture, exclude_newline: true, ..Default::default() }, None
            );
            assert!(parse(r"(a)|(\n)").is_err());
            assert!(parse(r"a|[\n]").is_ok());
            assert!(parse(r"(?i:\n|0)").is_ok());
            assert_eq!(parse("(?i:漢123)").unwrap().required_literal(), "漢123");
            assert!(parse(r"(?i:\n|k)").is_err());
            assert_eq!(parse("(needle)+").unwrap().required_literal(), "needle");
            assert_eq!(parse("(needle)?").unwrap().required_literal(), "");
            assert_eq!(parse("(界)+").unwrap().required_literal(), "界");
            assert_eq!(parse("([a])?").unwrap().required_literal(), "");
            assert_eq!(parse("(?i:É)+").unwrap().required_literal(), "");
        }
    }

    #[test]
    fn archived_queries() {
        let text = "café\nfunction() other CAFÉ";
        assert_eq!(spans("c.fé|other", text), [0..5, 17..22]);
        assert_eq!(spans("(?i)café", text), [0..5, 23..28]);
        assert_eq!(spans(r"café\nfunction", text), [0..14]);
        assert!(Regex::new("[").is_err());
        assert!(Glob::new("**/*.ts").unwrap().is_match("src/sub/a.ts"));
        assert!(Glob::new("**/*.ts").unwrap().is_match("a.ts"));
        assert!(!Glob::new("src/*.ts").unwrap().is_match("src/sub/a.ts"));
    }
    #[test]
    fn skipping_impossible_starts_preserves_offsets_and_priority() {
        assert_eq!(spans("^a*", "aaab"), [0..3]);
        assert_eq!(spans("^$", ""), [0..0]);
        assert_eq!(spans("^a|b", "xxb"), [2..3]);
        assert_eq!(spans("(?m)^ab", "é\nxab\nab"), [7..9]);
        assert_eq!(spans("[aé]+", "xxéaa"), [2..6]);
        assert_eq!(spans(r"[a]\b", "ab a!"), [3..4]);
    }
    #[test]
    fn precedence_and_boundaries() {
        assert_eq!(spans("a|aa", "aaa"), [0..1, 1..2, 2..3]);
        assert_eq!(spans("aa|a", "aaa"), [0..2, 2..3]);
        assert_eq!(spans("a*", "aba"), [0..1, 2..3]);
        assert_eq!(spans("(b?|a)*", "abaa"), [0..0, 1..4]);
        assert_eq!(spans("a+?", "aaa"), [0..1, 1..2, 2..3]);
        assert_eq!(spans("(?m)^", "a\n\n"), [0..0, 2..2, 3..3]);
        assert_eq!(spans("", "é"), [0..0, 2..2]);
        assert_eq!(spans(r"\b\w{2,3}\b", "α2"), [0..3]);
        assert_eq!(spans("(?i)ς", "Σσ"), [0..2, 2..4]);
        assert_eq!(spans("x.*z|q", "x12q3z"), [0..6]);
        assert_eq!(spans("x.*z|q", "x12q3"), [3..4]);
        assert!(Regex::new("(?=abc)").is_err());
    }
    #[test]
    fn unicode_properties_sets_and_scoped_flags() {
        assert_eq!(spans(r"[\p{Script=Greek}&&\p{Lu}]", "αΣ"), [2..4]);
        assert_eq!(spans(r"[a-e--[bc]~~[z]]+", "bczdea"), [2..6]);
        assert_eq!(spans(r"(?-u:\w+)", "éA_"), [2..4]);
        assert!(Regex::new(r"(?-u:\xFF)").is_err());
        assert_eq!(spans("(?x)(?<val> a # skip newline\n b )", "xab"), [1..3]);
        assert_eq!(spans("(?Rm)^", "a\r\nb\rc"), [0..0, 3..3, 5..5]);
        assert_eq!(spans(r"\b{start}", "α x"), [0..0, 3..3]);
        assert_eq!(spans("[--a]", "-Aa"), [0..1, 2..3]);
    }
    #[test]
    fn glob_bytes_classes_and_alternatives() {
        for s in ["a.rs", "b/a.ts", "dir/a.rs"] {
            assert!(Glob::new("**/*.{rs,ts}").unwrap().is_match(s))
        }
        assert!(!Glob::new("?.rs").unwrap().is_match("é.rs"));
        assert!(Glob::new("??.rs").unwrap().is_match("é.rs"));
        assert!(Glob::new("[!ab].rs").unwrap().is_match("c.rs"));
        assert!(Glob::new("a/{b,,c}").unwrap().is_match("a/c"));
        assert!(!Glob::new("a/{b,,c}").unwrap().is_match("a/"));
        assert!(Glob::new("[a").is_err());
    }
}
