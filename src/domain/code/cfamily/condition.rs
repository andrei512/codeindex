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
    // These operations have the same low 64 result bits in both integer
    // domains. A signed overflow makes the value unknown only in the signed domain.
    fn arithmetic((value, overflow): (i64, bool), unsigned: bool) -> Option<Self> {
        (unsigned || !overflow).then_some(Self {
            bits: value as u64,
            unsigned,
        })
    }

    fn negate(self) -> Option<Self> {
        Self::arithmetic((self.bits as i64).overflowing_neg(), self.unsigned)
    }
}
#[derive(Clone, Copy)]
enum Operator {
    Logical { decisive: bool },
    Integer(IntegerOperator),
}

#[derive(Clone, Copy)]
enum IntegerOperator {
    Bitwise(fn(u64, u64) -> u64),
    Compare(fn(std::cmp::Ordering) -> bool),
    Shift {
        signed: fn(i64, u32) -> Option<i64>,
        unsigned: fn(u64, u32) -> Option<u64>,
    },
    Arithmetic(fn(i64, i64) -> (i64, bool)),
    Quotient {
        signed: fn(i64, i64) -> Option<i64>,
        unsigned: fn(u64, u64) -> Option<u64>,
    },
}

// Indices into the infix grammar. Two-byte operators either repeat the
// first punctuation or append '='. The arrays are built at compile time.
struct Operators {
    single: [u8; 128],
    repeated: [u8; 128],
    equals: [u8; 128],
}
type Binding = (Operator, usize);
const SPELLINGS: &[(&[u8], Binding)] = {
    use IntegerOperator::*;
    use Operator::Integer as Op;
    use std::{
        cmp::Ordering,
        ops::{BitAnd, BitOr, BitXor},
    };
    &[
        (b"||", (Operator::Logical { decisive: true }, 1)),
        (b"&&", (Operator::Logical { decisive: false }, 2)),
        (b"!=", (Op(Compare(Ordering::is_ne)), 6)),
        (b"|", (Op(Bitwise(BitOr::bitor)), 3)),
        (b"^", (Op(Bitwise(BitXor::bitxor)), 4)),
        (b"&", (Op(Bitwise(BitAnd::bitand)), 5)),
        (b"==", (Op(Compare(Ordering::is_eq)), 6)),
        (b"<=", (Op(Compare(Ordering::is_le)), 7)),
        (b">=", (Op(Compare(Ordering::is_ge)), 7)),
        (
            b"<<",
            (
                Op(Shift {
                    signed: i64::checked_shl,
                    unsigned: u64::checked_shl,
                }),
                8,
            ),
        ),
        (
            b">>",
            (
                Op(Shift {
                    signed: i64::checked_shr,
                    unsigned: u64::checked_shr,
                }),
                8,
            ),
        ),
        (b"<", (Op(Compare(Ordering::is_lt)), 7)),
        (b">", (Op(Compare(Ordering::is_gt)), 7)),
        (b"+", (Op(Arithmetic(i64::overflowing_add)), 9)),
        (b"-", (Op(Arithmetic(i64::overflowing_sub)), 9)),
        (b"*", (Op(Arithmetic(i64::overflowing_mul)), 10)),
        (
            b"/",
            (
                Op(Quotient {
                    signed: i64::checked_div,
                    unsigned: u64::checked_div,
                }),
                10,
            ),
        ),
        (
            b"%",
            (
                Op(Quotient {
                    signed: i64::checked_rem,
                    unsigned: u64::checked_rem,
                }),
                10,
            ),
        ),
    ]
};
const OPERATORS: Operators = {
    let mut index = Operators {
        single: [u8::MAX; 128],
        repeated: [u8::MAX; 128],
        equals: [u8::MAX; 128],
    };
    let mut i = 0;
    while i < SPELLINGS.len() {
        match *SPELLINGS[i].0 {
            [a] => index.single[a as usize] = i as u8,
            [a, b'='] => index.equals[a as usize] = i as u8,
            [a, b] if a == b => index.repeated[a as usize] = i as u8,
            _ => panic!("unexpected infix spelling"),
        }
        i += 1;
    }
    index
};
impl Operator {
    fn single(byte: u8) -> Option<Binding> {
        SPELLINGS
            .get(*OPERATORS.single.get(usize::from(byte))? as usize)
            .map(|x| x.1)
    }
    fn pair([a, b]: [u8; 2]) -> Option<Binding> {
        let index = if b == b'=' {
            &OPERATORS.equals
        } else if a == b {
            &OPERATORS.repeated
        } else {
            return None;
        };
        SPELLINGS
            .get(*index.get(usize::from(a))? as usize)
            .map(|x| x.1)
    }
    fn short_circuits(self, left: Option<Integer>) -> bool {
        matches!(self,Self::Logical{decisive} if left.is_some_and(|n|n.truth()==decisive))
    }
    fn apply(
        self,
        left: Option<Integer>,
        right: Option<Integer>,
        evaluate: bool,
    ) -> Result<Option<Integer>, ()> {
        // A known zero denominator is an error even if another operand is unknown.
        // Only evaluated arithmetic can invalidate the preprocessor expression.
        if evaluate
            && matches!(self, Self::Integer(IntegerOperator::Quotient { .. }))
            && right.is_some_and(|n| !n.truth())
        {
            return Err(());
        }
        Ok(match self {
            Self::Logical { decisive } if evaluate => super::directives::logical(
                left.map(Integer::truth),
                right.map(Integer::truth),
                decisive,
            )
            .map(Integer::boolean),
            Self::Logical { .. } => left.and(right).map(|_| Integer::boolean(false)),
            Self::Integer(op) => left.zip(right).and_then(|(l, r)| op.apply(l, r, evaluate)),
        })
    }
}

