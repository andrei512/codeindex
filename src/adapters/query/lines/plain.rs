use super::{Lines, events};
use crate::adapters::{query::Outcome, repository};
use std::{fmt::Write, io, path::Path, time::Instant};

impl Lines<'_> {
    pub(crate) fn plain(&self, cwd: &Path, outcome: &mut Outcome) -> io::Result<String> {
        let start = Instant::now();
        let mut stats = events::Stats::default();
        let mut out = String::new();
        let mut matched = false;
        let operands = if self.paths.is_empty() {
            &["."][..]
        } else {
            &self.paths
        };
        for operand in operands {
            let path = cwd.join(operand).canonicalize()?;
            let file = path.is_file();
            let mut search = |name: &str,
                              load: &mut dyn FnMut() -> io::Result<Option<String>>|
             -> io::Result<bool> {
                if !self.includes(name) {
                    return Ok(false);
                }
                if self.files {
                    writeln!(out, "{name}").unwrap();
                    return Ok(true);
                }
                Ok(load()?.is_some_and(|body| {
                    self.scan(
                        name,
                        &body,
                        !file || operands.len() > 1,
                        &mut out,
                        &mut stats,
                    )
                }))
            };
            if file {
                let root = path.parent().unwrap();
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| io::Error::other("invalid file name"))?;
                matched |= search(operand, &mut || repository::capture(root, name))?;
            } else {
                let capture = repository::CaptureRoot::new(&path)?;
                for entry in repository::plain_paths(&path, self.hidden)? {
                    let display = if self.paths.is_empty() {
                        Path::new(&entry.path).to_owned()
                    } else {
                        Path::new(operand).join(&entry.path)
                    };
                    let hit = search(&display.to_string_lossy(), &mut || {
                        capture.capture(&entry.path, entry.stamp)
                    })?;
                    matched |= hit;
                    if self.quiet && hit {
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
        outcome.code = u8::from(!matched);
        Ok(out)
    }
}
