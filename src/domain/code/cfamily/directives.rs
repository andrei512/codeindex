use super::super::lexer::Mode;
use super::{
    Analyzer, Role, Token, condition, ident,
    project::{CProject, Command, Include, IncludeQuery, RowReuse, TranslationInput},
};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
enum Macro {
    Undefined,
    Number(condition::Integer),
    Alias(String),
    Defined,
    PositiveLanguageVersion,
    Unknown,
}

enum MacroValue<'a> {
    Stored(&'a Macro),
    Number(condition::Integer),
}

pub(super) struct Macros {
    names: HashMap<String, Macro>,
    configured: bool,
    pub(super) cpp: bool,
    pub(super) digit_separators: bool,
    native: bool,
    unsigned_char: Option<bool>,
    sdk_proofs: Vec<std::sync::Arc<super::sdk::Proof>>,
    limits: bool,
    stdint: bool,
}
impl Macros {
    fn new(command: Option<&Command>, cpp: bool) -> Self {
        let mut m = Self {
            names: HashMap::new(),
            cpp,
            configured: command.is_some(),
            digit_separators: command.is_some_and(Command::digit_separators),
            native: command.is_some_and(|c| c.native),
            unsigned_char: command.and_then(|c| c.unsigned_char),
            sdk_proofs: Vec::new(),
            limits: false,
            stdint: false,
        };
        if let Some(c) = command {
            m.apply_command(c);
        }
        m
    }

    fn apply_command(&mut self, c: &Command) {
        self.names.insert(
            "__cplusplus".into(),
            if c.cpp {
                c.standard.map_or(Macro::PositiveLanguageVersion, |n| {
                    Macro::Number(condition::Integer::signed(n))
                })
            } else {
                Macro::Undefined
            },
        );
        self.names.insert(
            "__STDC__".into(),
            Macro::Number(condition::Integer::signed(1)),
        );
        for (name, value) in &c.defines {
            let def = value
                .as_deref()
                .map_or(Macro::Undefined, |v| self.replacement(Some(v)));
            self.names.insert(name.clone(), def);
        }
    }
    fn write(&mut self, define: bool, args: &[Token], active: bool) -> bool {
        let Some(name) = args.first().filter(|name| ident(name)) else {
            return false;
        };
        let value = match (active, define) {
            (false, _) => Macro::Unknown,
            (true, false) => Macro::Undefined,
            (true, true) => self.replacement((args.len() == 2).then(|| args[1].text)),
        };
        self.names.insert(name.text.to_owned(), value);
        active && define
    }
    fn opaque_header(&mut self) {
        // A missing quoted header or an external directory before a repository
        // match can define or undefine application flags. Preserve uncertainty.
        self.configured = false;
        self.limits = false;
        self.stdint = false;
        self.names.clear();
    }
    fn external_header(
        &mut self,
        name: &str,
        active: bool,
        proof: Option<std::sync::Arc<super::sdk::Proof>>,
    ) {
        let prior = (self.limits, self.stdint);
        if let Some(proof) = proof {
            self.names.retain(|key, _| !proof.may_modify(key));
            if !self
                .sdk_proofs
                .iter()
                .any(|p| std::sync::Arc::ptr_eq(p, &proof))
            {
                self.sdk_proofs.push(proof);
            }
        } else {
            self.opaque_header();
        }
        if active && let Some(header) = super::standard::header(name) {
            self.limits |= prior.0 || header == super::standard::Header::Limits;
            self.stdint |= prior.1 || header == super::standard::Header::Stdint;
        }
    }
    fn replacement(&self, single: Option<&str>) -> Macro {
        let Some(value) = single else {
            return Macro::Defined;
        };
        if let Some(number) = condition::integer(value, self.digit_separators) {
            Macro::Number(number)
        } else if super::identifier_start(value) {
            Macro::Alias(value.to_owned())
        } else {
            Macro::Defined
        }
    }
    fn get(&self, name: &str) -> &Macro {
        self.names.get(name).unwrap_or_else(|| self.missing(name))
    }