impl IntegerOperator {
    fn apply(self, left: Integer, right: Integer, evaluate: bool) -> Option<Integer> {
        let (x, y) = (left.bits, right.bits);
        let promoted = left.unsigned || right.unsigned;
        // A skipped arm determines only the result type. The arithmetic
        // itself must not run, even for invalid shifts and zero divisors.
        let unsigned = self.result_unsigned(left, promoted);
        if !evaluate {
            return Some(Integer { unsigned, bits: 0 });
        }
        let bits = match self {
            Self::Compare(test) => u64::from(test(left.compare(right, promoted))),
            Self::Bitwise(op) => op(x, y),
            Self::Arithmetic(op) => return Integer::arithmetic(op(x as i64, y as i64), promoted),
            Self::Shift {
                signed,
                unsigned: shift,
            } => {
                let count = y.try_into().ok()?;
                left.signed_operation(unsigned, |v| signed(v, count), |v| shift(v, count))?
            }
            Self::Quotient {
                signed,
                unsigned: quotient,
            } => left.signed_operation(unsigned, |v| signed(v, y as i64), |v| quotient(v, y))?,
        };
        Some(Integer { unsigned, bits })
    }

    fn result_unsigned(self, left: Integer, promoted: bool) -> bool {
        match self {
            Self::Compare(_) => false,
            Self::Shift { .. } => left.unsigned,
            _ => promoted,
        }
    }
}

impl Integer {
    fn signed_operation(
        self,
        unsigned: bool,
        signed: impl FnOnce(i64) -> Option<i64>,
        natural: impl FnOnce(u64) -> Option<u64>,
    ) -> Option<u64> {
        if unsigned {
            natural(self.bits)
        } else {
            signed(self.bits as i64).map(|v| v as u64)
        }
    }
    fn compare(self, other: Self, promoted: bool) -> std::cmp::Ordering {
        if promoted {
            self.bits.cmp(&other.bits)
        } else {
            (self.bits as i64).cmp(&(other.bits as i64))
        }
    }
}
fn character_literal(s: &str) -> Option<Integer> {
    let code = match s.as_bytes() {
        [c] if c.is_ascii() => *c,
        [b'\\', ..] => escape_value(&s[1..])?,
        _ => return None,
    };
    (code < 128).then(|| Integer::signed(code.into()))
}

