//! Encode JSON strings directly from UTF-8. Numeric fields are emitted by their owners.
use std::{borrow::BorrowMut, fmt::Write};

pub fn quote(text: &str) -> String {
    let mut result = String::with_capacity(text.len() + 2);
    quote_into(&mut result, text);
    result
}

pub fn quote_into(result: &mut String, text: &str) {
    result.push('"');
    let mut start = 0;
    for (i, byte) in text.bytes().enumerate() {
        if byte >= b' ' && !matches!(byte, b'"' | b'\\') {
            continue;
        }
        // ASCII escapes always lie on UTF-8 boundaries.
        result.push_str(&text[start..i]);
        escape_ascii(result, byte);
        start = i + 1;
    }
    result.push_str(&text[start..]);
    result.push('"');
}

fn escape_ascii(result: &mut String, byte: u8) {
    let escaped = match byte {
        b'"' => "\\\"",
        b'\\' => "\\\\",
        b'\n' => "\\n",
        b'\r' => "\\r",
        b'\t' => "\\t",
        _ => return write!(result, "\\u{byte:04x}").unwrap(),
    };
    result.push_str(escaped);
}
/// Build directly into the response buffer. Keys are constant field names.
pub(super) struct Object<B = String> {
    buffer: B,
    first: bool,
}

impl Object {
    pub fn new() -> Self {
        Self::open(String::new())
    }

    pub fn finish(self) -> String {
        self.close()
    }
}

impl<B: BorrowMut<String>> Object<B> {
    fn open(mut buffer: B) -> Self {
        buffer.borrow_mut().push('{');
        Self { buffer, first: true }
    }

    fn close(mut self) -> B {
        self.buffer.borrow_mut().push('}');
        self.buffer
    }

    fn field(&mut self, key: &str) -> &mut String {
        let out = self.buffer.borrow_mut();
        if !self.first {
            out.push(',');
        }
        self.first = false;
        out.push('"');
        out.push_str(key);
        out.push_str("\":");
        out
    }

    pub fn string(&mut self, key: &str, value: &str) {
        quote_into(self.field(key), value);
    }

    pub fn nullable_string(&mut self, key: &str, value: Option<&str>) {
        match value {
            Some(text) => self.string(key, text),
            None => self.raw(key, "null"),
        }
    }

    pub fn number(&mut self, key: &str, value: usize) {
        write!(self.field(key), "{value}").unwrap();
    }

    pub fn decimal(&mut self, key: &str, value: f64) {
        write!(self.field(key), "{value}").unwrap();
    }

    pub fn raw(&mut self, key: &str, encoded: &str) {
        self.field(key).push_str(encoded);
    }

}

/// Encode rows in order directly into the array that will be returned.
#[derive(Default)]
pub(super) struct ObjectArray {
    buffer: String,
    pub len: usize,
}

impl ObjectArray {
    pub fn push(&mut self, fields: impl FnOnce(&mut Object<&mut String>)) {
        self.buffer.push(if self.len == 0 { '[' } else { ',' });
        let mut row = Object::open(&mut self.buffer);
        fields(&mut row);
        row.close();
        self.len += 1;
    }

    pub fn finish(mut self) -> String {
        if self.len == 0 {
            self.buffer.push('[');
        }
        self.buffer.push(']');
        self.buffer
    }
}

pub fn object(fields: &[(&str, String)]) -> String {
    let mut out = Object::new();
    for (key, value) in fields {
        out.raw(key, value);
    }
    out.finish()
}
pub fn array(vals: impl IntoIterator<Item = String>) -> String {
    let mut out = String::from("[");
    for (i, v) in vals.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&v);
    }
    out.push(']');
    out
}