    fn missing(&self, name: &str) -> &Macro {
        if self.configured
            && !name.starts_with("__")
            && !self.sdk_proofs.iter().any(|p| p.may_modify(name))
        {
            &Macro::Undefined
        } else {
            &Macro::Unknown
        }
    }

    fn value(&self, name: &str) -> MacroValue<'_> {
        if let Some(value) = self.names.get(name) {
            return MacroValue::Stored(value);
        }
        match self.standard_value(name) {
            Some(number) => MacroValue::Number(number),
            None => MacroValue::Stored(self.missing(name)),
        }
    }
    fn standard_value(&self, name: &str) -> Option<condition::Integer> {
        if !self.native {
            return None;
        }
        (self
            .limits
            .then(|| super::standard::limits(name, self.unsigned_char))
            .flatten())
        .or_else(|| self.stdint.then(|| super::standard::stdint(name)).flatten())
    }
    pub(super) fn positive_language_version(&self, name: &str) -> bool {
        matches!(self.get(name), Macro::PositiveLanguageVersion)
    }
    pub(super) fn defined(&self, name: &str) -> Option<bool> {
        match self.value(name) {
            MacroValue::Stored(Macro::Undefined) => Some(false),
            MacroValue::Stored(Macro::Unknown) => None,
            _ => Some(true),
        }
    }
    pub(super) fn number<'n>(&'n self, mut name: &'n str) -> Option<condition::Integer> {
        for _ in 0..64 {
            match self.value(name) {
                MacroValue::Number(n) => return Some(n),
                MacroValue::Stored(Macro::Number(n)) => return Some(*n),
                MacroValue::Stored(Macro::Undefined) => return Some(condition::Integer::signed(0)),
                MacroValue::Stored(Macro::Alias(to)) => name = to,
                _ => return None,
            }
        }
        None
    }
}
pub(super) fn logical(a: Option<bool>, b: Option<bool>, decisive: bool) -> Option<bool> {
    if a == Some(decisive) || b == Some(decisive) {
        Some(decisive)
    } else {
        a.and(b)
    }
}
struct Branches {
    active: Option<bool>,
    stack: Vec<(Option<bool>, Option<bool>)>,
}

impl Branches {
    fn enter(&mut self, test: impl FnOnce() -> Option<bool>) {
        self.stack.push((self.active, Some(true)));
        self.alternate(test);
    }

    fn alternate(&mut self, test: impl FnOnce() -> Option<bool>) {
        if let Some((parent, available)) = self.stack.last_mut() {
            let eligible = logical(*parent, *available, false);
            if eligible == Some(false) {
                self.active = Some(false);
                return;
            }
            let test = test();
            self.active = logical(eligible, test, false);
            *available = logical(*available, test.map(|v| !v), false);
        }
    }

    fn transition(&mut self, command: Directive, test: impl FnOnce(Guard) -> Option<bool>) -> bool {
        match command {
            Directive::If(guard) => self.enter(|| test(guard)),
            Directive::Alternate(guard) => self.alternate(|| test(guard)),
            Directive::End => self.end(),
            _ => return false,
        }
        true
    }

    fn end(&mut self) {
        if let Some((parent, _)) = self.stack.pop() {
            self.active = parent;
        }
    }
}