fn escape_value(escape: &str) -> Option<u8> {
    // Ordinary escape spellings have fixed execution-character values. Numeric
    // escapes instead use their full operand and must fit the supported byte.
    const ESCAPES: &[(u8, u8)] = &[
        (b'a', 7),
        (b'b', 8),
        (b't', b'\t'),
        (b'n', b'\n'),
        (b'v', 11),
        (b'f', 12),
        (b'r', b'\r'),
        (b'\\', b'\\'),
        (b'\'', b'\''),
        (b'"', b'"'),
        (b'?', b'?'),
    ];
    match escape.as_bytes() {
        [b'x', ..] => u8::from_str_radix(&escape[1..], 16).ok(),
        [b'0'..=b'7', ..] if escape.len() <= 3 => u8::from_str_radix(escape, 8).ok(),
        [c] => ESCAPES
            .iter()
            .find(|(written, _)| written == c)
            .map(|(_, value)| *value),
        _ => None,
    }
}
pub(super) fn integer(text: &str, separators: bool) -> Option<Integer> {
    if let Some(s) = text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return character_literal(s);
    }
    let n = super::number::Literal::read(text, separators)?;
    Some(Integer {
        bits: n.value,
        unsigned: n.explicit_unsigned() || n.value > i64::MAX as u64,
    })
}
pub(super) fn test(source: &str, tokens: &[Token], macros: &Macros) -> Option<bool> {
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
        source,
        tokens,
        i: 0,
        macros,
        depth: 0,
        values: true,
        valid: true,
    };
    let n = p.conditional();
    if p.valid && p.i == tokens.len() {
        n.map(Integer::truth)
    } else {
        None
    }
}
#[derive(Clone, Copy)]
struct Infix {
    op: Operator,
    width: usize,
    priority: usize,
}
fn operator_spelling(spelling: &str) -> Option<Binding> {
    match *spelling.as_bytes() {
        [byte] => Operator::single(byte),
        [a, b] => Operator::pair([a, b]),
        _ => None,
    }
}

fn written_operator(byte: u8, next: Option<u8>) -> Option<(usize, Binding)> {
    if let Some(pair) = next.and_then(|next| Operator::pair([byte, next])) {
        return Some((2, pair));
    }
    Operator::single(byte).map(|op| (1, op))
}

