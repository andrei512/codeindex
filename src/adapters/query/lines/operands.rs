use super::{Lines, events};
use crate::adapters::query::Outcome;
use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
};

impl Lines<'_> {
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
        if self.text {
            return self.scan(name, bytes, show_path, out, stats);
        }
        let Some(offset) = bytes.iter().position(|&b| b == 0) else {
            return self.scan(name, bytes, show_path, out, stats);
        };
        if self.count || self.list || self.quiet || self.json {
            let mut lines = bytes.to_vec();
            for b in &mut lines {
                if *b == 0 {
                    *b = b'\n';
                }
            }
            return self.scan(name, &lines, show_path, out, stats);
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
                write!(out, "{name}: ").unwrap();
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
