//! Map increasing source offsets to one-based physical lines in one pass.

pub(super) struct LineCursor<'a> {
    bytes: &'a [u8],
    previous: usize,
    line: usize,
}

impl<'a> LineCursor<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            previous: 0,
            line: 1,
        }
    }

    pub(super) fn at(&mut self, offset: usize) -> usize {
        self.line += self.bytes[self.previous..offset]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        self.previous = offset;
        self.line
    }
}
