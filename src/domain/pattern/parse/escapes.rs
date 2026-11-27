//! Escaped atoms may assert a position. Inside a bracket expression only
//! character escapes are valid.
use super::{Assertion, Boundary, Expr, Flag, Member, Parser, Result, unicode};

impl Parser<'_> {
    pub(super) fn escape_atom(&mut self) -> Result<Expr> {
        let c = self.pop()?;
        let assertion = match c {
            'A' => Assertion::Start(super::Anchor::Text),
            'z' => Assertion::End(super::Anchor::Text),
            'b' | 'B' | '<' | '>' => Assertion::Word(self.word_boundary(c)?, !self.flags.has(Flag::Unicode)),
            _ => return self.escaped_class(c),
        };
        Ok(Expr::Assert(assertion))
    }

    fn escaped_class(&mut self, c: char) -> Result<Expr> {
        // Only the fixed-width \xNN spelling denotes a raw byte outside Unicode mode.
        if let Some(scalar) = self.escaped_scalar(c)? {
            return Ok(self.literal(scalar));
        }
        let m = self.escaped(c)?;
        if !self.flags.has(Flag::Unicode) {
            let set = self.class_set(m);
            if set.0.last().is_some_and(|r| r.1 > 127) {
                self.require_bytes("byte escape can match invalid UTF-8")?;
            }
            return Ok(self.one(Member::Set(set)));
        }
        Ok(self.one(m))
    }
    fn word_boundary(&mut self, c: char) -> Result<Boundary> {
        match c {
            '<' => Ok(Boundary::Start),
            '>' => Ok(Boundary::End),
            'b' if self.remaining().strip_prefix('{').is_some_and(|tail| tail.starts_with(char::is_alphabetic)) => {
                self.cursor += 1;
                const KINDS: &[(&str, Boundary)] = &[
                    ("start", Boundary::Start), ("end", Boundary::End),
                    ("start-half", Boundary::StartHalf), ("end-half", Boundary::EndHalf),
                ];
                let name = self.braced_value()?;
                KINDS.iter().find_map(|&(key, kind)| (key == name).then_some(kind))
                    .ok_or_else(|| "unknown word boundary".into())
            }
            _ => Ok(Boundary::Either(c == 'b')),
        }
    }

    pub(super) fn escaped_scalar(&mut self, escape: char) -> Result<Option<char>> {
        let width = match escape {
            'x' if self.peek() == Some('{') => 2,
            'u' => 4,
            'U' => 8,
            _ => return Ok(None),
        };
        self.hex_scalar(width).map(Some)
    }

    pub(super) fn escaped(&mut self, c: char) -> Result<Member> {
        let m = match c {
            'p' | 'P' => return self.property(c == 'P'),
            'd' | 'D' => Member::Digit(c == 'd'),
            's' | 'S' => Member::Space(c == 's'),
            'w' | 'W' => Member::Word(c == 'w'),
            _ => {
                let scalar = self.literal_escape(c)?;
                Member::Range(scalar, scalar)
            }
        };
        Ok(m)
    }

    fn literal_escape(&mut self, c: char) -> Result<char> {
        const CONTROLS: &[(char, char)] = &[
            ('n','\n'), ('r','\r'), ('t','\t'), ('a','\x07'), ('f','\x0c'), ('v','\x0b'),
        ];
        if c == 'x' { return self.hex_scalar(2); }
        if c.is_ascii_punctuation() || c == ' ' { return Ok(c); }
        CONTROLS.iter().find_map(|&(key, value)| (key == c).then_some(value))
            .ok_or_else(|| format!("unsupported escape \\{c}"))
    }
    fn property(&mut self, negative: bool) -> Result<Member> {
        if !self.flags.has(Flag::Unicode) {
            return Err("Unicode properties require Unicode mode".into());
        }
        let name = if self.eat('{') {
            self.braced_value()?
        } else {
            let start = self.cursor;
            self.pop()?;
            &self.source[start..self.cursor]
        };
        let set = unicode::property(name)
            .ok_or_else(|| format!("unknown Unicode property {name}"))?;
        let set = if self.flags.has(Flag::Fold) { set.folded() } else { set };
        Ok(Member::Set(if negative { set.complement() } else { set }))
    }

    fn hex_scalar(&mut self, fixed: usize) -> Result<char> {
        let mut value = 0u32;
        let braced = self.eat('{');
        let mut count = 0;
        while self.has_hex_digit(braced, count, fixed) {
            let n = self
                .pop()?
                .to_digit(16)
                .ok_or("invalid hexadecimal escape")?;
            value = value
                .checked_mul(16)
                .and_then(|x| x.checked_add(n))
                .ok_or("invalid Unicode escape")?;
            count += 1;
        }
        if braced {
            self.expect('}')?
        }
        if count == 0 {
            return Err("empty Unicode escape".into());
        }
        char::from_u32(value).ok_or_else(|| "invalid Unicode scalar".into())
    }

    fn has_hex_digit(&self, braced: bool, count: usize, fixed: usize) -> bool {
        if braced { self.peek() != Some('}') } else { count < fixed }
    }
}
