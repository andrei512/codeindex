//! Delimiters shared by literal Make references and PGXS expressions.

#[derive(Clone, Copy)]
pub(super) enum Delimiter {
    Paren,
    Brace,
}

impl Delimiter {
    pub(super) fn open(text: &str) -> Option<(Self, &str)> {
        match text.as_bytes().first()? {
            b'(' => Some((Self::Paren, &text[1..])),
            b'{' => Some((Self::Brace, &text[1..])),
            _ => None,
        }
    }

    fn bytes(self) -> (u8, u8) {
        match self {
            Self::Paren => (b'(', b')'),
            Self::Brace => (b'{', b'}'),
        }
    }

    pub(super) fn close(self, text: &str) -> Option<&str> {
        text.strip_prefix(char::from(self.bytes().1))
    }

    pub(super) fn payload(self, rest: &str) -> Option<(&str, &str)> {
        let (open, close) = self.bytes();
        let mut depth: usize = 1;
        let end = rest.bytes().position(|byte| {
            match byte {
                b if b == open => depth += 1,
                b if b == close => depth -= 1,
                _ => {}
            }
            depth == 0
        })?;
        Some((&rest[..end], &rest[end + 1..]))
    }
}
