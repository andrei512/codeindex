//! Preprocessor integer arithmetic uses intmax_t or uintmax_t. Preserve
//! signedness through promotions so comparisons choose the same active branch.
use super::{Token, directives::Macros};

#[derive(Clone, Copy)]
pub(super) struct Integer {
    bits: u64,
    unsigned: bool,
}
impl Integer {
    pub(super) fn unsigned(bits: u64) -> Self {
        Self {
            bits,
            unsigned: true,
        }
    }
    pub(super) fn signed(n: i64) -> Self {
        Self {
            bits: n as u64,
            unsigned: false,
        }
    }
    fn boolean(v: bool) -> Self {
        Self::signed(i64::from(v))
    }
    fn truth(self) -> bool {
        self.bits != 0
    }
    fn negate(self) -> Option<Self> {
        Some(if self.unsigned {
            Self {
                bits: self.bits.wrapping_neg(),
                ..self
            }
        } else {
            Self::signed((self.bits as i64).checked_neg()?)
        })
    }
    fn op(self, op: &str, b: Self) -> Option<Self> {
        let (x, y) = (self.bits, b.bits);
        let unsigned = self.unsigned || b.unsigned;
        let order = if unsigned {
            x.cmp(&y)
        } else {
            (x as i64).cmp(&(y as i64))
        };
        let bits = match op {
            "==" => return Some(Self::boolean(x == y)),
            "!=" => return Some(Self::boolean(x != y)),
            "<" => return Some(Self::boolean(order.is_lt())),
            "<=" => return Some(Self::boolean(order.is_le())),
            ">" => return Some(Self::boolean(order.is_gt())),
            ">=" => return Some(Self::boolean(order.is_ge())),
            "&&" => return Some(Self::boolean(self.truth() && b.truth())),
            "||" => return Some(Self::boolean(self.truth() || b.truth())),
            "&" => x & y,
            "|" => x | y,
            "^" => x ^ y,
            "<<" => {
                return Some(Self {
                    bits: x.checked_shl(y.try_into().ok()?)?,
                    ..self
                });
            }
            ">>" => {
                return Some(Self {
                    bits: if self.unsigned {
                        x.checked_shr(y.try_into().ok()?)?
                    } else {
                        (x as i64).checked_shr(y.try_into().ok()?)? as u64
                    },
                    ..self
                });
            }
            "+" if unsigned => x.wrapping_add(y),
            "-" if unsigned => x.wrapping_sub(y),
            "*" if unsigned => x.wrapping_mul(y),
            "/" if unsigned => x.checked_div(y)?,
            "%" if unsigned => x.checked_rem(y)?,
            _ => {
                let (x, y) = (x as i64, y as i64);
                return Some(Self::signed(match op {
                    "+" => x.checked_add(y)?,
                    "-" => x.checked_sub(y)?,
                    "*" => x.checked_mul(y)?,
                    "/" => x.checked_div(y)?,
                    "%" => x.checked_rem(y)?,
                    _ => return None,
                }));
            }
        };
        Some(Self { bits, unsigned })
    }
}
pub(super) fn integer(text: &str) -> Option<Integer> {
    if let Some(s) = text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        let code = match s.as_bytes() {
            [c] if c.is_ascii() => *c,
            [b'\\', c] => match c {
                b'n' => 10,
                b'r' => 13,
                b't' => 9,
                b'0' => 0,
                b'a' => 7,
                b'b' => 8,
                b'f' => 12,
                b'v' => 11,
                b'\\' | b'\'' | b'"' | b'?' => *c,
                _ => return None,
            },
            _ if s.starts_with("\\x") => u8::from_str_radix(&s[2..], 16).ok()?,
            _ if s.starts_with('\\') => u8::from_str_radix(&s[1..], 8).ok()?,
            _ => return None,
        };
        return (code < 128).then(|| Integer::signed(code.into()));
    }
    let digits = text.trim_end_matches(['l', 'L', 'u', 'U']);
    let suffix = &text[digits.len()..];
    let (radix, digits) = if let Some(s) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, s)
    } else if let Some(s) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        (2, s)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, digits)
    } else {
        (10, digits)
    };
    let bits = u64::from_str_radix(digits, radix).ok()?;
    Some(Integer {
        bits,
        unsigned: suffix.contains(['u', 'U']) || bits > i64::MAX as u64,
    })
}
pub(super) fn test(tokens: &[Token], macros: &Macros) -> Option<bool> {
    if tokens.len() > 1024 {
        return None;
    }
    if let [single] = tokens {
        if macros.positive_language_version(single.text) {
            // Every C++ language-version value is positive, including future standards.
            return Some(true);
        }
    }
    let mut p = Expression {
        tokens,
        i: 0,
        macros,
        depth: 0,
    };
    let n = p.expr(0);
    if p.i == tokens.len() {
        n.map(Integer::truth)
    } else {
        None
    }
}
struct Expression<'a, 's, 'm> {
    tokens: &'a [Token<'s>],
    macros: &'m Macros,
    i: usize,
    depth: usize,
}
impl Expression<'_, '_, '_> {
    fn peek(&self, n: usize) -> &str {
        self.tokens.get(self.i + n).map_or("", |t| t.text)
    }
    fn atom(&mut self) -> Option<Integer> {
        if self.depth >= 80 {
            self.i = self.tokens.len();
            return None;
        }
        self.depth += 1;
        let n = self.operand();
        self.depth -= 1;
        n
    }
    fn operand(&mut self) -> Option<Integer> {
        let word = self.tokens.get(self.i)?.text;
        self.i += 1;
        match word {
            "(" => {
                let n = self.expr(0);
                if self.peek(0) != ")" {
                    return None;
                }
                self.i += 1;
                n
            }
            "!" => self.atom().map(|n| Integer::boolean(!n.truth())),
            "~" => self.atom().map(|n| Integer { bits: !n.bits, ..n }),
            "-" => self.atom()?.negate(),
            "+" => self.atom(),
            "defined" => {
                let parens = self.peek(0) == "(";
                self.i += usize::from(parens);
                let name = self.tokens.get(self.i)?.text;
                self.i += 1;
                if parens {
                    if self.peek(0) != ")" {
                        return None;
                    }
                    self.i += 1;
                }
                self.macros.defined(name).map(Integer::boolean)
            }
            _ if self.peek(0) == "(" => {
                let mut depth = 0;
                while self.i < self.tokens.len() {
                    match self.peek(0) {
                        "(" => depth += 1,
                        ")" => depth -= 1,
                        _ => {}
                    }
                    self.i += 1;
                    if depth == 0 {
                        break;
                    }
                }
                None
            }
            name => integer(name).or_else(|| {
                if name.starts_with(|c: char| c == '_' || c.is_alphabetic()) {
                    self.macros.number(name)
                } else {
                    None
                }
            }),
        }
    }
    fn expr(&mut self, min: usize) -> Option<Integer> {
        let mut left = self.atom();
        loop {
            let (op, width, priority) = match (self.peek(0), self.peek(1)) {
                ("|", "|") => ("||", 2, 1),
                ("&", "&") => ("&&", 2, 2),
                ("|", _) => ("|", 1, 3),
                ("^", _) => ("^", 1, 4),
                ("&", _) => ("&", 1, 5),
                ("=", "=") => ("==", 2, 6),
                ("!", "=") => ("!=", 2, 6),
                ("<", "=") => ("<=", 2, 7),
                (">", "=") => (">=", 2, 7),
                ("<", "<") => ("<<", 2, 8),
                (">", ">") => (">>", 2, 8),
                ("<", _) => ("<", 1, 7),
                (">", _) => (">", 1, 7),
                ("+", _) => ("+", 1, 9),
                ("-", _) => ("-", 1, 9),
                ("*", _) => ("*", 1, 10),
                ("/", _) => ("/", 1, 10),
                ("%", _) => ("%", 1, 10),
                _ => break,
            };
            if priority < min {
                break;
            }
            self.i += width;
            let right = self.expr(priority + 1);
            left = match (op, left, right) {
                ("&&", Some(n), _) | ("&&", _, Some(n)) if !n.truth() => {
                    Some(Integer::boolean(false))
                }
                ("||", Some(n), _) | ("||", _, Some(n)) if n.truth() => {
                    Some(Integer::boolean(true))
                }
                (_, Some(a), Some(b)) => a.op(op, b),
                _ => None,
            };
        }
        left
    }
}
