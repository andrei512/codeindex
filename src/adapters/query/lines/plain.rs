use super::{Lines, events};
use crate::adapters::{query::Outcome, repository};
use std::{
    io::{self, Write},
    path::Path,
    time::Instant,
};

impl Lines<'_> {
    pub(crate) fn plain(&self, cwd: &Path, outcome: &mut Outcome) -> io::Result<Vec<u8>> {
        let start = Instant::now();
        let mut stats = events::Stats::default();
        let mut out = Vec::new();
        let mut matched = false;
        let operands = if self.paths.is_empty() {
            &["."][..]
        } else {
            &self.paths
        };
        for operand in operands {
            let Some(path) = self.operand(cwd, operand, outcome) else {
                continue;
            };
            let file = path.is_file();
            let show_path = !file || operands.len() > 1;
            if file {
                if self.files {
                    writeln!(out, "{operand}")?;
                    matched = true;
                } else {
                    matched |= self.scan_bytes(
                        operand,
                        &std::fs::read(path)?,
                        show_path,
                        &mut out,
                        &mut stats,
                    );
                }
            } else {
                let capture = repository::CaptureRoot::new(&path)?;
                for entry in repository::plain_paths(&path, self.hidden)? {
                    let display = if self.paths.is_empty() {
                        Path::new(&entry.path).to_owned()
                    } else {
                        Path::new(operand).join(&entry.path)
                    };
                    let name = display.to_string_lossy();
                    if !self.includes(&name) {
                        continue;
                    }
                    if self.files {
                        writeln!(out, "{name}")?;
                        matched = true;
                    } else if self.text {
                        if let Some(bytes) = capture.bytes(&entry.path, entry.stamp)? {
                            matched |= self.scan(&name, &bytes, true, &mut out, &mut stats);
                        }
                    } else if let Some(body) = capture.capture(&entry.path, entry.stamp)? {
                        matched |= self.scan(&name, body.as_bytes(), true, &mut out, &mut stats);
                    }
                    if self.quiet && matched {
                        break;
                    }
                }
            }
            if self.quiet && matched {
                break;
            }
        }
        if self.json && !self.files && !self.quiet {
            stats.summary(start, &mut out);
        }
        self.finish(outcome, matched);
        Ok(out)
    }
}
