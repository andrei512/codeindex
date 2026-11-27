mod ascii;
mod classes;
mod escapes;
use super::{Anchor, Assertion, Boundary, Class, Expr, Member};
use super::{sets::Set, unicode};

/// A written capture affects regex validation even when the caller needs no registers.
enum Group {
    Scope,
    Capture(Option<usize>),
}

impl Group {
    fn wrap(self, body: Expr, check_newline: bool) -> Expr {
        match self {
            Self::Capture(Some(id)) => Expr::Capture(Some(id), Box::new(body)),
            Self::Capture(None) if check_newline && body.singleton_class().is_some() => {
                Expr::Capture(None, Box::new(body))
            }
            _ => body,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Flags(u8);

#[derive(Clone, Copy)]
enum Flag {
    Fold,
    Multiline,
    DotAll,
    Ungreedy,
    Crlf,
    Verbose,
    Unicode,
}

impl Flag {
    const SPELLINGS: [(char, Self); 7] = [
        ('i', Self::Fold),
        ('m', Self::Multiline),
        ('s', Self::DotAll),
        ('U', Self::Ungreedy),
        ('R', Self::Crlf),
        ('x', Self::Verbose),
        ('u', Self::Unicode),
    ];

    fn read(letter: char) -> Option<Self> {
        Self::SPELLINGS.iter().find_map(|&(key, flag)| (key == letter).then_some(flag))
    }

    fn bit(self) -> u8 { 1 << self as u8 }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaseFold {
    Exact,
    Ascii,
    Unicode,
}

impl Flags {
    fn case_fold(self) -> CaseFold {
        match (self.has(Flag::Fold), self.has(Flag::Unicode)) {
            (false, _) => CaseFold::Exact,
            (true, false) => CaseFold::Ascii,
            (true, true) => CaseFold::Unicode,
        }
    }

    fn has(self, flag: Flag) -> bool {
        self.0 & flag.bit() != 0
    }

    fn set(&mut self, flag: Flag, enabled: bool) {
        self.0 &= !flag.bit();
        self.0 |= flag.bit() * u8::from(enabled);
    }

    fn anchor(self, start: bool) -> Assertion {
        let mode = match (self.has(Flag::Multiline), self.has(Flag::Crlf)) {
            (false, _) => Anchor::Text,
            (true, false) => Anchor::Line,
            (true, true) => Anchor::Crlf,
        };
        if start { Assertion::Start(mode) } else { Assertion::End(mode) }
    }

    fn dot(self) -> Expr {
        let class = match (self.has(Flag::DotAll), self.has(Flag::Crlf)) {
            (true, _) => Class::Any,
            (false, true) => Class::ExceptCrlf,
            (false, false) => Class::ExceptLf,
        };
        Expr::Class(class)
    }
}

fn admit_flag(seen: u8, flag: Flag) -> Result<u8> {
    if seen & flag.bit() != 0 {
        return Err("duplicate regex flag".into());
    }
    Ok(seen | flag.bit())
}

struct Parser<'a> {
    source: &'a str,
    cursor: usize,
    flags: Flags,
    names: std::collections::HashSet<&'a str>,
    captures: Option<Vec<Option<String>>>,
    check_newline: bool,
    possible_newline: bool,
    byte_input: Option<&'a mut bool>,
}
type Result<T> = std::result::Result<T, String>;
pub(super) fn parse_groups<'a>(
    pattern: &'a str,
    options: &super::CompileOptions,
    byte_input: Option<&'a mut bool>,
) -> Result<(Expr, Vec<Option<String>>)> {
    if pattern.len() > 128 * 1024 {
        return Err("pattern too long".into());
    }
    let mut flags = Flags(Flag::Unicode.bit());
    flags.set(Flag::Fold, options.fold);
    let mut p = Parser {
        source: pattern,
        cursor: 0,
        flags,
        names: Default::default(),
        captures: options.capture.then(Vec::new),
        check_newline: options.exclude_newline,
        possible_newline: false,
        byte_input,
    };
    let body = p.expression(0)?;
    if p.peek().is_some() {
        return Err("unexpected closing parenthesis".into());
    }
    if p.rejects_newline(&body) {
        return Err(super::Regex::LITERAL_NEWLINE_ERROR.into());
    }
    Ok((body, p.captures.unwrap_or_default()))
}
impl<'a> Parser<'a> {
    fn rejects_newline(&self, body: &Expr) -> bool {
        let possible = self.check_newline && self.possible_newline;
        possible && body.line_policy().prohibits_lf()
    }

    fn require_bytes(&mut self, message: &'static str) -> Result<()> {
        let detected = self.byte_input.as_deref_mut().ok_or(message)?;
        *detected = true;
        Ok(())
    }

    fn remaining(&self) -> &'a str {
        &self.source[self.cursor..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }
    fn pop(&mut self) -> Result<char> {
        let c = self.peek().ok_or("unexpected end of pattern")?;
        self.cursor += c.len_utf8();
        Ok(c)
    }
    fn eat(&mut self, c: char) -> bool {
        if self.remaining().starts_with(c) {
            self.cursor += c.len_utf8();
            true
        } else {
            false
        }
    }
    fn eat_prefix(&mut self, prefix: &str) -> bool {
        if self.remaining().starts_with(prefix) {
            self.cursor += prefix.len();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, c: char) -> Result<()> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!(
                "expected '{c}' at character {}",
                self.source[..self.cursor].chars().count()
            ))
        }
    }
    fn take_while(&mut self, predicate: impl Fn(char) -> bool) -> &'a str {
        let rest = self.remaining();
        let len = rest.find(|c| !predicate(c)).unwrap_or(rest.len());
        self.cursor += len;
        &rest[..len]
    }

    fn braced_value(&mut self) -> Result<&'a str> {
        let rest = self.remaining();
        let len = rest.find('}').ok_or("unexpected end of pattern")?;
        self.cursor += len + 1;
        Ok(&rest[..len])
    }

    fn spaces(&mut self) {
        if self.flags.has(Flag::Verbose) {
            loop {
                self.take_while(char::is_whitespace);
                if !self.eat('#') {
                    break;
                }
                let rest = self.remaining();
                self.cursor += rest.find('\n').unwrap_or(rest.len());
            }
        }
    }
    fn capture_name(&mut self) -> Result<&'a str> {
        if !self.peek().is_some_and(|c| c == '_' || c.is_alphabetic()) {
            return Err("invalid capture name".into());
        }
        let name = self.take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'));
        self.expect('>')?;
        if !self.names.insert(name) {
            return Err("duplicate capture name".into());
        }
        Ok(name)
    }
    fn capture(&mut self, name: Option<&str>) -> Option<usize> {
        let names = self.captures.as_mut()?;
        names.push(name.map(str::to_owned));
        Some(names.len())
    }
    fn expression(&mut self, depth: usize) -> Result<Expr> {
        if depth > 100 {
            return Err("pattern nesting limit exceeded".into());
        }
        let first = self.sequence(depth)?;
        if !self.eat('|') {
            return Ok(first);
        }
        let mut alternatives = vec![first];
        loop {
            alternatives.push(self.sequence(depth)?);
            if !self.eat('|') {
                return Ok(Expr::Alternatives(alternatives));
            }
        }
    }

    fn sequence(&mut self, depth: usize) -> Result<Expr> {
        self.spaces();
        let Some(first) = self.term(depth)? else { return Ok(Expr::empty()) };
        let Some(second) = self.term(depth)? else { return Ok(first) };
        let mut sequence = vec![first, second];
        while let Some(term) = self.term(depth)? {
            sequence.push(term);
        }
        Ok(Expr::Sequence(sequence))
    }

    // Scoped flags can produce an empty atom. Consume them here so a
    // sequence only allocates after recognizing two actual terms.
    fn term(&mut self, depth: usize) -> Result<Option<Expr>> {
        while let Some(c) = self.peek().filter(|c| !matches!(c, ')' | '|')) {
            self.cursor += c.len_utf8();
            let atom = self.atom(c, depth)?;
            let atom = self.suffixes(atom, depth)?;
            if !atom.is_empty() {
                return Ok(Some(atom));
            }
        }
        Ok(None)
    }
    fn suffixes(&mut self, mut atom: Expr, mut depth: usize) -> Result<Expr> {
        self.spaces();
        while let Some(operator @ ('*' | '+' | '?' | '{')) = self.peek() {
            depth += 1;
            if depth > 100 {
                return Err("pattern nesting limit exceeded".into());
            }
            atom = self.quantify(atom, operator)?;
            self.spaces();
        }
        Ok(atom)
    }

    fn group(&mut self, depth: usize) -> Result<Expr> {
        let saved = self.flags;
        let Some(group) = self.group_header()? else {
            return Ok(Expr::empty());
        };
        let body = self.expression(depth + 1)?;
        self.expect(')')?;
        self.flags = saved;
        Ok(group.wrap(body, self.check_newline))
    }

    // A flag-only group changes the enclosing scope and consumes its own close.
    fn group_header(&mut self) -> Result<Option<Group>> {
        if !self.eat('?') {
            return Ok(Some(Group::Capture(self.capture(None))));
        }
        if self.eat('<') || self.eat_prefix("P<") {
            let name = self.capture_name()?;
            return Ok(Some(Group::Capture(self.capture(Some(name)))));
        }
        if self.group_flags()? && self.eat(')') {
            return Ok(None);
        }
        if self.eat(':') { Ok(Some(Group::Scope)) }
        else { Err("unsupported group construct or regex flags".into()) }
    }

    fn group_flags(&mut self) -> Result<bool> {
        let mut seen = 0u8;
        let enabled = self.read_flags(true, &mut seen)?;
        if !self.eat('-') {
            return Ok(enabled);
        }
        let disabled = self.read_flags(false, &mut seen)?;
        if self.peek() == Some('-') {
            return Err("duplicate flag negation".into());
        }
        if !disabled {
            return Err("expected a flag after negation".into());
        }
        Ok(true)
    }

    fn read_flags(&mut self, enabled: bool, seen: &mut u8) -> Result<bool> {
        let start = self.cursor;
        while let Some(flag) = self.peek().and_then(Flag::read) {
            *seen = admit_flag(*seen, flag)?;
            self.flags.set(flag, enabled);
            self.cursor += 1;
        }
        Ok(self.cursor != start)
    }

    fn atom(&mut self, c: char, depth: usize) -> Result<Expr> {
        // Only a written LF, escape, or bracket expression can produce a singleton LF.
        self.possible_newline |= matches!(c, '\n' | '\\' | '[');
        match c {
            '(' => self.group(depth),
            '[' => self.bracket(),
            '.' => self.dot(),
            '^' | '$' => Ok(Expr::Assert(self.flags.anchor(c == '^'))),
            '\\' => self.escape_atom(),
            '*' | '+' | '?' | '{' => Err("repetition needs an expression".into()),
            c => Ok(self.literal(c)),
        }
    }

    fn dot(&mut self) -> Result<Expr> {
        if !self.flags.has(Flag::Unicode) {
            self.require_bytes("byte-mode dot can match invalid UTF-8")?;
        }
        Ok(self.flags.dot())
    }

    fn literal(&self, c: char) -> Expr {
        let range = Member::Range(c, c);
        match self.flags.case_fold() {
            CaseFold::Unicode => Expr::Class(Class::one(range, true)),
            // A written scalar keeps its UTF-8 encoding beside raw byte escapes.
            _ if !c.is_ascii() => Expr::Scalar(c),
            CaseFold::Ascii => Expr::Class(Class::Set(ascii::literal(c))),
            CaseFold::Exact => Expr::Class(Class::one(range, false)),
        }
    }
    fn one(&self, m: Member) -> Expr {
        Expr::Class(Class::one(m, self.flags.case_fold() == CaseFold::Unicode))
    }
    fn number(&mut self) -> Result<usize> {
        let start = self.cursor;
        let mut n = 0usize;
        while let Some(byte @ b'0'..=b'9') = self.source.as_bytes().get(self.cursor).copied() {
            n = n * 10 + usize::from(byte - b'0');
            self.cursor += 1;
            self.spaces();
            if n > 65_536 {
                return Err("repetition limit exceeded".into());
            }
        }
        if start == self.cursor {
            Err("expected repetition count".into())
        } else {
            Ok(n)
        }
    }
    fn bound_separator(&mut self) {
        self.take_while(char::is_whitespace);
        self.spaces();
    }

    fn repetition_bounds(&mut self) -> Result<(usize, Option<usize>)> {
        self.bound_separator();
        let min = self.number()?;
        self.bound_separator();
        let max = if self.eat(',') { self.upper_bound()? } else { Some(min) };
        self.bound_separator();
        self.expect('}')?;
        if max.is_some_and(|n| n < min) {
            return Err("invalid repetition bounds".into());
        }
        Ok((min, max))
    }

    fn upper_bound(&mut self) -> Result<Option<usize>> {
        self.spaces();
        if self.peek() == Some('}') {
            return Ok(None);
        }
        self.bound_separator();
        self.number().map(Some)
    }

    fn quantify(&mut self, body: Expr, operator: char) -> Result<Expr> {
        self.cursor += 1; // All quantifier operators are one ASCII byte.
        let (min, max) = match operator {
            '*' => (0, None),
            '+' => (1, None),
            '?' => (0, Some(1)),
            '{' => self.repetition_bounds()?,
            _ => unreachable!("quantifier selected by sequence"),
        };
        self.spaces();
        let greedy = self.eat('?') == self.flags.has(Flag::Ungreedy);
        if max == Some(0) || body.is_empty() {
            return Ok(Expr::empty());
        }
        Ok(Expr::Repeat(super::Repetition {
            body: Box::new(body),
            min,
            max,
            greedy,
        }))
    }
}
