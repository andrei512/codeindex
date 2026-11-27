//! Checked magnitude and explicit suffix of a written C or C++ integer.
pub(super) struct Literal<'a> {
    pub(super) value: u64,
    pub(super) suffix: &'a str,
}
impl<'a> Literal<'a> {
    pub(super) fn read(text: &'a str, separators: bool) -> Option<Self> {
        let digits = text.trim_end_matches(['l', 'L', 'u', 'U']);
        let suffix = &text[digits.len()..];
        if !valid_suffix(suffix) {
            return None;
        }
        let (radix, digits) = radix_digits(digits);
        // A radix prefix cannot be followed by a sign.
        if radix != 10 && digits.starts_with(['+', '-']) {
            return None;
        }
        let value = if separators && digits.as_bytes().contains(&b'\'') {
            separated_digits(digits, radix)?
        } else {
            u64::from_str_radix(digits, radix).ok()?
        };
        Some(Self { value, suffix })
    }
    pub(super) fn explicit_unsigned(&self) -> bool {
        self.suffix.contains(['u', 'U'])
    }
}

fn radix_digits(digits: &str) -> (u32, &str) {
    match digits.as_bytes() {
        [b'0', b'x' | b'X', ..] => (16, &digits[2..]),
        [b'0', b'b' | b'B', ..] => (2, &digits[2..]),
        [b'0', _, ..] => (8, digits),
        _ => (10, digits),
    }
}

fn valid_suffix(suffix: &str) -> bool {
    let length = suffix
        .strip_prefix(['u', 'U'])
        .or_else(|| suffix.strip_suffix(['u', 'U']))
        .unwrap_or(suffix);
    matches!(length, "" | "l" | "L" | "ll" | "LL")
}

fn separated_digits(digits: &str, radix: u32) -> Option<u64> {
    // Both neighbors of every separator must be digits in this radix.
    digits.split('\'').try_fold(0u64, |acc, part| {
        if part.is_empty() {
            return None;
        }
        part.bytes().try_fold(acc, |n, ch| {
            let digit = char::from(ch).to_digit(radix)?;
            n.checked_mul(u64::from(radix))?
                .checked_add(u64::from(digit))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::Literal;

    #[test]
    fn separator_requires_valid_digits_on_both_sides() {
        for (text, value) in [("0'7", 7), ("0x1'fU", 31), ("+2", 2)] {
            assert_eq!(Literal::read(text, true).unwrap().value, value);
        }
        for text in ["0x+2", "0x'F", "0b1'2", "1''2", "12'u", "1uUl"] {
            assert!(Literal::read(text, true).is_none(), "{text}");
        }
        assert!(Literal::read("1'2", false).is_none());
    }
}
