mod events;
use super::{CONTENT, Pin, Regex, candidates};
use std::{collections::BTreeSet, fmt::Write, io, path::Path};
use std::{ops::Range, time::Instant};

enum Pattern<'a> {
    Literal(&'a str),
    Regex(Regex),
}

#[derive(Default)]
pub struct Lines<'a> {
    patterns: Vec<Pattern<'a>>,
    paths: Vec<&'a str>,
    before: usize,
    after: usize,
    numbered: Option<bool>,
    filename: Option<bool>,
    column: bool,
    terminal: bool,
    heading: Option<bool>,
    invert: bool,
    word: bool,
    whole: bool,
    only: bool,
    json: bool,
    hidden: bool,
    color: Option<bool>,
    terminal_color: bool,
    files: bool,
    count: bool,
    list: bool,
    quiet: bool,
    max: Option<usize>,
    crlf: bool,
    globs: Vec<(bool, bool, super::Glob)>,
}

impl<'a> Lines<'a> {
    pub(super) fn parse(input: &[&'a str]) -> Result<Self, String> {
        let mut q = Self::default();
        let mut args = input.iter().copied();
        let (mut fixed, mut fold, mut smart, mut flags) = (false, false, false, true);
        let mut patterns = Vec::new();
        while let Some(arg) = args.next() {
            if !flags || !arg.starts_with('-') || arg == "-" {
                q.paths.push(arg);
                continue;
            }
            if arg == "--" {
                flags = false;
                continue;
            }
            let (name, inline) = arg
                .split_once('=')
                .map_or((arg, None), |(k, v)| (k, Some(v)));
            let short = match name {
                "--cx-terminal-color" => {
                    q.terminal_color = true;
                    continue;
                }
                "--color" => {
                    q.color = match inline.or_else(|| args.next()) {
                        Some("always" | "ansi") => Some(true),
                        Some("never") => Some(false),
                        Some("auto") => None,
                        _ => return Err("expected auto, always, ansi or never for --color".into()),
                    };
                    continue;
                }
                "--cx-terminal" => {
                    q.terminal = true;
                    continue;
                }
                "--json" => {
                    q.json = true;
                    continue;
                }
                "--hidden" => {
                    q.hidden = true;
                    continue;
                }
                "--word-regexp" => "w",
                "--line-regexp" => "x",
                "--only-matching" => "o",
                "--files" => {
                    q.files = true;
                    continue;
                }
                "--crlf" => {
                    q.crlf = true;
                    continue;
                }
                "--heading" => {
                    q.heading = Some(true);
                    continue;
                }
                "--no-heading" => {
                    q.heading = Some(false);
                    continue;
                }
                "--column" => {
                    q.column = true;
                    q.numbered = Some(true);
                    continue;
                }
                "--line-number" => "n",
                "--no-line-number" => "N",
                "--with-filename" => "H",
                "--no-filename" => "I",
                "--fixed-strings" => "F",
                "--ignore-case" => "i",
                "--smart-case" => "S",
                "--case-sensitive" => "s",
                "--files-with-matches" => "l",
                "--count" => "c",
                "--quiet" => "q",
                "--invert-match" => "v",
                "--after-context" => "A",
                "--before-context" => "B",
                "--context" => "C",
                "--max-count" => "m",
                "--regexp" => "e",
                "--glob" => "g",
                s if s.starts_with("--") => return Err(format!("unsupported option {s}")),
                _ => &arg[1..],
            };
            for (i, opt) in short.char_indices() {
                match opt {
                    'w' => q.word = true,
                    'x' => q.whole = true,
                    'o' => q.only = true,
                    'F' => fixed = true,
                    'i' => {
                        fold = true;
                        smart = false
                    }
                    's' => {
                        fold = false;
                        smart = false
                    }
                    'S' => smart = true,
                    'n' => q.numbered = Some(true),
                    'N' => q.numbered = Some(false),
                    'H' => q.filename = Some(true),
                    'I' => q.filename = Some(false),
                    'c' => q.count = true,
                    'l' => q.list = true,
                    'v' => q.invert = true,
                    'q' => q.quiet = true,
                    'A' | 'B' | 'C' | 'm' | 'e' | 'g' => {
                        let tail = &short[i + 1..];
                        let v = if !tail.is_empty() {
                            tail
                        } else {
                            inline
                                .or_else(|| args.next())
                                .ok_or_else(|| format!("missing argument for {arg}"))?
                        };
                        if opt == 'e' {
                            patterns.push(v);
                        } else if opt == 'g' {
                            let pat = v.strip_prefix('!').unwrap_or(v);
                            q.globs
                                .push((pat == v, pat.contains('/'), super::Glob::new(pat)?));
                        } else {
                            let n = v.parse().map_err(|_| format!("invalid number for {arg}"))?;
                            match opt {
                                'A' => q.after = n,
                                'B' => q.before = n,
                                'C' => {
                                    q.before = n;
                                    q.after = n
                                }
                                _ => q.max = Some(n),
                            }
                        }
                        break;
                    }
                    _ => return Err(format!("unsupported option -{opt}")),
                }
            }
        }
        if patterns.is_empty() && !q.files {
            if q.paths.is_empty() {
                return Err("missing pattern".into());
            }
            patterns.push(q.paths.remove(0));
        }
        if smart {
            fold = patterns.iter().all(|p| !p.chars().any(char::is_uppercase));
        }
        for p in patterns {
            q.patterns.push(if fixed && fold {
                Pattern::Regex(Regex::folded_literal(p)?)
            } else if fixed {
                Pattern::Literal(p)
            } else {
                Pattern::Regex(Regex::with_case_folding(p, fold)?)
            });
        }
        Ok(q)
    }

    fn eligible(&self, hay: &str, r: &Range<usize>) -> bool {
        use crate::domain::pattern::word_char;
        (!self.whole || r.start == 0 && r.end == hay.len())
            && (!self.word
                || (!hay[..r.start].chars().next_back().is_some_and(word_char)
                    && !hay[r.end..].chars().next().is_some_and(word_char)))
    }
    fn first(&self, hay: &str) -> Option<usize> {
        self.patterns
            .iter()
            .filter_map(|p| match p {
                Pattern::Literal(q) => hay
                    .match_indices(q)
                    .find(|(i, s)| self.eligible(hay, &(*i..*i + s.len())))
                    .map(|(i, _)| i),
                Pattern::Regex(r) => r
                    .matches(hay)
                    .find(|r| self.eligible(hay, r))
                    .map(|r| r.start),
            })
            .min()
    }
    fn ranges(&self, hay: &str) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        for p in &self.patterns {
            match p {
                Pattern::Literal(q) => ranges.extend(
                    hay.match_indices(q)
                        .map(|(i, s)| i..i + s.len())
                        .filter(|r| self.eligible(hay, r)),
                ),
                Pattern::Regex(re) => {
                    ranges.extend(re.matches(hay).filter(|r| self.eligible(hay, r)))
                }
            }
        }
        ranges.sort_by_key(|r| r.start);
        let mut next = 0;
        ranges.retain(|r| {
            if r.start < next {
                false
            } else {
                next = r.end.max(r.start + 1);
                true
            }
        });
        ranges
    }
    pub fn stdin_only(&self) -> bool {
        self.paths.as_slice() == ["-"] || self.paths.is_empty() && !self.files
    }
    pub fn explicit_stdin(&self) -> bool {
        self.paths.as_slice() == ["-"]
    }
    pub fn stream(&self, text: &str) -> (String, bool) {
        let mut out = String::new();
        let start = Instant::now();
        let mut stats = events::Stats::default();
        let hit = self.scan("<stdin>", text, false, &mut out, &mut stats);
        if self.json && !self.quiet {
            stats.summary(start, &mut out);
        }
        (out, hit)
    }
    fn scan(
        &self,
        path: &str,
        text: &str,
        show_path: bool,
        out: &mut String,
        stats: &mut events::Stats,
    ) -> bool {
        if self.json && !self.quiet {
            return events::search(self, path, text, out, stats);
        }
        let color = self.color.unwrap_or(self.terminal_color);
        let paint = |style: &str, value: &str| format!("\x1b[0m{style}{value}\x1b[0m");
        let colored_path;
        let path = if color {
            colored_path = paint("\x1b[35m", path);
            &colored_path
        } else {
            path
        };
        let show_path = self.filename.unwrap_or(show_path);
        let heading = show_path && self.heading.unwrap_or(self.terminal);
        let numbered = self.numbered.unwrap_or(self.terminal);
        let mut hits = 0usize;
        let mut after_end = 0;
        let mut emitted = 0;
        let mut previous = std::collections::VecDeque::new();
        let start = out.len();
        for (index, body) in text.split_terminator('\n').enumerate() {
            let line = index + 1;
            let hay = if self.crlf {
                body.strip_suffix('\r').unwrap_or(body)
            } else {
                body
            };
            let ranges = (self.only || color).then(|| self.ranges(hay));
            let pos = match &ranges {
                Some(r) => r.first().map(|r| r.start),
                None => self.first(hay),
            };
            let hit = (pos.is_some() != self.invert) && hits < self.max.unwrap_or(usize::MAX);
            if hit {
                hits += 1;
                if self.quiet {
                    return true;
                }
                if self.list {
                    writeln!(out, "{path}").unwrap();
                    return true;
                }
            }
            if self.count {
                continue;
            }
            let mut emit = |n: usize, s: &str, col: Option<usize>| {
                if emitted == 0 && heading {
                    if start > 0 {
                        out.push('\n');
                    }
                    writeln!(out, "{path}").unwrap();
                }
                if (self.before != 0 || self.after != 0)
                    && (emitted != 0 && n > emitted + 1 || emitted == 0 && start > 0 && !heading)
                {
                    out.push_str("--\n");
                }
                let sep = if col.is_some() { ':' } else { '-' };
                if show_path && !heading {
                    write!(out, "{path}{sep}").unwrap();
                }
                if numbered {
                    if color {
                        write!(out, "{}{sep}", paint("\x1b[32m", &n.to_string())).unwrap();
                    } else {
                        write!(out, "{n}{sep}").unwrap();
                    }
                }
                if self.column
                    && let Some(c) = col
                {
                    if color {
                        write!(out, "{}:", paint("", &c.to_string())).unwrap();
                    } else {
                        write!(out, "{c}:").unwrap();
                    }
                }
                writeln!(out, "{s}").unwrap();
                emitted = n;
            };
            if hit {
                for (n, s) in previous.drain(..) {
                    emit(n, s, None);
                }
                if self.only {
                    for r in ranges.unwrap().into_iter().filter(|r| !r.is_empty()) {
                        let value = &body[r.clone()];
                        if color {
                            emit(line, &paint("\x1b[1m\x1b[31m", value), Some(r.start + 1));
                        } else {
                            emit(line, value, Some(r.start + 1));
                        }
                    }
                } else if let Some(ranges) = ranges {
                    let mut styled = String::new();
                    let mut at = 0;
                    for r in ranges.into_iter().filter(|r| !r.is_empty()) {
                        styled.push_str(&body[at..r.start]);
                        styled.push_str(&paint("\x1b[1m\x1b[31m", &body[r.clone()]));
                        at = r.end;
                    }
                    styled.push_str(&body[at..]);
                    emit(line, &styled, Some(pos.unwrap_or(0) + 1));
                } else {
                    emit(line, body, Some(pos.unwrap_or(0) + 1));
                }
                after_end = line.saturating_add(self.after);
            } else if line <= after_end {
                emit(line, body, None);
            } else if self.before > 0 {
                if previous.len() == self.before {
                    previous.pop_front();
                }
                previous.push_back((line, body));
            }
            if hits == self.max.unwrap_or(usize::MAX) && line >= after_end {
                break;
            }
        }
        if self.count && hits > 0 {
            if show_path {
                write!(out, "{path}:").unwrap();
            }
            writeln!(out, "{hits}").unwrap();
        }
        hits > 0
    }
    pub(super) fn run(
        &self,
        pin: &Pin,
        cwd: &Path,
        outcome: &mut super::Outcome,
    ) -> io::Result<String> {
        let start = Instant::now();
        let mut stats = events::Stats::default();
        let mut ids = BTreeSet::new();
        if self.files || self.invert {
            ids.extend(0..pin.files.len());
        } else {
            for pattern in &self.patterns {
                let key = match pattern {
                    Pattern::Literal(p) => p,
                    Pattern::Regex(re) => re.required_literal(),
                };
                ids.extend(candidates(pin, CONTENT, key)?);
            }
        }
        let mut out = String::new();
        let mut matched = false;
        let paths = if self.paths.is_empty() {
            &["."][..]
        } else {
            self.paths.as_slice()
        };
        for operand in paths {
            let absolute = cwd.join(operand).canonicalize()?;
            let file = absolute.is_file();
            let show_path = !file || paths.len() > 1;
            let scope = absolute
                .strip_prefix(&pin.root)
                .map_err(|_| io::Error::other("path outside repository"))?;
            if file
                && pin
                    .files
                    .binary_search_by(|f| f.path().cmp(scope.to_str().unwrap_or("")))
                    .is_err()
            {
                if self.files {
                    writeln!(out, "{operand}").unwrap();
                    matched = true;
                } else {
                    let text = std::fs::read(&absolute)?;
                    if !text.contains(&0) {
                        matched |= self.scan(
                            operand,
                            &String::from_utf8_lossy(&text),
                            show_path,
                            &mut out,
                            &mut stats,
                        );
                    }
                }
                continue;
            }
            for &id in &ids {
                let entry = &pin.files[id];
                let Ok(suffix) = Path::new(entry.path()).strip_prefix(scope) else {
                    continue;
                };
                if !file
                    && !self.hidden
                    && suffix
                        .components()
                        .any(|c| c.as_os_str().as_encoded_bytes().starts_with(b"."))
                {
                    continue;
                }
                let name = if file {
                    Path::new(operand).to_owned()
                } else if self.paths.is_empty() {
                    suffix.to_owned()
                } else {
                    Path::new(operand).join(suffix)
                };
                let name = name.to_string_lossy();
                let basename = name.rsplit('/').next().unwrap_or(&name);
                let mut include = !self.globs.iter().any(|g| g.0);
                for (value, full, g) in &self.globs {
                    if g.is_match(if *full { &name } else { basename }) {
                        include = *value;
                    }
                }
                if !include {
                    continue;
                }
                if self.files {
                    writeln!(out, "{name}").unwrap();
                    matched = true;
                    continue;
                }
                if entry.bytes.is_none() {
                    continue;
                }
                matched |= self.scan(&name, &pin.source(id)?, show_path, &mut out, &mut stats);
                if self.quiet && matched {
                    break;
                }
            }
        }
        if self.json && !self.files && !self.quiet {
            stats.summary(start, &mut out);
        }
        outcome.code = u8::from(!matched);
        Ok(out)
    }
}