struct Scan<'a, 's> {
    project: Option<(&'a CProject<'s>, &'a Command)>,
    expansion: Option<super::translation::Expansion>,
    macros: Macros,
    headers: HashMap<String, bool>,
    consulted: HashSet<String>,
    chain: Vec<String>,
    capture_owners: bool,
    owners: HashMap<usize, String>,
}
/// Spelling inside a preprocessor header-name, including whitespace in angle names.
pub(super) struct HeaderName<'s> {
    pub(super) name: &'s str,
    pub(super) quoted: bool,
}
impl<'s> HeaderName<'s> {
    pub(super) fn read(text: &'s str, args: &[Token<'s>]) -> Option<Self> {
        let first = args.first()?;
        if let Some(quoted) = first.text.strip_prefix('"') {
            return Some(Self { name: quoted.strip_suffix('"')?, quoted: true });
        }
        let last = args.last()?;
        if (first.text, last.text) != ("<", ">") {
            return None;
        }
        Some(Self { name: text.get(first.span.end..last.span.start)?, quoted: false })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::domain::code) enum Guard {
    Expression,
    Defined(bool),
    Always,
}
#[derive(Clone, Copy)]
struct Tokenized<'a> {
    source: &'a str,
    tokens: &'a [Token<'a>],
}
impl Guard {
    // Opening and alternate directives use the same three condition spellings.
    fn suffix(suffix: &str) -> Option<Self> {
        match suffix {
            "" => Some(Self::Expression),
            "def" => Some(Self::Defined(true)),
            "ndef" => Some(Self::Defined(false)),
            _ => None,
        }
    }
    fn test(self, args: Tokenized<'_>, macros: &Macros) -> Option<bool> {
        match self {
            Self::Expression => condition::test(args.source, args.tokens, macros),
            Self::Defined(expected) => macros
                .defined(args.tokens.first().map_or("", |t| t.text))
                .map(|v| v == expected),
            Self::Always => Some(true),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::domain::code) enum Directive {
    If(Guard),
    Alternate(Guard),
    End,
    Write(bool),
    Include,
    Import,
    IncludeNext,
    Pragma,
    Other,
}
impl Directive {
    fn read(word: &str) -> Self {
        match word {
            "else" => Self::Alternate(Guard::Always),
            "endif" => Self::End,
            "define" => Self::Write(true),
            "undef" => Self::Write(false),
            _ => Self::header_or_conditional(word),
        }
    }

    fn header_or_conditional(word: &str) -> Self {
        match word {
            "include" => Self::Include,
            "import" => Self::Import,
            "include_next" => Self::IncludeNext,
            "pragma" => Self::Pragma,
            _ => Self::conditional(word),
        }
    }

    fn conditional(word: &str) -> Self {
        if let Some(suffix) = word.strip_prefix("elif") {
            return Guard::suffix(suffix).map_or(Self::Other, Self::Alternate);
        }
        word.strip_prefix("if")
            .and_then(Guard::suffix)
            .map_or(Self::Other, Self::If)
    }
}

/// Marker and keyword recognition is independent of the consumer's payload policy.
pub(in crate::domain::code) struct WrittenDirective<'a, 's> {
    pub kind: Directive,
    pub args: &'a [Token<'s>],
}

impl<'a, 's> WrittenDirective<'a, 's> {
    pub(in crate::domain::code) fn read(row: &'a [Token<'s>], digraph: bool) -> Option<Self> {
        let prefix = marker(row, 0, digraph);
        if prefix == 0 {
            return None;
        }
        Self::after_marker(&row[prefix..])
    }

    fn after_marker(tokens: &'a [Token<'s>]) -> Option<Self> {
        let (command, args) = tokens.split_first()?;
        Some(Self {
            kind: Directive::read(command.text),
            args,
        })
    }
}

pub(super) struct SourceRow {
    start: usize,
    end: usize,
    action: RowAction,
}
enum RowAction {
    Text,
    Skip,
    Directive(Statement),
}
struct Statement {
    command: Directive,
    first: usize,
    cooked_args: Option<String>,
}

fn marker(t: &[Token], i: usize, digraph: bool) -> usize {
    match t.get(i).map(|token| token.text) {
        Some("#") => 1,
        Some("%")
            if digraph
                && t.get(i + 1)
                    .is_some_and(|next| next.text == ":" && t[i].span.end == next.span.start) =>
        {
            2
        }
        _ => 0,
    }
}

struct MarkedLine<'a> {
    tokens: &'a [Token<'a>],
    prefix: usize,
}

