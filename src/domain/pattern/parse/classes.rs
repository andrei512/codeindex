//! Bracket-expression operands, set operators, and case-folding boundaries.
use super::super::sets::Op;
use super::{Expr, Flag, Member, Parser, Result, Set, ascii, unicode};

#[derive(Default)]
struct Union {
    ranges: Vec<(u32, u32)>,
    to_fold: Vec<(u32, u32)>,
}

impl Union {
    fn push(&mut self, parser: &Parser, member: Member) {
        match member {
            Member::Range(a, b) if parser.flags.has(Flag::Fold) => self.to_fold.push((a as u32, b as u32)),
            Member::Range(a, b) => self.ranges.push((a as u32, b as u32)),
            other => self.ranges.extend_from_slice(&parser.class_set(other).0),
        }
    }

    fn finish(mut self, parser: &Parser) -> Set {
        if !self.to_fold.is_empty() {
            // Literal ranges share one fold before the next set operator.
            // Negated classes and nested expressions already carry their own flags.
            let folded = parser.fold_class(Set::ranges(self.to_fold));
            if self.ranges.is_empty() {
                return folded;
            }
            self.ranges.extend_from_slice(&folded.0);
        }
        Set::ranges(self.ranges)
    }
}

impl Parser<'_> {
    pub(super) fn class_set(&self, m: Member) -> Set {
        let perl = |key| {
            if self.flags.has(Flag::Unicode) {
                Set::chars(unicode::perl(key))
            } else {
                ascii::perl(key)
            }
        };
        let (set, positive) = match m {
            Member::Range(a, b) => (Set::chars(&[(a, b)]), true),
            Member::Set(s) => return s,
            Member::Word(p) => (perl('w'), p),
            Member::Digit(p) => (perl('d'), p),
            Member::Space(p) => (perl('s'), p),
        };
        let set = self.fold_class(set);
        if positive { set } else { set.complement() }
    }

    fn fold_class(&self, set: Set) -> Set {
        match self.flags.case_fold() {
            super::CaseFold::Exact => set,
            super::CaseFold::Ascii => ascii::fold(set),
            super::CaseFold::Unicode => set.folded(),
        }
    }
    fn class_item(&mut self, depth: usize) -> Result<Member> {
        let c = self.pop()?;
        let scalar = match c {
            '[' if self.eat(':') => return self.posix_class(),
            '[' => return self.bracket_set(depth + 1).map(Member::Set),
            '\\' => {
                let escaped = self.pop()?;
                let Some(scalar) = self.escaped_scalar(escaped)? else {
                    return self.escaped(escaped);
                };
                scalar
            }
            scalar => scalar,
        };
        if !self.flags.has(Flag::Unicode) && !scalar.is_ascii() {
            return Err("Unicode not allowed in byte character class".into());
        }
        Ok(Member::Range(scalar, scalar))
    }
    fn posix_class(&mut self) -> Result<Member> {
        let negated = self.eat('^');
        let name = self.take_while(|c| c.is_ascii_lowercase());
        self.expect(':')?;
        self.expect(']')?;
        let members = self.fold_class(ascii::posix(name).ok_or("invalid POSIX class")?);
        Ok(Member::Set(if negated { members.complement() } else { members }))
    }

    fn class_range(&mut self, depth: usize) -> Result<Member> {
        let first = self.class_item(depth)?;
        self.spaces();
        match self.remaining().as_bytes() {
            [b'-', next, ..] if !matches!(next, b'-' | b']') => self.cursor += 1,
            _ => return Ok(first),
        }
        let (Member::Range(a, _), Member::Range(b, _)) = (first, self.class_item(depth)?) else {
            return Err("invalid character range".into());
        };
        if b < a { return Err("descending character range".into()); }
        Ok(Member::Range(a,b))
    }

    fn operator(&self) -> Option<Op> {
        match self.remaining().as_bytes().get(..2)? {
            b"&&" => Some(Op::Intersection),
            b"--" => Some(Op::Difference),
            b"~~" => Some(Op::Xor),
            _ => None,
        }
    }
    fn bracket_set(&mut self, depth: usize) -> Result<Set> {
        if depth > 100 {
            return Err("class nesting limit exceeded".into());
        }
        self.spaces();
        let negated = self.eat('^');
        let mut result = self.class_union(depth, true)?;
        while let Some(operator) = self.operator() {
            self.cursor += 2;
            let operand = self.class_union(depth, false)?;
            result = Set::combine(&result, &operand, operator);
        }
        self.expect(']')?;
        Ok(if negated { result.complement() } else { result })
    }

    /// The first operand permits leading literal dashes and a literal closing bracket.
    /// After an operator a closing bracket ends the operand, which may be empty.
    fn class_union(&mut self, depth: usize, first: bool) -> Result<Set> {
        let mut union = Union::default();
        if first {
            self.spaces();
            self.class_first(depth, &mut union)?;
        }
        self.spaces();
        while self.peek() != Some(']') && self.operator().is_none() {
            let member = self.class_range(depth)?;
            union.push(self, member);
            self.spaces();
        }
        Ok(union.finish(self))
    }

    fn class_first(&mut self, depth: usize, union: &mut Union) -> Result<()> {
        while self.eat('-') {
            union.ranges.push((45, 45));
            self.spaces();
        }
        if union.ranges.is_empty() {
            let member = self.class_range(depth)?;
            union.push(self, member);
        }
        Ok(())
    }

    pub(super) fn bracket(&mut self) -> Result<Expr> {
        let set = self.bracket_set(0)?;
        if !self.flags.has(Flag::Unicode) && set.0.last().is_some_and(|r| r.1 > 127) {
            self.require_bytes("byte class can match invalid UTF-8")?;
        }
        Ok(self.one(Member::Set(set)))
    }
}
