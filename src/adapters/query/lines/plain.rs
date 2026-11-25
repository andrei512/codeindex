use super::{Lines, Pin, events};
use crate::adapters::query::Outcome;
use std::{collections::BTreeSet, io, path::Path, time::Instant};

impl Lines<'_> {
    pub(crate) fn plain(&self, cwd: &Path, outcome: &mut Outcome) -> io::Result<Vec<u8>> {
        self.walk_search(cwd, outcome, None)
    }
    pub(super) fn walk_search(
        &self,
        cwd: &Path,
        outcome: &mut Outcome,
        index: Option<(&Pin, Option<&BTreeSet<usize>>)>,
    ) -> io::Result<Vec<u8>> {
        let start = Instant::now();
        let mut stats = events::Stats::default();
        let mut out = Vec::new();
        let mut matched = false;
        let mut searched = 0;
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
            let capture = if !file && !self.walk.follow {
                Some(crate::adapters::repository::CaptureRoot::new(&path)?)
            } else {
                None
            };
            let search = |absolute: &Path,
                          name: &str,
                          stamp: Option<[u64; 7]>,
                          out: &mut Vec<u8>,
                          stats: &mut events::Stats|
             -> io::Result<bool> {
                if self.files {
                    if !self.quiet {
                        self.print_path(out, name, b"\n");
                    }
                    return Ok(true);
                }
                if self.transform.applies(name) {
                    return Ok(self.scan_input(
                        name,
                        &self.transform.read(absolute, cwd)?,
                        show_path,
                        file,
                        out,
                        stats,
                    ));
                }
                if let Some((pin, ids)) = index
                    && let Some(relative) =
                        absolute.strip_prefix(&pin.root).ok().and_then(Path::to_str)
                    && let Ok(id) = pin
                        .files
                        .binary_search_by(|entry| entry.path().cmp(relative))
                    && stamp.is_none_or(|s| s == pin.files[id].stamp())
                    && pin.files[id].bytes.is_some()
                {
                    if ids.is_some_and(|ids| !ids.contains(&id)) {
                        return Ok(false);
                    }
                    return Ok(self.scan_input(
                        name,
                        pin.source(id)?.as_bytes(),
                        show_path,
                        file,
                        out,
                        stats,
                    ));
                }
                let bytes = if let (Some(root), Some(stamp)) = (&capture, stamp) {
                    let relative = absolute
                        .strip_prefix(&path)
                        .ok()
                        .and_then(Path::to_str)
                        .ok_or_else(|| io::Error::other("invalid search path"))?;
                    let Some(bytes) = root.bytes(relative, stamp)? else {
                        return Ok(false);
                    };
                    bytes
                } else {
                    std::fs::read(absolute)?
                };
                Ok(self.scan_input(name, &bytes, show_path, file, out, stats))
            };
            if file {
                searched += 1;
                matched |= search(&path, operand, None, &mut out, &mut stats)?;
            } else {
                let mut entries =
                    self.walk
                        .paths(&path, cwd, self.hidden, self.scope.depth, |n| {
                            self.glob_override(n)
                        })?;
                self.scope.order(&mut entries, &path, |entry| &entry.name);
                let scan_entries =
                    |entries: &[crate::adapters::repository::search_paths::SearchPath],
                     out: &mut Vec<u8>,
                     stats: &mut events::Stats|
                     -> io::Result<(usize, bool)> {
                        let mut count = 0;
                        let mut hit = false;
                        for entry in entries {
                            if !self.scope.accepts(Path::new(&entry.name), entry.stamp[2]) {
                                continue;
                            }
                            let display = if self.paths.is_empty() {
                                Path::new(&entry.name).to_owned()
                            } else {
                                Path::new(operand).join(&entry.name)
                            };
                            let name = display.to_string_lossy();
                            if self.includes(&name) {
                                count += 1;
                                hit |= search(
                                    &path.join(&entry.name),
                                    &name,
                                    Some(entry.stamp),
                                    out,
                                    stats,
                                )?;
                                if self.quiet && !self.stats && hit {
                                    break;
                                }
                            }
                        }
                        Ok((count, hit))
                    };
                let workers = self.threads.min(entries.len());
                if workers > 1
                    && !self.heading.unwrap_or(self.terminal)
                    && self.before == 0
                    && self.after == 0
                    && !self.quiet
                {
                    std::thread::scope(|scope| -> io::Result<()> {
                        let mut jobs = Vec::new();
                        for chunk in entries.chunks(entries.len().div_ceil(workers)) {
                            let scan = &scan_entries;
                            jobs.push(scope.spawn(move || {
                                let mut out = Vec::new();
                                let mut stats = events::Stats::default();
                                scan(chunk, &mut out, &mut stats)
                                    .map(|(count, hit)| (count, hit, out, stats))
                            }));
                        }
                        for job in jobs {
                            let (count, hit, bytes, part) = job
                                .join()
                                .map_err(|_| io::Error::other("search worker panicked"))??;
                            searched += count;
                            matched |= hit;
                            out.extend(bytes);
                            stats.add(&part);
                        }
                        Ok(())
                    })?;
                } else {
                    let (count, hit) = scan_entries(&entries, &mut out, &mut stats)?;
                    searched += count;
                    matched |= hit;
                }
            }
            if self.quiet && !self.stats && matched {
                break;
            }
        }
        self.summary(&stats, start, &mut out);
        self.finish(outcome, matched);
        if self.no_messages {
            outcome.diagnostics.clear();
        } else if self.paths.is_empty() && searched == 0 && !self.files && !self.quiet {
            outcome.diagnostics.push_str(crate::adapters::cli::name());
            outcome.diagnostics.push_str(": No files were searched, which means ripgrep probably applied a filter you didn't expect.\nRunning with --debug will show why files are being skipped.\n");
        }
        Ok(out)
    }
}