struct DirectiveSpan {
    first_line: std::ops::Range<usize>,
    end: usize,
    prefix: usize,
}

struct RowCursor<'a, I: Iterator<Item = MarkedLine<'a>>> {
    input: Tokenized<'a>,
    lines: std::iter::Peekable<I>,
    end: usize,
    native: bool,
}

impl<'a, I: Iterator<Item = MarkedLine<'a>>> RowCursor<'a, I> {
    fn consume_text(&mut self) {
        while let Some(line) = self.lines.next_if(|line| line.prefix == 0) {
            self.end += line.tokens.len();
        }
    }

    fn consume_continuation(&mut self, mut line: &'a [Token<'a>]) {
        while let Some(last) = line.last().filter(|token| token.text == "\\") {
            let Some(next) = self
                .lines
                .next_if(|line| line.tokens[0].line == last.line + 1)
            else {
                break;
            };
            line = next.tokens;
            self.end += line.len();
        }
    }
}

impl<'a, I: Iterator<Item = MarkedLine<'a>>> Iterator for RowCursor<'a, I> {
    type Item = SourceRow;

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.lines.next()?;
        let start = self.end;
        self.end += line.tokens.len();
        let action = if line.prefix == 0 {
            self.consume_text();
            RowAction::Text
        } else {
            let first_line = start..self.end;
            if !self.native {
                self.consume_continuation(line.tokens);
            }
            let span = DirectiveSpan {
                first_line,
                end: self.end,
                prefix: line.prefix,
            };
            RowAction::Directive(Statement::from_lines(self.input, span))
        };
        Some(SourceRow {
            start,
            end: self.end,
            action,
        })
    }
}

fn rows<'a>(t: &'a [Token], text: &'a str, native: bool) -> impl Iterator<Item = SourceRow> + 'a {
    let gap_sensitive = native && (text.contains("/*") || text.contains("R\""));
    let lines = t
        .chunk_by(move |left, right| {
            left.line == right.line
                || gap_sensitive
                    && !super::physical_lines::starts_new_line(
                        &text[left.span.end..right.span.start],
                    )
        })
        .map(move |tokens| MarkedLine {
            tokens,
            prefix: marker(tokens, 0, native),
        })
        .peekable();
    RowCursor {
        input: Tokenized {
            source: text,
            tokens: t,
        },
        lines,
        end: 0,
        native,
    }
}

