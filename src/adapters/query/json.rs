//! Encode JSON strings directly from UTF-8. Numeric fields are emitted by their owners.
use std::fmt::Write;

pub fn quote(text: &str) -> String {
    let mut result = String::with_capacity(text.len() + 2);
    result.push('"');
    for c in text.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c < ' ' => {
                write!(result, "\\u{:04x}", c as u32).unwrap();
            }
            c => result.push(c),
        }
    }
    result.push('"');
    result
}
pub fn object(fields: &[(&str, String)]) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(k);
        out.push_str("\":");
        out.push_str(v);
    }
    out.push('}');
    out
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
