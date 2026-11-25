use super::{Lines, events};
use crate::adapters::query::Outcome;
use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
};

pub(super) fn unescape(value: &str) -> Vec<u8> {
    let mut bytes = value.bytes().peekable();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        let escaped = match bytes.peek() {
            Some(b'n') => b'\n',
            Some(b'r') => b'\r',
            Some(b't') => b'\t',
            Some(b'0') => 0,
            Some(b'\\') => b'\\',
            Some(b'x') => {
                let mut hex = bytes.clone();
                hex.next();
                if let (Some(hi), Some(lo)) = (hex.next(), hex.next())
                    && let (Some(hi), Some(lo)) =
                        ((hi as char).to_digit(16), (lo as char).to_digit(16))
                {
                    decoded.push((hi * 16 + lo) as u8);
                    bytes = hex;
                    continue;
                }
                decoded.push(byte);
                continue;
            }
            _ => {
                decoded.push(byte);
                continue;
            }
        };
        bytes.next();
        decoded.push(escaped);
    }
    decoded
}

pub(super) fn separator(value: &str) -> Result<Option<u8>, String> {
    let decoded = unescape(value);
    match decoded.as_slice() {
        [] => Ok(None),
        [byte] => Ok(Some(*byte)),
        _ => Err(format!(
            "error parsing flag --path-separator: A path separator must be exactly one byte, but the given separator is {} bytes: {value}\nIn some shells on Windows '/' is automatically expanded. Use '//' instead.",
            decoded.len()
        )),
    }
}

impl Lines<'_> {
    pub(super) fn print_path(&self, out: &mut Vec<u8>, path: &str, suffix: &[u8]) {
        let start = out.len();
        out.extend_from_slice(path.as_bytes());
        if let Some(separator) = self.path_separator.filter(|&c| c != b'/') {
            for byte in &mut out[start..] {
                if *byte == b'/' {
                    *byte = separator;
                }
            }
        }
        if self.null_path {
            out.push(0);
        } else {
            out.extend_from_slice(suffix);
        }
    }

    pub(super) fn operand(&self, cwd: &Path, name: &str, outcome: &mut Outcome) -> Option<PathBuf> {
        match cwd.join(name).canonicalize() {
            Ok(path) => Some(path),
            Err(error) => {
                if self.paths.len() == 1 {
                    writeln!(
                        outcome.diagnostics,
                        "cx: {name}: IO error for operation on {name}: {error}"
                    )
                    .unwrap();
                } else {
                    writeln!(outcome.diagnostics, "cx: {name}: {error}").unwrap();
                }
                None
            }
        }
    }

    pub(super) fn finish(&self, outcome: &mut Outcome, matched: bool) {
        outcome.code = if !outcome.diagnostics.is_empty() && !(self.quiet && matched) {
            2
        } else {
            u8::from(!matched)
        };
    }

    pub(super) fn scan_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        show_path: bool,
        out: &mut Vec<u8>,
        stats: &mut events::Stats,
    ) -> bool {
        self.scan_input(name, bytes, show_path, true, out, stats)
    }

    pub(super) fn scan_input(
        &self,
        name: &str,
        bytes: &[u8],
        show_path: bool,
        explicit: bool,
        out: &mut Vec<u8>,
        stats: &mut events::Stats,
    ) -> bool {
        let decoded = self.encoding.decode(bytes);
        let bytes = decoded.as_ref();
        if self.text || self.null_data {
            return self.scan(name, bytes, show_path, out, stats);
        }
        let Some(offset) = bytes.iter().position(|&b| b == 0) else {
            return self.scan(name, bytes, show_path, out, stats);
        };
        // A buffered search can emit complete records from earlier reads before
        // it observes the first NUL. The read containing the NUL is withheld.
        let block_start = offset / (64 * 1024) * (64 * 1024);
        let prior = bytes[..block_start]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        if !explicit && !self.binary {
            if self.count || self.without || prior == 0 {
                return false;
            }
            let hit = self.scan(name, &bytes[..prior], show_path, out, stats);
            if hit && !self.quiet && !self.list {
                if self.filename.unwrap_or(show_path) {
                    self.print_path(out, name, b": ");
                }
                writeln!(out,"WARNING: stopped searching binary file after match (found \"\\0\" byte around offset {offset})").unwrap();
            }
            return hit;
        }
        if self.count || self.without || self.list || self.quiet || self.json {
            let mut lines = bytes.to_vec();
            for b in &mut lines {
                if *b == 0 {
                    *b = b'\n';
                }
            }
            return self.scan(name, &lines, show_path, out, stats);
        }
        if prior != 0 {
            self.scan(name, &bytes[..prior], show_path, out, stats);
        }
        let hit = self.max != Some(0)
            && bytes
                .split_inclusive(|&b| b == b'\n' || b == 0)
                .any(|line| {
                    let hay = String::from_utf8_lossy(
                        line.strip_suffix(b"\n")
                            .or_else(|| line.strip_suffix(b"\0"))
                            .unwrap_or(line),
                    );
                    self.first(&hay).is_some() != self.invert
                });
        if hit {
            if self.filename.unwrap_or(show_path) {
                self.print_path(out, name, b": ");
            }
            writeln!(
                out,
                "binary file matches (found \"\\0\" byte around offset {offset})"
            )
            .unwrap();
        }
        hit
    }
}