#[derive(Default)]
struct Projection {
    changes: Vec<(usize, usize)>,
    next_change: usize,
    delta: usize,
    rows: Vec<SourceRow>,
}
impl Projection {
    // Cooked starts include a splice at the same offset. Exclusive ends
    // stay on its left. Row and token spans arrive in physical order.
    fn offset(&mut self, at: usize, end: bool) -> usize {
        let through = at + usize::from(!end);
        while let Some(&(_, delta)) = self
            .changes
            .get(self.next_change)
            .filter(|(offset, _)| *offset < through)
        {
            self.delta = delta;
            self.next_change += 1;
        }
        at + self.delta
    }
    fn gap(&mut self, end: usize) {
        let cursor = self.rows.last().map_or(0, |row| row.end);
        if cursor < end {
            self.rows.push(SourceRow {
                start: cursor,
                end,
                action: RowAction::Skip,
            });
        }
    }
    fn tokens(&self, t: &[Token], span: std::ops::Range<usize>) -> std::ops::Range<usize> {
        let cursor = self.rows.last().map_or(0, |row| row.end);
        let start = cursor
            + t[cursor..]
                .iter()
                .take_while(|tok| tok.span.end <= span.start)
                .count();
        let end = start
            + t[start..]
                .iter()
                .take_while(|tok| tok.span.start < span.end)
                .count();
        start..end
    }
    fn push(&mut self, tokens: std::ops::Range<usize>, action: RowAction) {
        let std::ops::Range { start, end } = tokens;
        self.gap(start);
        if start == end {
            return;
        }
        if matches!(action, RowAction::Text)
            && let Some(last) = self
                .rows
                .last_mut()
                .filter(|r| matches!(r.action, RowAction::Text))
        {
            last.end = end;
        } else {
            self.rows.push(SourceRow { start, end, action });
        }
    }
    fn statement(&mut self, row: SourceRow, logical: Tokenized<'_>, written: Tokenized<'_>) {
        let Tokenized {
            source: cooked,
            tokens: logical,
        } = logical;
        let Tokenized {
            source: text,
            tokens: written,
        } = written;
        let slice = &logical[row.start..row.end];
        let RowAction::Directive(Statement { command, first, .. }) = row.action else {
            for token in slice {
                let span = self.offset(token.span.start, false)..self.offset(token.span.end, true);
                self.push(self.tokens(written, span), RowAction::Text);
            }
            return;
        };
        let start = self.offset(slice[0].span.start, false);
        let cooked_end = slice.last().unwrap().span.end;
        let args = logical.get(first).filter(|_| first < row.end);
        let raw_start = args.map(|tok| self.offset(tok.span.start, false));
        let changes_before_arguments = self.next_change;
        let end = self.offset(cooked_end, true);
        let raw_name = raw_start.unwrap_or(end);
        let tokens = self.tokens(written, start..end);
        let name =
            tokens.start + written[tokens.clone()].partition_point(|tok| tok.span.end <= raw_name);
        let cooked_args = args
            .filter(|_| self.next_change != changes_before_arguments)
            .and_then(|tok| {
                let argument = &cooked[tok.span.start..cooked_end];
                (text[raw_name..end] != *argument).then(|| argument.to_owned())
            });
        self.push(
            tokens,
            RowAction::Directive(Statement {
                command,
                first: name,
                cooked_args,
            }),
        );
    }
}
fn translated_rows(text: &str, t: &[Token], mode: Mode) -> Option<Vec<SourceRow>> {
    let mut projection = Projection::default();
    let std::borrow::Cow::Owned(cooked) = super::physical_lines::translate(
        text,
        || super::physical_lines::raw_strings(text, t, mode.language),
        |at, delta| projection.changes.push((at, delta)),
    ) else {
        return None;
    };
    let logical = mode.tokens(&cooked);
    let translated = Tokenized {
        source: &cooked,
        tokens: &logical,
    };
    let original = Tokenized {
        source: text,
        tokens: t,
    };
    for row in rows(&logical, &cooked, true) {
        projection.statement(row, translated, original);
    }
    projection.gap(t.len());
    Some(projection.rows)
}
pub(super) fn logical_rows<'a>(
    text: &'a str,
    t: &'a [Token],
    mode: Mode,
) -> impl Iterator<Item = SourceRow> + 'a {
    let native = matches!(mode.language, super::Language::C | super::Language::Cpp);
    let translated = native.then(|| translated_rows(text, t, mode)).flatten();
    let plain = if translated.is_some() { &t[..0] } else { t };
    translated
        .into_iter()
        .flatten()
        .chain(rows(plain, text, native))
}

#[derive(Clone, Copy)]
struct FileSource<'a> {
    mode: Mode,
    path: &'a str,
    text: &'a str,
    tokens: &'a [Token<'a>],
}

impl<'a> FileSource<'a> {
    fn captured(path: &'a str, source: &'a super::project::PreprocessingSource<'a>) -> Self {
        Self {
            path,
            text: source.text,
            mode: source.mode,
            tokens: &source.tokens,
        }
    }
}
struct FileVisit<'a> {
    source: FileSource<'a>,
    cached: Option<&'a [SourceRow]>,
    main: bool,
    active: Option<bool>,
}

impl<'a> FileVisit<'a> {
    fn main(source: FileSource<'a>, cached: Option<&'a [SourceRow]>) -> Self {
        Self {
            source,
            cached,
            main: true,
            active: Some(true),
        }
    }

