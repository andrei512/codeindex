use super::{Assertion, Boundary, Class, Expr, Member};
use super::{
    sets::{Op, Set},
    unicode,
};

#[derive(Clone, Copy, Default)]
struct Flags {
    fold: bool,
    multiline: bool,
    dot_all: bool,
    ungreedy: bool,
    unicode: bool,
    verbose: bool,
    crlf: bool,
}
struct Parser {
    chars: Vec<char>,
    cursor: usize,
    flags: Flags,
    names: std::collections::HashSet<String>,
    captures: Option<Vec<Option<String>>>,
    byte_input:bool,
}
type Result<T> = std::result::Result<T, String>;
pub(super) fn parse(pattern: &str, fold: bool) -> Result<Expr> {
    parse_groups(pattern, fold, false,false).map(|(expr,_)|expr)
}
pub(super) fn parse_groups(pattern: &str, fold:bool, save:bool, byte_input:bool) -> Result<(Expr,Vec<Option<String>>)> {
    if pattern.len() > 128 * 1024 {
        return Err("pattern too long".into());
    }
    let mut p = Parser {
        chars: pattern.chars().collect(),
        cursor: 0,
        flags: Flags {
            fold,
            unicode: true,
            ..Flags::default()
        },
        names: Default::default(),
        captures: save.then(Vec::new),
        byte_input,
    };
    let body = p.expression(0)?;
    if p.peek().is_some() {
        return Err("unexpected closing parenthesis".into());
    }
    Ok((body,p.captures.unwrap_or_default()))
}
impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.cursor).copied()
    }
    fn pop(&mut self) -> Result<char> {
        let c = self.peek().ok_or("unexpected end of pattern")?;
        self.cursor += 1;
        Ok(c)
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, c: char) -> Result<()> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected '{c}' at character {}", self.cursor))
        }
    }
    fn spaces(&mut self) {
        if !self.flags.verbose {
            return;
        }
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.cursor += 1
            }
            if !self.eat('#') {
                break;
            }
            while self.peek().is_some_and(|c| c != '\n') {
                self.cursor += 1
            }
        }
    }
    fn capture_name(&mut self) -> Result<String> {
        let start = self.cursor;
        if !self.peek().is_some_and(|c| c == '_' || c.is_alphabetic()) {
            return Err("invalid capture name".into());
        }
        while self
            .peek()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'))
        {
            self.cursor += 1
        }
        let name:String = self.chars[start..self.cursor].iter().collect();
        self.expect('>')?;
        if !self.names.insert(name.clone()) {
            return Err("duplicate capture name".into());
        }
        Ok(name)
    }
    fn capture(&mut self,name:Option<String>)->Option<usize>{
        let names=self.captures.as_mut()?; names.push(name); Some(names.len())
    }
    fn expression(&mut self, depth: usize) -> Result<Expr> {
        if depth > 100 {
            return Err("pattern nesting limit exceeded".into());
        }
        let mut alternatives = vec![];
        loop {
            let mut sequence = vec![];
            self.spaces();
            while !matches!(self.peek(), None | Some(')' | '|')) {
                let mut atom = self.atom(depth)?;
                self.spaces();
                let mut quantifiers = 0;
                while matches!(self.peek(), Some('*' | '+' | '?' | '{')) {
                    quantifiers += 1;
                    if depth + quantifiers > 100 {
                        return Err("pattern nesting limit exceeded".into());
                    }
                    atom = self.quantify(atom)?;
                    self.spaces();
                }
                if !matches!(atom, Expr::Empty) {
                    sequence.push(atom);
                }
            }
            alternatives.push(match sequence.len() {
                0 => Expr::Empty,
                1 => sequence.pop().unwrap(),
                _ => Expr::Sequence(sequence),
            });
            if !self.eat('|') {
                break;
            }
        }
        Ok(if alternatives.len() == 1 {
            alternatives.pop().unwrap()
        } else {
            Expr::Alternatives(alternatives)
        })
    }
    fn atom(&mut self, depth: usize) -> Result<Expr> {
        let c = self.pop()?;
        match c {
            '(' => {
                let saved = self.flags;
                let mut group=None;
                if self.eat('?') {
                    if self.eat('<')
                        || (self.peek() == Some('P')
                            && self.chars.get(self.cursor + 1) == Some(&'<')
                            && {
                                self.cursor += 2;
                                true
                            })
                    {
                        let name=self.capture_name()?;
                        group=self.capture(Some(name));
                        let e = self.expression(depth + 1)?;
                        self.expect(')')?;
                        self.flags = saved;
                        return Ok(if let Some(i)=group {Expr::Capture(i,Box::new(e))} else {e});
                    }
                    let mut on = true;
                    let mut modified = false;
                    let mut seen = [false; 128];
                    while let Some(f) = self.peek() {
                        let slot = match f {
                            'i' => &mut self.flags.fold,
                            'm' => &mut self.flags.multiline,
                            's' => &mut self.flags.dot_all,
                            'U' => &mut self.flags.ungreedy,
                            'R' => &mut self.flags.crlf,
                            'x' => &mut self.flags.verbose,
                            'u' => &mut self.flags.unicode,
                            '-' => {
                                if !on {
                                    return Err("duplicate flag negation".into());
                                }
                                on = false;
                                self.cursor += 1;
                                continue;
                            }
                            _ => break,
                        };
                        if seen[f as usize] {
                            return Err("duplicate regex flag".into());
                        }
                        seen[f as usize] = true;
                        *slot = on;
                        modified = true;
                        self.cursor += 1;
                    }
                    if modified && self.eat(')') {
                        return Ok(Expr::Empty);
                    }
                    if !self.eat(':') {
                        return Err("unsupported group construct or regex flags".into());
                    }
                } else {group=self.capture(None);}
                let e = self.expression(depth + 1)?;
                self.expect(')')?;
                self.flags = saved;
                Ok(if let Some(i)=group {Expr::Capture(i,Box::new(e))} else {e})
            }
            '[' => self.bracket(),
            '.' if !self.flags.unicode && !self.byte_input => Err("byte-mode dot can match invalid UTF-8".into()),
            '.' => Ok(Expr::Class(Class {
                members: if self.flags.dot_all {
                    vec![]
                } else if self.flags.crlf {
                    vec![Member::Range('\r', '\r'), Member::Range('\n', '\n')]
                } else {
                    vec![Member::Range('\n', '\n')]
                }
                .into(),
                negated: true,
                fold: false,
            })),
            '^' if self.flags.multiline && self.flags.crlf => {
                Ok(Expr::Assert(Assertion::StartCrlf))
            }
            '$' if self.flags.multiline && self.flags.crlf => Ok(Expr::Assert(Assertion::EndCrlf)),
            '^' => Ok(Expr::Assert(Assertion::Start(self.flags.multiline))),
            '$' => Ok(Expr::Assert(Assertion::End(self.flags.multiline))),
            '\\' => {
                let c = self.pop()?;
                if let Some(a) = match c {
                    'A' => Some(Assertion::Start(false)),
                    'z' => Some(Assertion::End(false)),
                    'b' | 'B' | '<' | '>' => {
                        let mut kind = match c {
                            '<' => Boundary::Start,
                            '>' => Boundary::End,
                            _ => Boundary::Either(c == 'b'),
                        };
                        if c == 'b'
                            && self.peek() == Some('{')
                            && self
                                .chars
                                .get(self.cursor + 1)
                                .is_some_and(|c| c.is_alphabetic())
                        {
                            self.cursor += 1;
                            let start = self.cursor;
                            while self.peek() != Some('}') {
                                self.pop()?;
                            }
                            let tag = self.chars[start..self.cursor].iter().collect::<String>();
                            self.expect('}')?;
                            kind = match tag.as_str() {
                                "start" => Boundary::Start,
                                "end" => Boundary::End,
                                "start-half" => Boundary::StartHalf,
                                "end-half" => Boundary::EndHalf,
                                _ => return Err("unknown word boundary".into()),
                            }
                        }
                        Some(Assertion::Word(kind, !self.flags.unicode))
                    }
                    _ => None,
                } {
                    return Ok(Expr::Assert(a));
                }
                let m = self.escaped(c)?;
                if !self.flags.unicode {
                    let set = self.class_set(m);
                    if !self.byte_input && !set.0.iter().all(|r| r.1 < 128) {
                        return Err("byte escape can match invalid UTF-8".into());
                    }
                    return Ok(self.one(Member::Set(set)));
                }
                Ok(self.one(m))
            }
            '*' | '+' | '?' | '{' => Err("repetition needs an expression".into()),
            c if !self.flags.unicode && self.flags.fold && c.is_ascii() => {
                Ok(self.one(Member::Set(self.class_set(Member::Range(c, c)))))
            }
            c => Ok(self.one(Member::Range(c, c))),
        }
    }
    fn one(&self, m: Member) -> Expr {
        Expr::Class(Class {
            members: vec![m].into(),
            negated: false,
            fold: self.flags.fold && self.flags.unicode,
        })
    }
    fn escaped(&mut self, c: char) -> Result<Member> {
        let m = match c {
            'p' | 'P' if !self.flags.unicode => {
                return Err("Unicode properties require Unicode mode".into());
            }
            'p' | 'P' => {
                let value = if self.eat('{') {
                    let start = self.cursor;
                    while self.peek() != Some('}') {
                        self.pop()?;
                    }
                    let s = self.chars[start..self.cursor].iter().collect();
                    self.expect('}')?;
                    s
                } else {
                    self.pop()?.to_string()
                };
                let mut set = unicode::property(&value)
                    .ok_or_else(|| format!("unknown Unicode property {value}"))?;
                if self.flags.fold {
                    set = set.folded()
                }
                if c == 'P' {
                    set = set.complement()
                }
                Member::Set(set)
            }
            'd' | 'D' => Member::Digit(c == 'd'),
            's' | 'S' => Member::Space(c == 's'),
            'w' | 'W' => Member::Word(c == 'w'),
            _ => {
                let v = match c {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'a' => '\x07',
                    'f' => '\x0c',
                    'v' => '\x0b',
                    'x' | 'u' | 'U' => {
                        let mut value = 0u32;
                        let braced = self.eat('{');
                        let mut count = 0;
                        let fixed = match c {
                            'x' => 2,
                            'u' => 4,
                            _ => 8,
                        };
                        while (braced && self.peek() != Some('}')) || (!braced && count < fixed) {
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
                        char::from_u32(value).ok_or("invalid Unicode scalar")?
                    }
                    c if c.is_ascii_punctuation() || c == ' ' => c,
                    _ => return Err(format!("unsupported escape \\{c}")),
                };
                Member::Range(v, v)
            }
        };
        Ok(m)
    }
    fn class_set(&self, m: Member) -> Set {
        let perl = matches!(&m, Member::Word(_) | Member::Space(_) | Member::Digit(_));
        let (mut set, positive) = match m {
            Member::Range(a, b) => (Set::chars(&[(a, b)]), true),
            Member::Set(s) => return s,
            Member::Word(p) => (Set::chars(unicode::perl('w')), p),
            Member::Digit(p) => (Set::chars(unicode::perl('d')), p),
            Member::Space(p) => (Set::chars(unicode::perl('s')), p),
        };
        if !self.flags.unicode {
            if perl {
                set = set.combine(&Set::ranges(vec![(0, 127)]), Op::Intersection);
            }
            if self.flags.fold {
                let mut pairs = set.0.to_vec();
                for c in b'A'..=b'Z' {
                    let lo = c.to_ascii_lowercase();
                    if set.contains(char::from(c)) || set.contains(char::from(lo)) {
                        pairs.extend([(c as u32, c as u32), (lo as u32, lo as u32)])
                    }
                }
                set = Set::ranges(pairs)
            }
        } else if self.flags.fold {
            set = set.folded()
        }
        if positive { set } else { set.complement() }
    }
    fn class_item(&mut self, depth: usize) -> Result<Member> {
        let c = self.pop()?;
        if c == '[' {
            if self.eat(':') {
                let neg = self.eat('^');
                let start = self.cursor;
                while self.peek().is_some_and(|c| c.is_ascii_lowercase()) {
                    self.cursor += 1
                }
                let key: String = self.chars[start..self.cursor].iter().collect();
                self.expect(':')?;
                self.expect(']')?;
                let mut ranges = vec![];
                for n in 0u8..128 {
                    let yes = match key.as_str() {
                        "alnum" => n.is_ascii_alphanumeric(),
                        "alpha" => n.is_ascii_alphabetic(),
                        "ascii" => true,
                        "blank" => matches!(n, b' ' | b'\t'),
                        "cntrl" => n.is_ascii_control(),
                        "digit" => n.is_ascii_digit(),
                        "graph" => n.is_ascii_graphic(),
                        "lower" => n.is_ascii_lowercase(),
                        "print" => n.is_ascii_graphic() || n == b' ',
                        "punct" => n.is_ascii_punctuation(),
                        "space" => matches!(n, 9..=13 | 32),
                        "upper" => n.is_ascii_uppercase(),
                        "word" => n.is_ascii_alphanumeric() || n == b'_',
                        "xdigit" => n.is_ascii_hexdigit(),
                        _ => return Err("invalid POSIX class".into()),
                    };
                    if yes {
                        ranges.push((n as u32, n as u32))
                    }
                }
                let mut set = Set::ranges(ranges);
                if self.flags.fold {
                    set = set.folded()
                }
                if neg {
                    set = set.complement()
                }
                return Ok(Member::Set(set));
            }
            return self.bracket_set(depth + 1).map(Member::Set);
        }
        if c == '\\' {
            let escaped = self.pop()?;
            self.escaped(escaped)
        } else {
            Ok(Member::Range(c, c))
        }
    }
    fn operator(&self) -> Option<Op> {
        match self.chars.get(self.cursor..self.cursor + 2)? {
            ['&', '&'] => Some(Op::Intersection),
            ['-', '-'] => Some(Op::Difference),
            ['~', '~'] => Some(Op::Xor),
            _ => None,
        }
    }
    fn bracket_set(&mut self, depth: usize) -> Result<Set> {
        if depth > 100 {
            return Err("class nesting limit exceeded".into());
        }
        self.spaces();
        let negated = self.eat('^');
        let mut pending = None;
        let mut operation = Op::Union;
        let mut ranges = vec![];
        let mut first = true;
        self.spaces();
        while self.eat('-') {
            ranges.push((45, 45));
            first = false;
            self.spaces();
        }
        loop {
            self.spaces();
            if !first && (self.peek() == Some(']') || self.operator().is_some()) {
                let union = Set::ranges(std::mem::take(&mut ranges));
                pending = Some(match pending {
                    None => union,
                    Some(ref lhs) => Set::combine(lhs, &union, operation),
                });
                if self.eat(']') {
                    break;
                }
                operation = self.operator().unwrap();
                self.cursor += 2;
                continue;
            }
            let mut item = self.class_item(depth)?;
            self.spaces();
            if self.peek() == Some('-')
                && self.operator().is_none()
                && self.chars.get(self.cursor + 1).is_some_and(|&c| c != ']')
            {
                self.cursor += 1;
                let (Member::Range(a, _), Member::Range(b, _)) = (item, self.class_item(depth)?)
                else {
                    return Err("invalid character range".into());
                };
                if b < a {
                    return Err("descending character range".into());
                }
                item = Member::Range(a, b);
            }
            let set = self.class_set(item);
            ranges.extend(set.0.iter().copied());
            first = false;
        }
        let result = pending.unwrap();
        Ok(if negated { result.complement() } else { result })
    }
    fn bracket(&mut self) -> Result<Expr> {
        let set = self.bracket_set(0)?;
        if !self.byte_input && !self.flags.unicode && set.0.iter().any(|r| r.1 > 127) {
            return Err("byte class can match invalid UTF-8".into());
        }
        Ok(self.one(Member::Set(set)))
    }
    fn number(&mut self) -> Result<usize> {
        let start = self.cursor;
        let mut n = 0usize;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            n = n * 10 + self.pop()?.to_digit(10).unwrap() as usize;
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
    fn quantify(&mut self, body: Expr) -> Result<Expr> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.cursor += 1;
                (0, None)
            }
            Some('+') => {
                self.cursor += 1;
                (1, None)
            }
            Some('?') => {
                self.cursor += 1;
                (0, Some(1))
            }
            Some('{') => {
                self.cursor += 1;
                let min = self.number()?;
                let max = if self.eat(',') {
                    if self.peek() == Some('}') {
                        None
                    } else {
                        Some(self.number()?)
                    }
                } else {
                    Some(min)
                };
                self.expect('}')?;
                if max.is_some_and(|n| n < min) {
                    return Err("invalid repetition bounds".into());
                }
                (min, max)
            }
            _ => return Ok(body),
        };
        self.spaces();
        let greedy = self.eat('?') == self.flags.ungreedy;
        if max == Some(0) || matches!(body, Expr::Empty) {
            return Ok(Expr::Empty);
        }
        Ok(Expr::Repeat {
            body: Box::new(body),
            min,
            max,
            greedy,
        })
    }
}