struct Expression<'a, 'm> {
    source: &'a str,
    tokens: &'a [Token<'a>],
    macros: &'m Macros,
    i: usize,
    depth: usize,
    values: bool,
    valid: bool,
}
impl<'a> Expression<'a, '_> {
    fn next_word(&mut self) -> Option<&'a str> {
        let Some(token) = self.tokens.get(self.i) else {
            self.valid = false;
            return None;
        };
        self.i += 1;
        Some(self.spelling(token.text))
    }
    fn spelling(&self, raw: &'a str) -> &'a str {
        const ALTERNATIVES: &[(&str, &str)] = &[
            ("and", "&&"),
            ("and_eq", "&="),
            ("bitand", "&"),
            ("bitor", "|"),
            ("compl", "~"),
            ("not", "!"),
            ("not_eq", "!="),
            ("or", "||"),
            ("or_eq", "|="),
            ("xor", "^"),
            ("xor_eq", "^="),
        ];
        if self.macros.cpp && (2..=6).contains(&raw.len()) {
            return ALTERNATIVES
                .iter()
                .find_map(|&(name, punct)| (raw == name).then_some(punct))
                .unwrap_or(raw);
        }
        raw
    }
    fn operator(&self) -> Option<Infix> {
        let first = self.tokens.get(self.i)?;
        let (width, (op, priority)) = match first.text.as_bytes() {
            &[byte] => written_operator(byte, self.source.as_bytes().get(first.span.end).copied())?,
            _ => (1, operator_spelling(self.spelling(first.text))?),
        };
        Some(Infix {
            op,
            width,
            priority,
        })
    }
    fn closing(&mut self, expected: &str) -> bool {
        let closed = self.take(expected);
        self.valid &= closed;
        closed
    }
    fn take(&mut self, word: &str) -> bool {
        if self
            .tokens
            .get(self.i)
            .is_none_or(|token| token.text != word)
        {
            return false;
        }
        self.i += 1;
        true
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
        let word = self.next_word()?;
        // An adjacent previous sign is the other half of ++ or --.
        // This checks arithmetic operands only; macro arguments may be opaque.
        if self.repeated_sign(word) {
            self.valid = false;
            return None;
        }
        match word {
            "(" => {
                let n = self.conditional();
                self.closing(")");
                n
            }
            "!" => self.atom().map(|n| Integer::boolean(!n.truth())),
            "~" => self.atom().map(|n| Integer { bits: !n.bits, ..n }),
            "+" | "-" => self.signed_operand(word),
            name => self.primary(name),
        }
    }

    fn repeated_sign(&self, word: &str) -> bool {
        if !matches!(word, "+" | "-") || self.i <= 1 {
            return false;
        }
        let at = self.tokens[self.i - 1].span.start;
        self.source.as_bytes().get(at - 1) == word.as_bytes().first()
    }

    fn signed_operand(&mut self, sign: &str) -> Option<Integer> {
        let negate = sign == "-" && self.values;
        let value = self.atom()?;
        if negate { value.negate() } else { Some(value) }
    }

    fn primary(&mut self, name: &str) -> Option<Integer> {
        if name == "defined" {
            return self.defined_operand();
        }
        if super::identifier_start(name) {
            if self.take("(") {
                self.skip_arguments();
                return None;
            }
            return self.macros.number(name);
        }
        let number = integer(name, self.macros.digit_separators);
        self.valid &= number.is_some() || name.starts_with('\'') && name.ends_with('\'');
        number
    }

    fn defined_operand(&mut self) -> Option<Integer> {
        let parens = self.take("(");
        let name = self.next_word()?;
        self.valid &= super::identifier_start(name);
        if parens {
            self.closing(")");
        }
        self.macros.defined(name).map(Integer::boolean)
    }

    fn skip_arguments(&mut self) {
        // Contents of an opaque invocation cannot establish a numeric value.
        // Only balanced delimiters are required to resume the enclosing expression.
        let mut depth = 1;
        for token in &self.tokens[self.i..] {
            self.i += 1;
            match token.text {
                "(" => depth += 1,
                ")" if depth == 1 => return,
                ")" => depth -= 1,
                _ => {}
            }
        }
        self.valid = false;
    }

    fn conditional(&mut self) -> Option<Integer> {
        let (condition, _) = self.expr(0);
        if !self.take("?") {
            return condition;
        }
        if self.depth >= 80 {
            self.valid = false;
            self.i = self.tokens.len();
            return None;
        }
        self.depth += 1;
        let evaluate = self.values;
        let selected = condition.map(Integer::truth);
        self.values = evaluate && selected == Some(true);
        let yes = self.conditional();
        let colon = self.closing(":");
        self.values = evaluate && selected == Some(false);
        let no = colon.then(|| self.conditional()).flatten();
        self.values = evaluate;
        self.depth -= 1;
        let (yes, no) = (yes?, no?);
        Some(Integer {
            bits: if selected? { yes.bits } else { no.bits },
            unsigned: yes.unsigned || no.unsigned,
        })
    }

    fn expr(&mut self, min: usize) -> (Option<Integer>, Option<Infix>) {
        let mut left = self.atom();
        // The first lower-precedence operator belongs to the caller. Return its
        // already-read spelling and precedence instead of classifying it again.
        let mut lookahead = self.operator();
        while let Some(Infix {
            op,
            width,
            priority,
        }) = lookahead
        {
            if priority < min {
                break;
            }
            self.i += width;
            let evaluate = self.values;
            self.values = evaluate && !op.short_circuits(left);
            let (right, next) = self.expr(priority + 1);
            lookahead = next;
            self.values = evaluate;
            left = op.apply(left, right, evaluate).unwrap_or_else(|()| {
                self.valid = false;
                None
            });
        }
        (left, lookahead)
    }
}