    fn header(
        path: &'a str,
        source: &'a super::project::PreprocessingSource<'a>,
        active: Option<bool>,
    ) -> Self {
        Self {
            source: FileSource::captured(path, source),
            cached: Some(source.rows()),
            main: false,
            active,
        }
    }
}

struct FileFrame<'a> {
    source: FileSource<'a>,
    origin: Option<usize>,
    main: bool,
    branches: Branches,
}

impl Statement {
    fn from_lines(input: Tokenized<'_>, span: DirectiveSpan) -> Self {
        let DirectiveSpan {
            first_line: line,
            end,
            prefix,
        } = span;
        let Tokenized { source, tokens } = input;
        let row = &tokens[line.clone()];
        let (command, first) = WrittenDirective::after_marker(&row[prefix..])
            .map_or((Directive::Other, line.end), |directive| {
                (directive.kind, line.end - directive.args.len())
            });
        let cooked_args = (line.end != end
            && matches!(
                command,
                Directive::If(Guard::Expression) | Directive::Alternate(Guard::Expression)
            ))
        .then(|| {
            source[tokens[first - 1].span.end..tokens[end - 1].span.end]
                .replace("\\\r\n", "")
                .replace("\\\n", "")
        });
        Self {
            command,
            first,
            cooked_args,
        }
    }
    fn with_arguments<R>(
        &self,
        file: FileSource<'_>,
        end: usize,
        read: impl FnOnce(Tokenized<'_>) -> R,
    ) -> R {
        let cooked;
        let args = match self.cooked_args.as_deref() {
            Some(text) => {
                cooked = file.mode.tokens(text);
                Tokenized {
                    source: text,
                    tokens: &cooked,
                }
            }
            None => Tokenized {
                source: file.text,
                tokens: &file.tokens[self.first..end],
            },
        };
        read(args)
    }

    fn apply(&self, frame: &mut FileFrame, end: usize, scan: &mut Scan) -> Option<usize> {
        let file = frame.source;
        let conditional = frame.branches.transition(self.command, |guard| {
            self.with_arguments(file, end, |args| guard.test(args, &scan.macros))
        });
        if conditional || frame.branches.active == Some(false) {
            return None;
        }
        self.with_arguments(file, end, |args| {
            if !scan.effect(file.path, self.command, args, frame.branches.active) {
                return None;
            }
            let name = args.tokens.first()?.text;
            (file.tokens.get(self.first)?.text == name).then_some(self.first)
        })
    }
}

impl FileFrame<'_> {
    fn row(
        &mut self,
        scan: &mut Scan,
        row: &SourceRow,
        output: &mut impl FnMut(usize, usize, Option<usize>),
    ) {
        let definition = match &row.action {
            RowAction::Text if self.branches.active == Some(true) => {
                self.active_line(scan, row.start..row.end);
                return;
            }
            RowAction::Skip | RowAction::Text => None,
            RowAction::Directive(statement) => statement.apply(self, row.end, scan),
        };
        if let Some(name) = definition
            && let Some((expansion, id)) = scan.expansion.as_mut().zip(self.origin)
        {
            expansion.definition(id, name);
        }
        output(row.start, row.end, definition);
    }

    fn active_line(&self, scan: &mut Scan, range: std::ops::Range<usize>) {
        if let Some((expansion, origin)) = scan.expansion.as_mut().zip(self.origin) {
            expansion.active(origin, range.clone());
        }
        if !scan.capture_owners {
            return;
        }
        for i in range.clone() {
            if let Some(Macro::Alias(value)) = scan.macros.names.get(self.source.tokens[i].text)
                && let Some(owner) = super::owner_macro::owner(value)
            {
                if self.main {
                    scan.owners.insert(i, owner.to_owned());
                }
                if let Some(expansion) = scan.expansion.as_mut() {
                    expansion.owner(range.end - i, owner);
                }
            }
        }
    }
}

