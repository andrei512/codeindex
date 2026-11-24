use super::Pin;
use std::{
    fmt::Write,
    io,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct Display {
    pub public: bool,
    pub json: bool,
    pub column: bool,
    pub before: usize,
    pub after: usize,
}

impl Display {
    pub fn parse(flags: &[String]) -> Result<Self, String> {
        let mut d = Self {
            public: true,
            ..Self::default()
        };
        let text_option = flags.iter().any(|s| s != "--json");
        let mut flags = flags.iter().map(String::as_str);
        while let Some(flag) = flags.next() {
            match flag {
                "--json" => d.json = true,
                "--column" => d.column = true,
                s if ["-A", "-B", "-C"].iter().any(|p| s.starts_with(p)) => {
                    let (opt, tail) = s.split_at(2);
                    let count = if tail.is_empty() {
                        flags.next().unwrap_or("")
                    } else {
                        tail
                    };
                    let n = count
                        .parse()
                        .map_err(|_| format!("expected a line count after {opt}"))?;
                    if opt != "-A" {
                        d.before = n;
                    }
                    if opt != "-B" {
                        d.after = n;
                    }
                }
                _ => return Err(format!("unknown indexed display option {flag}")),
            }
        }
        if d.json && text_option {
            return Err("--json cannot be combined with text display options".into());
        }
        Ok(d)
    }
}

pub fn relative(file: &Path, cwd: &Path) -> PathBuf {
    let mut base = cwd;
    let mut prefix = PathBuf::new();
    while !file.starts_with(base) {
        let Some(parent) = base.parent() else {
            return file.to_owned();
        };
        prefix.push("..");
        base = parent;
    }
    prefix.join(file.strip_prefix(base).unwrap())
}

pub fn render(
    pin: &Pin,
    cwd: &Path,
    mut sites: Vec<(usize, usize)>,
    d: &Display,
    ranked: bool,
) -> io::Result<String> {
    if !ranked {
        sites.sort_unstable();
    }
    let mut groups: Vec<(usize, usize, usize, Vec<(usize, usize)>)> = Vec::new();
    for (file, byte) in sites {
        let entry = &pin.files[file];
        let (line, col) = entry.line_column(byte);
        let start = line.saturating_sub(d.before).max(1);
        let len = entry.bytes.as_ref().map_or(0, |r| r.end - r.start);
        let total = entry.line_ends.len()
            + usize::from(entry.line_ends.last().copied().unwrap_or(0) < len as usize);
        let end = line.saturating_add(d.after).min(total.max(line));
        if !ranked
            && let Some((prev, _, last, anchors)) = groups.last_mut()
            && *prev == file
            && start <= last.saturating_add(1)
        {
            *last = end.max(*last);
            if anchors.last().map(|a| a.0) != Some(line) {
                anchors.push((line, col));
            }
        } else {
            groups.push((file, start, end, vec![(line, col)]));
        }
    }
    let mut out = String::new();
    for (i, (file, first, last, anchors)) in groups.into_iter().enumerate() {
        let entry = &pin.files[file];
        let path = relative(&pin.root.join(entry.path()), cwd);
        let start = entry
            .line_start(first)
            .ok_or_else(|| io::Error::other("missing source line"))?;
        let end = entry
            .line_ends
            .get(last - 1)
            .map(|n| *n as u64)
            .unwrap_or_else(|| entry.bytes.as_ref().map_or(0, |r| r.end - r.start));
        let text = pin.source_slice(file, start as u64..end)?;
        if i > 0 && (d.before > 0 || d.after > 0) {
            out.push_str("--\n");
        }
        for (j, body) in text.lines().enumerate() {
            let line = first + j;
            if let Ok(k) = anchors.binary_search_by_key(&line, |a| a.0) {
                write!(out, "{}:{line}:", path.display()).unwrap();
                if d.column {
                    write!(out, "{}:", anchors[k].1).unwrap();
                }
            } else {
                write!(out, "{}-{line}-", path.display()).unwrap();
            }
            writeln!(out, "{body}").unwrap();
        }
    }
    out.pop();
    Ok(out)
}