impl Scan<'_, '_> {
    fn file(&mut self, visit: FileVisit<'_>, mut output: impl FnMut(usize, usize, Option<usize>)) {
        let FileVisit {
            source,
            cached,
            main,
            active,
        } = visit;
        let path = source.path;
        if self.chain.len() >= 128 || self.chain.iter().any(|p| p == path) {
            return;
        }
        let origin = self.expansion.as_mut().map(|e| e.enter(path, main));
        self.chain.push(path.to_owned());
        let mut frame = FileFrame {
            source,
            origin,
            main,
            branches: Branches {
                active,
                stack: Vec::new(),
            },
        };
        let mut apply = |row: &SourceRow| frame.row(self, row, &mut output);
        match cached {
            Some(rows) => rows.iter().for_each(apply),
            None => {
                logical_rows(source.text, source.tokens, source.mode).for_each(|row| apply(&row))
            }
        }
        self.chain.pop();
    }
    // A directive changes branch or macro state; only an active definition
    // contributes a declaration to the current translation.
    fn effect(
        &mut self,
        path: &str,
        command: Directive,
        args: Tokenized<'_>,
        active: Option<bool>,
    ) -> bool {
        let Tokenized {
            source: text,
            tokens: args,
        } = args;
        let name = args.first().map_or("", |t| t.text);
        match command {
            Directive::Write(define) => {
                return self.macros.write(define, args, active.is_some());
            }
            Directive::Pragma if name == "once" && active.is_some() => {
                self.headers.insert(path.to_owned(), true);
            }
            Directive::Include | Directive::Import => self.include(
                path,
                HeaderName::read(text, args),
                active,
                command == Directive::Import,
            ),
            _ => {}
        }
        false
    }
    fn include(
        &mut self,
        path: &str,
        header: Option<HeaderName>,
        active: Option<bool>,
        import: bool,
    ) {
        let Some((project, command)) = self.project else {
            return;
        };
        let Some(HeaderName { name, quoted }) = header else {
            self.macros.opaque_header();
            return;
        };
        let query = IncludeQuery {
            name: &name,
            quoted_from: quoted.then(|| std::path::Path::new(path)),
            optional: active.is_none(),
        };
        match project.include(query, command, &mut self.consulted) {
            Include::Captured(child, source) => self.header(child, &source, active, import),
            Include::Opaque => self.macros.opaque_header(),
            Include::External(proof) => {
                self.macros
                    .external_header(&name, active == Some(true), proof);
            }
        }
    }

    fn header(
        &mut self,
        path: &str,
        source: &super::project::PreprocessingSource,
        active: Option<bool>,
        import: bool,
    ) {
        if let Some(sealed) = self.headers.get_mut(path) {
            // Once a header was included, import skips its body. Only a
            // definitely active import can seal it against subsequent includes.
            if *sealed || import {
                *sealed |= import && active == Some(true);
                return;
            }
        } else if active == Some(true) {
            self.headers.insert(path.to_owned(), import);
        }
        self.file(FileVisit::header(path, source, active), |_, _, _| {});
    }
}
impl Analyzer<'_, '_> {
    fn scan_rows(&mut self, scan: &mut Scan, file: FileSource<'_>, cached: Option<&[SourceRow]>) {
        scan.file(FileVisit::main(file, cached), |i, end, definition| {
            self.skip[i..end].fill(true);
            if let Some(name) = definition {
                self.add(name, 0, "local_binding", Role::Value);
            }
        });
    }
    pub(super) fn directives(&mut self, text: &str, lang: super::Language) {
        let mode = Mode::written(lang);
        let source = FileSource {
            path: "",
            text,
            mode,
            tokens: self.t,
        };
        self.written_directives(source, None);
    }
    fn written_directives(&mut self, source: FileSource<'_>, cached: Option<&[SourceRow]>) {
        self.scan_rows(
            &mut Scan::new(None, source.mode.language == super::Language::Cpp, false),
            source,
            cached,
        );
    }
    pub(super) fn source_directives(&mut self, input: TranslationInput) {
        self.written_directives(input.file(), input.rows());
    }
    pub(super) fn project_directives(
        &mut self,
        project: &CProject,
        command: &Command,
        input: TranslationInput,
        graph: bool,
    ) -> (HashSet<String>, Option<super::translation::Partition>) {
        let mut scan = Scan::configured(project, command, graph);
        self.scan_rows(&mut scan, input.file(), input.rows());
        self.macro_owners = std::mem::take(&mut scan.owners);
        scan.finish(input, super::translation::PartitionPolicy::Included)
    }
}

impl<'p, 'src> Scan<'p, 'src> {
    fn new(project: Option<(&'p CProject<'src>, &'p Command)>, cpp: bool, graph: bool) -> Self {
        let command = project.map(|(_, c)| c);
        Self {
            project,
            expansion: graph.then(Default::default),
            macros: Macros::new(command, cpp),
            headers: HashMap::new(),
            consulted: HashSet::new(),
            chain: Vec::new(),
            capture_owners: command.is_some_and(|c| {
                c.defines
                    .values()
                    .flatten()
                    .any(|v| super::owner_macro::owner(v).is_some())
            }),
            owners: HashMap::new(),
        }
    }
    fn configured(project: &'p CProject<'src>, command: &'p Command, graph: bool) -> Self {
        let mut scan = Self::new(Some((project, command)), command.cpp, graph);
        for (candidate, spelling) in &command.forced {
            scan.apply_forced_header(project, command, candidate, spelling);
        }
        scan
    }

    fn apply_forced_header(
        &mut self,
        project: &CProject,
        command: &Command,
        candidate: &str,
        spelling: &str,
    ) {
        match project.forced_header(candidate, spelling, command, &mut self.consulted) {
            Some((file, source)) => self.header(file, &source, Some(true), false),
            None => self.macros.opaque_header(),
        }
    }

    fn finish(
        self,
        input: TranslationInput,
        policy: super::translation::PartitionPolicy,
    ) -> (HashSet<String>, Option<super::translation::Partition>) {
        let partition = self
            .expansion
            .zip(self.project)
            .and_then(|(expansion, project)| expansion.bind(project, input, policy));
        (self.consulted, partition)
    }
}

// A multi-command translation consumes only expanded tokens and dependency paths.
// Its written declarations are collected separately by the file's own analyzer.
pub(super) fn configured_partition(
    project: &CProject,
    command: &Command,
    input: TranslationInput,
) -> (HashSet<String>, Option<super::translation::Partition>) {
    let mut scan = Scan::configured(project, command, true);
    scan.file(FileVisit::main(input.file(), input.rows()), |_, _, _| {});
    scan.finish(input, super::translation::PartitionPolicy::Configured)
}
impl<'a> TranslationInput<'a> {
    fn file(self) -> FileSource<'a> {
        FileSource::captured(self.path, self.source)
    }
    fn rows(self) -> Option<&'a [SourceRow]> {
        self.source
            .reusable_rows(matches!(self.reuse, RowReuse::Shared))
    }
}

#[cfg(test)]
mod conditional_tests {
    use super::*;
    use crate::domain::code::{Language, lexer};

    #[test]
    fn conditional_uses_both_arm_types_and_only_the_selected_value() {
        let macros = Macros::new(None, false);
        for (expression, expected) in [
            ("(1 ? -1 : 0U) > 0", Some(true)),
            ("0 ? 1 / 0 : 0 ? 3 : 7", Some(true)),
            ("1 || 0 ? 0 : 1", Some(false)),
            ("0 ? OPAQUE(2) : 1", None),
            ("1 ? 2", None),
        ] {
            let tokens = lexer::tokens(expression, Language::C);
            assert_eq!(
                condition::test(expression, &tokens, &macros),
                expected,
                "{expression}"
            );
        }
    }
}
