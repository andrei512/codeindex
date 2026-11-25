mod color;
mod encoding;
mod events;
mod multiline;
mod operands;
pub(crate) mod pcre;
mod plain;
mod replace;
mod scope;
mod transform;
mod types;
use super::{CONTENT, Pin, Regex, candidates};
use std::{
    collections::BTreeSet,
    io::{self, Write},
    path::Path,
};
use std::{ops::Range, time::Instant};

enum Pattern<'a> {
    Literal(&'a str),
    Regex(Regex),
    Pcre(pcre::Pcre),
}

#[derive(Default)]
pub struct Lines<'a> {
    patterns: Vec<Pattern<'a>>,
    paths: Vec<&'a str>,
    before: usize,
    after: usize,
    numbered: Option<bool>,
    filename: Option<bool>,
    path_separator: Option<u8>,
    scope: scope::Scope,
    walk: crate::adapters::repository::search_paths::SearchRules<'a>,
    no_messages: bool,
    encoding: encoding::Encoding,
    binary: bool,
    mapped: bool,
    transform: transform::Transform<'a>,
    vimgrep: bool,
    byte_offset: bool,
    null_path: bool,
    pattern_stdin: bool,
    count_matches: bool,
    include_zero: bool,
    without: bool,
    trim: bool,
    replacement_text: Option<&'a str>,
    max_columns: Option<usize>,
    preview: bool,
    passthru: bool,
    stop_on_nonmatch: bool,
    match_separator: Vec<u8>,
    context_field: Vec<u8>,
    context_separator: Option<Vec<u8>>,

    column: bool,
    terminal: bool,
    heading: Option<bool>,
    invert: bool,
    word: bool,
    whole: bool,
    multiline: bool,
    dotall: bool,
    null_data: bool,
    only: bool,
    json: bool,
    hidden: bool,
    text: bool,
    types: types::Types,
    color: Option<bool>,
    colors: color::Colors,
    terminal_color: bool,
    files: bool,
    count: bool,
    list: bool,
    quiet: bool,
    stats: bool,
    threads: usize,
    ascii: bool,
    byte_input: bool,
    max: Option<usize>,
    crlf: bool,
    globs: Vec<(bool, bool, super::Glob)>,
}

impl<'a> Lines<'a> {
    pub(super) fn parse(input: &[&'a str]) -> Result<Self, String> {
        let mut q = Self {
            match_separator: b":".to_vec(),
            context_field: b"-".to_vec(),
            context_separator: Some(b"--".to_vec()),
            ..Self::default()
        };
        let (mut before, mut after) = (None, None);
        let mut args = input.iter().copied();
        let (mut fixed, mut fold, mut smart, mut flags) = (false, false, false, true);
        let mut patterns = Vec::new();
        let mut edits = Vec::new();
        let mut selections = Vec::new();
        let mut list_types = false;
        let mut pattern_file = false;
        let mut unrestricted = 0;
        let mut engine = "default";
        let mut regex_limit = None;
        let mut glob_case = false;
        let mut glob_specs = Vec::new();
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
                "--cx-pattern-file" => { pattern_file = true; continue; }
                "--cx-pattern-stdin" => { q.pattern_stdin = true; continue; }
                "--no-config" => continue,
                "--cx-terminal-color" => {
                    q.terminal_color = true;
                    continue;
                }
                "--multiline" => "U",
                "--no-multiline" => { q.multiline = false; continue; }
                "--multiline-dotall" | "--no-multiline-dotall" => { q.dotall = name == "--multiline-dotall"; continue; }
                "--null-data" | "--no-null-data" => { q.null_data = name == "--null-data"; continue; }
                "--sort-files" => { q.scope.set_sort("path", false)?; continue; }
                "--sort" | "--sortr" | "--maxdepth" | "--max-depth" | "--max-filesize" => {
                    let value = inline.or_else(|| args.next()).ok_or_else(|| format!("missing argument for {name}"))?;
                    match name {
                        "--sort" | "--sortr" => q.scope.set_sort(value, name == "--sortr")?,
                        "--max-filesize" => q.scope.size = Some(scope::size(value)?),
                        _ => q.scope.depth = Some(value.parse().map_err(|_| format!("invalid depth: {value}"))?),
                    }
                    continue;
                }
                "--byte-offset" => "b",
                "--no-byte-offset" => { q.byte_offset = false; continue; }
                "--no-column" => { q.column = false; continue; }
                "--vimgrep" => {
                    q.vimgrep = true; q.column = true; q.numbered = Some(true);
                    q.filename = Some(true); q.heading = Some(false); continue;
                }
                "--search-zip" => "z",
                "--no-search-zip" => { q.transform.compressed = false; continue; }
                "--pre" | "--pre-glob" => {
                    let value = inline.or_else(||args.next()).ok_or_else(||format!("missing argument for {name}"))?;
                    if name == "--pre" { q.transform.program = (!value.is_empty()).then_some(value); }
                    else {
                        let glob = value.strip_prefix('!').unwrap_or(value);
                        q.transform.globs.push((glob == value, super::Glob::new(glob)?));
                    }
                    continue;
                }
                "--no-pre" => { q.transform.program = None; continue; }
                "--mmap" | "--no-mmap" => { q.mapped = name == "--mmap"; continue; }
                "--binary" | "--no-binary" => { q.binary = name == "--binary"; q.text = false; continue; }
                "--encoding" => "E",
                "--no-encoding" => { q.encoding = encoding::Encoding::None; continue; }
                "--no-messages" | "--messages" => { q.no_messages = name == "--no-messages"; continue; }
                "--unrestricted" => "u",
                "--regex-size-limit" => {
                    let value = inline.or_else(|| args.next()).ok_or("missing regex size limit")?;
                    regex_limit = Some(scope::size(value)?);
                    continue;
                }
                "--pretty" => "p",
                "--no-hidden" => { q.hidden = false; continue; }
                "--one-file-system" | "--no-one-file-system" => { q.walk.one_device = name == "--one-file-system"; continue; }
                "--no-ignore-global" | "--ignore-global" => { q.walk.no_global = name == "--no-ignore-global"; continue; }
                "--follow" => "L",
                "--no-follow" => { q.walk.follow = false; continue; }
                "--no-ignore" | "--ignore" => { q.walk.no_ignore = name == "--no-ignore"; continue; }
                "--no-ignore-parent" | "--ignore-parent" => { q.walk.no_parent = name == "--no-ignore-parent"; continue; }
                "--no-ignore-dot" | "--ignore-dot" => { q.walk.no_dot = name == "--no-ignore-dot"; continue; }
                "--no-ignore-vcs" | "--ignore-vcs" => { q.walk.no_vcs = name == "--no-ignore-vcs"; continue; }
                "--no-ignore-exclude" | "--ignore-exclude" => { q.walk.no_exclude = name == "--no-ignore-exclude"; continue; }
                "--no-ignore-files" | "--ignore-files" => { q.walk.no_files = name == "--no-ignore-files"; continue; }
                "--no-require-git" | "--require-git" => { q.walk.no_require_git = name == "--no-require-git"; continue; }
                "--ignore-file-case-insensitive" | "--no-ignore-file-case-insensitive" => { q.walk.fold = name == "--ignore-file-case-insensitive"; continue; }
                "--ignore-file" => {q.walk.files.push(inline.or_else(||args.next()).ok_or("missing ignore file")?);continue;}
                "--replace" => "r",
                "--max-columns" => "M",
                "--max-columns-preview" | "--no-max-columns-preview" => {q.preview = name=="--max-columns-preview"; continue;}
                "--stats" | "--no-stats" => {q.stats=name=="--stats";continue;}
                "--threads" => "j",
                "--no-unicode" | "--unicode" => {q.ascii=name=="--no-unicode";continue;}
                "--dfa-size-limit" => {
                    scope::size(inline.or_else(|| args.next()).ok_or("missing size")?)?;
                    // cx uses a bounded NFA and allocates no DFA cache.
                    continue;
                }
                "--engine" => {engine=inline.or_else(||args.next()).ok_or("missing regex engine")?;
                    if !matches!(engine,"default"|"auto"|"pcre2"){return Err(format!("unknown regex engine: {engine}"));}continue;}
                "--pcre2" => "P",
                "--auto-hybrid-regex" => {engine="auto";continue;}
                "--no-auto-hybrid-regex"|"--no-pcre2" => {engine="default";continue;}
                "--no-pcre2-unicode" => {q.ascii=true;continue;}
                "--pcre2-unicode" => {q.ascii=false;continue;}
                "--null" => "0",
                "--no-null" => { q.null_path = false; continue; }
                "--files-without-match" => { q.without = true; q.list = false; q.count = false; continue; }
                "--count-matches" => { q.count_matches = true; q.count = true; q.list = false; q.without = false; continue; }
                "--include-zero" | "--no-include-zero" => { q.include_zero = name == "--include-zero"; continue; }
                "--trim" | "--no-trim" => { q.trim = name == "--trim"; continue; }
                "--stop-on-nonmatch" | "--no-stop-on-nonmatch" => { q.stop_on_nonmatch = name == "--stop-on-nonmatch"; continue; }
                "--passthru" | "--passthrough" => { q.passthru = true; before = None; after = None; q.before = 0; q.after = 0; continue; }
                "--no-context-separator" => { q.context_separator = None; continue; }
                "--context-separator" | "--field-context-separator" | "--field-match-separator" => {
                    let value = inline.or_else(|| args.next()).ok_or_else(|| format!("missing argument for {name}"))?;
                    let bytes = operands::unescape(value);
                    match name {
                        "--context-separator" => q.context_separator = Some(bytes),
                        "--field-context-separator" => q.context_field = bytes,
                        _ => q.match_separator = bytes,
                    }
                    continue;
                }
                "--path-separator" => {
                    let value = inline.or_else(|| args.next()).ok_or("missing argument for --path-separator")?;
                    q.path_separator = operands::separator(value)?;
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
                "--iglob" => {glob_specs.push((inline.or_else(||args.next()).ok_or("missing glob")?,true));continue;}
                "--glob-case-insensitive" | "--no-glob-case-insensitive" => {glob_case = name=="--glob-case-insensitive";continue;}
                "--colors" => {q.colors.set(inline.or_else(||args.next()).ok_or("missing color")?)?; continue;}
                "--hyperlink-format" if inline == Some("") => continue,
                "--glob" => "g",
                "--type" => "t",
                "--type-not" => "T",
                "--type-list" => { list_types = true; continue; }
                "--type-add" | "--type-clear" => {
                    let value = inline.or_else(|| args.next()).ok_or_else(|| format!("missing argument for {arg}"))?;
                    edits.push(if name == "--type-add" { types::Edit::Add(value) } else { types::Edit::Clear(value) });
                    continue;
                }
                "--text" => "a",
                "--include" => return Err("unrecognized flag --include\n\nsimilar flags that are available: --include-zero".into()),
                s if s.starts_with("--") => return Err(format!("unrecognized flag {s}")),
                _ => &arg[1..],
            };
            for (i, opt) in short.char_indices() {
                match opt {
                    'p' => {
                        q.color = Some(true);
                        q.heading = Some(true);
                        q.numbered = Some(true);
                    }
                    'L' => q.walk.follow = true,
                    'u' => {
                        unrestricted += 1;
                        match unrestricted {
                            1 => q.walk.no_ignore = true,
                            2 => q.hidden = true,
                            _ => q.binary = true,
                        }
                    }
                    'z' => q.transform.compressed = true,
                    'P' => engine = "pcre2",
                    'b' => q.byte_offset = true,
                    'U' => q.multiline = true,
                    '0' => q.null_path = true,
                    'a' => q.text = true,
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
                    'c' => {
                        q.count = true;
                        q.count_matches = false;
                        q.list = false;
                        q.without = false;
                    }
                    'l' => {
                        q.list = true;
                        q.count = false;
                        q.without = false;
                    }
                    'v' => q.invert = true,
                    'q' => q.quiet = true,
                    'A' | 'B' | 'C' | 'm' | 'e' | 'g' | 't' | 'T' | 'E' | 'r' | 'M' | 'j' | 'd' => {
                        let tail = &short[i + 1..];
                        let v = if !tail.is_empty() {
                            tail
                        } else {
                            inline
                                .or_else(|| args.next())
                                .ok_or_else(|| format!("missing argument for {arg}"))?
                        };
                        if opt == 'j' {
                            q.threads = v
                                .parse()
                                .map_err(|_| format!("invalid thread count: {v}"))?;
                        } else if opt == 'r' {
                            q.replacement_text = Some(v);
                        } else if opt == 'M' {
                            q.max_columns =
                                Some(v.parse().map_err(|_| format!("invalid max columns: {v}"))?);
                        } else if opt == 'E' {
                            q.encoding = encoding::Encoding::parse(v)?;
                        } else if opt == 'e' {
                            patterns.push(v);
                        } else if matches!(opt, 't' | 'T') {
                            selections.push((opt == 't', v));
                        } else if opt == 'g' {
                            glob_specs.push((v, false));
                        } else {
                            let n = v.parse().map_err(|_| format!("invalid number for {arg}"))?;
                            match opt {
                                'd' => q.scope.depth = Some(n),
                                'A' => {
                                    after = Some(n);
                                    q.passthru = false;
                                }
                                'B' => {
                                    before = Some(n);
                                    q.passthru = false;
                                }
                                'C' => {
                                    q.passthru = false;
                                    q.before = n;
                                    q.after = n
                                }
                                _ => q.max = Some(n),
                            }
                        }
                        break;
                    }
                    _ => return Err(format!("unrecognized flag -{opt}")),
                }
            }
        }
        q.before = before.unwrap_or(q.before);
        q.after = after.unwrap_or(q.after);
        for (value, fold) in glob_specs {
            let pat = value.strip_prefix('!').unwrap_or(value);
            q.globs.push((
                pat == value,
                pat.contains('/'),
                super::Glob::with_case(pat.trim_start_matches('/'), fold || glob_case)?,
            ));
        }
        q.types = types::Types::build(edits, selections, list_types)?;
        if list_types {
            return Ok(q);
        }
        if patterns.is_empty() && !q.files && !pattern_file {
            if q.paths.is_empty() {
                return Err("missing pattern".into());
            }
            patterns.push(q.paths.remove(0));
        }
        if smart {
            fold = patterns.iter().all(|p| !p.chars().any(char::is_uppercase));
        }
        let mut regex_bytes = 0u64;
        for p in patterns {
            // Literal NULs cannot be selected by the ordinary binary-filtered matcher.
            if !(q.text || q.binary || q.null_data)
                && (p.contains('\0') || !fixed && explicit_zero(p))
            {
                return Err("pattern contains \"\\0\" but it is impossible to match\n\nConsider enabling text mode with the --text flag (or -a for short). Otherwise,\nbinary detection is enabled and matching a NUL byte is impossible.".into());
            }
            if engine == "pcre2" && !fixed {
                q.patterns.push(Pattern::Pcre(pcre::Pcre::new(
                    p,
                    fold,
                    q.dotall && q.multiline,
                    !q.ascii,
                    q.crlf,
                )?));
                continue;
            }
            q.patterns.push(if fixed && fold {
                Pattern::Regex(Regex::folded_literal(p)?)
            } else if fixed {
                Pattern::Literal(p)
            } else {
                let flags = if q.multiline {
                    match (q.dotall, q.crlf) {
                        (true, true) => "(?msR)",
                        (true, false) => "(?ms)",
                        (false, true) => "(?mR)",
                        (false, false) => "(?m)",
                    }
                } else {
                    ""
                };
                let source = format!("{}{flags}{p}", if q.ascii { "(?-u)" } else { "" });
                let groups = q.replacement_text.is_some_and(|s| s.contains('$'));
                let wrapped = format!("(?:{source})");
                let compiled = if groups {
                    Regex::capturing(&wrapped, fold)
                } else {
                    Regex::with_case_folding(&wrapped, fold)
                };
                match compiled {
                    Ok(re) => Pattern::Regex(re),
                    Err(e) if e.contains("can match invalid UTF-8") => {
                        q.byte_input = true;
                        Pattern::Regex(Regex::byte_input(&wrapped, fold, groups)?)
                    }
                    Err(_) if engine == "auto" => Pattern::Pcre(pcre::Pcre::new(
                        p,
                        fold,
                        q.dotall && q.multiline,
                        !q.ascii,
                        q.crlf,
                    )?),
                    Err(e) => return Err(e),
                }
            });
            if let (Some(limit), Some(Pattern::Regex(regex))) = (regex_limit, q.patterns.last())
                && (fold || p.chars().any(|c| r"\.^$|?*+()[]{}".contains(c)))
            {
                regex_bytes = regex_bytes.saturating_add(regex.instruction_bytes() as u64);
                if regex_bytes > limit {
                    return Err(format!("compiled regex exceeds size limit of {limit}"));
                }
            }
        }
        Ok(q)
    }

    fn view<'b>(&self, bytes: &'b [u8]) -> std::borrow::Cow<'b, str> {
        if self.byte_input && !bytes.is_ascii() {
            std::borrow::Cow::Owned(bytes.iter().map(|&b| char::from(b)).collect())
        } else {
            String::from_utf8_lossy(bytes)
        }
    }
    fn original(&self, bytes: &[u8], view: &std::borrow::Cow<'_, str>, offset: usize) -> usize {
        match view {
            std::borrow::Cow::Borrowed(_) => offset,
            _ if self.byte_input => view[..offset].chars().count(),
            _ => original_offset(bytes, offset),
        }
    }
    fn eligible(&self, hay: &str, r: &Range<usize>) -> bool {
        use crate::domain::pattern::word_char;
        (!self.whole
            || (r.start == 0 || self.multiline && hay.as_bytes()[r.start - 1] == b'\n')
                && (r.end == hay.len() || self.multiline && hay.as_bytes()[r.end] == b'\n'))
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
                Pattern::Pcre(re) => re
                    .matches(hay)
                    .find(|r| self.eligible(hay, r))
                    .map(|r| r.start),
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
                Pattern::Pcre(re) => {
                    ranges.extend(re.matches(hay).filter(|r| self.eligible(hay, r)))
                }
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
    pub fn requires_caller(&self) -> bool {
        self.transform.active()
    }
    pub fn type_listing(&self) -> Option<&str> {
        self.types.listing.as_deref()
    }
    pub fn stdin_only(&self) -> bool {
        self.paths.as_slice() == ["-"]
            || self.paths.is_empty() && !self.files && !self.pattern_stdin
    }
    pub fn explicit_stdin(&self) -> bool {
        self.paths.as_slice() == ["-"]
    }
    pub fn stream(&self, text: &[u8]) -> (Vec<u8>, bool) {
        let mut out = Vec::new();
        let start = Instant::now();
        let mut stats = events::Stats::default();
        let hit = self.scan_bytes("<stdin>", text, false, &mut out, &mut stats);
        self.summary(&stats, start, &mut out);
        (out, hit)
    }
    fn summary(&self, stats: &events::Stats, start: Instant, out: &mut Vec<u8>) {
        if self.json && !self.files && (!self.quiet || self.stats) {
            stats.summary(start, out);
        } else if self.stats && !self.files {
            stats.plain_summary(start, out);
        }
    }
    fn scan(
        &self,
        path: &str,
        text: &[u8],
        show_path: bool,
        out: &mut Vec<u8>,
        stats: &mut events::Stats,
    ) -> bool {
        if self.json && !self.quiet {
            return events::search(self, path, text, out, stats);
        }
        let clock = self.stats.then(Instant::now);
        let color = self.color.unwrap_or(self.terminal_color);
        let paint = |style: &str, value: &str| format!("\x1b[0m{style}{value}\x1b[0m");
        let colored_path;
        let path = if color {
            colored_path = paint(&self.colors.path.ansi(), path);
            &colored_path
        } else {
            path
        };
        let show_path = self.filename.unwrap_or(show_path);
        let heading = show_path && self.heading.unwrap_or(self.terminal);
        let numbered = self.numbered.unwrap_or(self.terminal);
        let match_style = color.then(|| self.colors.matched.ansi());
        let mut hits = 0usize;
        let mut occurrences = 0usize;
        let mut after_end = 0;
        let mut emitted = 0;
        let mut previous = std::collections::VecDeque::new();
        let start = out.len();
        let terminator = if self.null_data { 0 } else { b'\n' };
        let source = self.multiline.then(|| self.view(text));
        let spans = source.as_ref().map(|s| self.ranges(s));
        let blocks = if self.replacement_text.is_some() && !self.invert {
            source
                .as_ref()
                .map_or_else(Vec::new, |s| multiline::blocks(s, spans.as_ref().unwrap()))
        } else {
            Vec::new()
        };
        let mut block_index = 0;
        let mut span_index = 0;
        let mut decoded_end = 0;
        let mut byte_end = 0;
        for (index, record) in text.split_inclusive(|&b| b == terminator).enumerate() {
            let raw = record.strip_suffix(&[terminator]).unwrap_or(record);
            let byte_start = byte_end;
            byte_end += record.len();
            let decoded = self.view(raw);
            let body = decoded.as_ref();
            let line = index + 1;
            let hay = if self.crlf {
                body.strip_suffix('\r').unwrap_or(body)
            } else {
                body
            };
            let line_start = decoded_end;
            decoded_end += body.len() + record.len() - raw.len();
            let mut starts = 0;
            let ranges = if let Some(spans) = &spans {
                while let Some(span) = spans.get(span_index) {
                    if span.end > line_start || span.start == line_start {
                        break;
                    }
                    span_index += 1;
                }
                let mut local = Vec::new();
                for span in &spans[span_index..] {
                    if span.start >= decoded_end {
                        break;
                    }
                    starts += usize::from(span.start >= line_start);
                    if self.vimgrep && span.start < line_start {
                        continue;
                    }
                    local.push(
                        span.start.saturating_sub(line_start).min(hay.len())
                            ..span.end.saturating_sub(line_start).min(hay.len()),
                    );
                }
                Some(local)
            } else {
                (self.only
                    || self.vimgrep
                    || color
                    || self.count_matches
                    || self.stats
                    || self.replacement_text.is_some())
                .then(|| self.ranges(hay))
            };
            let match_count = ranges.as_ref().map_or(0, Vec::len);
            let pos = match &ranges {
                Some(r) => r.first().map(|r| r.start),
                None => self.first(hay),
            };
            let hit = (pos.is_some() != self.invert) && hits < self.max.unwrap_or(usize::MAX);
            if !hit && self.stop_on_nonmatch && hits > 0 {
                break;
            }
            if hit {
                if self.without {
                    return false;
                }
                hits += 1;
                if self.multiline && !self.invert {
                    occurrences += starts;
                } else if (self.stats || self.count && (self.count_matches || self.only))
                    && !self.invert
                {
                    occurrences += ranges.as_ref().map_or(0, Vec::len);
                } else {
                    occurrences += 1;
                }
                if self.quiet {
                    if self.stats {
                        continue;
                    }
                    return true;
                }
                if self.list {
                    self.print_path(out, path, b"\n");
                    return true;
                }
            }
            if self.count || self.without {
                continue;
            }
            let remaining_matches = ranges.as_ref().map_or(0, |rs| {
                rs.iter()
                    .filter(|r| r.start >= self.max_columns.unwrap_or(usize::MAX))
                    .count()
            });
            let mut emit = |n: usize, offset: usize, s: &[u8], col: Option<usize>| {
                if emitted == 0 && heading {
                    if start > 0 {
                        out.push(b'\n');
                    }
                    self.print_path(out, path, b"\n");
                }
                if (self.before != 0 || self.after != 0)
                    && (emitted != 0 && n > emitted + 1 || emitted == 0 && start > 0 && !heading)
                {
                    if let Some(separator) = &self.context_separator {
                        out.extend_from_slice(separator);
                        out.push(b'\n');
                    }
                }
                let sep = if col.is_some() {
                    &self.match_separator
                } else {
                    &self.context_field
                };
                if show_path && !heading {
                    self.print_path(out, path, sep);
                }
                if numbered {
                    if color {
                        write!(out, "{}", paint(&self.colors.line.ansi(), &n.to_string())).unwrap();
                    } else {
                        write!(out, "{n}").unwrap();
                    }
                }
                if numbered {
                    out.extend_from_slice(sep);
                }
                if self.column
                    && let Some(c) = col
                {
                    if color {
                        write!(out, "{}", paint(&self.colors.column.ansi(), &c.to_string()))
                            .unwrap();
                    } else {
                        write!(out, "{c}").unwrap();
                    }
                }
                if self.column && col.is_some() {
                    out.extend_from_slice(sep);
                }
                if self.byte_offset {
                    let offset = offset + if self.only { col.unwrap_or(1) - 1 } else { 0 };
                    write!(out, "{offset}").unwrap();
                    out.extend_from_slice(sep);
                }
                let content = if self.trim { s.trim_ascii_start() } else { s };
                if let Some(limit) = self.max_columns.filter(|&n| n > 0 && content.len() > n) {
                    let tracked =
                        color || self.replacement_text.is_some() || self.only || self.vimgrep;
                    if self.preview {
                        out.extend_from_slice(&content[..limit]);
                        if tracked && col.is_some() {
                            let match_count = remaining_matches;
                            write!(
                                out,
                                " [... {match_count} more match{}]",
                                if match_count == 1 { "" } else { "es" }
                            )
                            .unwrap();
                        } else {
                            out.extend_from_slice(b" [... omitted end of long line]");
                        }
                    } else if col.is_none() {
                        out.extend_from_slice(b"[Omitted long context line]");
                    } else if tracked {
                        write!(
                            out,
                            "[Omitted long line with {match_count} match{}]",
                            if match_count == 1 { "" } else { "es" }
                        )
                        .unwrap();
                    } else {
                        out.extend_from_slice(b"[Omitted long matching line]");
                    }
                } else {
                    out.extend_from_slice(content);
                }
                if self.crlf && self.only && raw.ends_with(b"\r") {
                    out.push(b'\r');
                }
                out.push(terminator);
                emitted = n;
            };
            if hit {
                for (n, offset, s) in previous.drain(..) {
                    emit(n, offset, s, None);
                }
                while block_index < blocks.len() && blocks[block_index].end <= line_start {
                    block_index += 1;
                }
                if let Some(block) = blocks
                    .get(block_index)
                    .filter(|b| b.start <= line_start && line_start < b.end)
                {
                    if block.start == line_start {
                        for (offset, bytes) in self.replace_block(
                            self.replacement_text.unwrap(),
                            source.as_ref().unwrap(),
                            spans.as_ref().unwrap(),
                            block,
                        ) {
                            emit(
                                line + offset,
                                byte_start,
                                &bytes,
                                Some(pos.unwrap_or(0) + 1),
                            );
                        }
                    }
                    after_end = line.saturating_add(self.after);
                    continue;
                }
                // Regex offsets refer to the decoded line. Columns and emitted slices
                // refer to the original bytes, including malformed UTF-8 under -a.
                let byte_offset = |offset| self.original(raw, &decoded, offset);
                if let Some(ranges) = ranges.filter(|_| {
                    self.only || self.vimgrep || color || self.replacement_text.is_some()
                }) {
                    let mut styled = Vec::new();
                    let mut at = 0;
                    for range in ranges.into_iter().filter(|r| {
                        !r.is_empty() || self.replacement_text.is_some() || self.word && self.only
                    }) {
                        let start = byte_offset(range.start);
                        let end = byte_offset(range.end);
                        if let Some(template) = self.replacement_text {
                            let replacement = self.replacement(template, hay, range.clone());
                            if self.only {
                                emit(line, byte_start, &replacement, Some(start + 1));
                            } else {
                                styled.extend_from_slice(&raw[at..start]);
                                styled.extend_from_slice(&replacement);
                            }
                            at = end;
                            continue;
                        }
                        if self.vimgrep && !self.only {
                            emit(line, byte_start, raw, Some(start + 1));
                            continue;
                        }
                        if color {
                            if !self.only {
                                styled.extend_from_slice(&raw[at..start]);
                            }
                            if match_style.as_ref().is_some_and(|s| !s.is_empty()) {
                                styled.extend_from_slice(b"\x1b[0m");
                                styled.extend_from_slice(match_style.as_ref().unwrap().as_bytes());
                            }
                            styled.extend_from_slice(&raw[start..end]);
                            if match_style.as_ref().is_some_and(|s| !s.is_empty()) {
                                styled.extend_from_slice(b"\x1b[0m");
                            }
                        }
                        if self.only {
                            emit(
                                line,
                                byte_start,
                                if color { &styled } else { &raw[start..end] },
                                Some(start + 1),
                            );
                            styled.clear();
                        }
                        at = end;
                    }
                    if !self.only && !self.vimgrep {
                        styled.extend_from_slice(&raw[at..]);
                        emit(
                            line,
                            byte_start,
                            &styled,
                            Some(byte_offset(pos.unwrap_or(0)) + 1),
                        );
                    }
                } else {
                    emit(
                        line,
                        byte_start,
                        raw,
                        Some(byte_offset(pos.unwrap_or(0)) + 1),
                    );
                }
                after_end = line.saturating_add(self.after);
            } else if self.passthru || line <= after_end {
                emit(line, byte_start, raw, None);
            } else if self.before > 0 {
                if previous.len() == self.before {
                    previous.pop_front();
                }
                previous.push_back((line, byte_start, raw));
            }
            if hits == self.max.unwrap_or(usize::MAX) && line >= after_end {
                break;
            }
        }
        if let Some(clock) = clock {
            stats.scanned(byte_end, out.len() - start, hits, occurrences, clock);
        }
        if self.without {
            self.print_path(out, path, b"\n");
            return true;
        }
        if self.count && (hits > 0 || self.include_zero) {
            if show_path {
                self.print_path(out, path, b":");
            }
            writeln!(
                out,
                "{}",
                if self.multiline || self.count_matches || self.only {
                    occurrences
                } else {
                    hits
                }
            )
            .unwrap();
        }
        hits > 0
    }
    fn glob_override(&self, name: &str) -> Option<bool> {
        let basename = name.rsplit('/').next().unwrap_or(name);
        self.globs
            .iter()
            .rev()
            .find(|(_, full, g)| g.is_match(if *full { name } else { basename }))
            .map(|g| g.0)
    }
    fn includes(&self, name: &str) -> bool {
        self.glob_override(name).unwrap_or_else(|| {
            !self.globs.iter().any(|g| g.0)
                && self.types.accepts(name.rsplit('/').next().unwrap_or(name))
        })
    }
    pub(super) fn run(
        &self,
        pin: &Pin,
        cwd: &Path,
        outcome: &mut super::Outcome,
    ) -> io::Result<Vec<u8>> {
        let needs_all = self.files || self.invert || self.without || self.include_zero
            || self.passthru || matches!(self.encoding, encoding::Encoding::Legacy(_));
        let mut candidates_by_id = None;
        if !needs_all {
            let mut ids = BTreeSet::new();
            for pattern in &self.patterns {
                let key = match pattern {
                    Pattern::Literal(p) => p,
                    Pattern::Regex(re) => re.required_literal(),
                    Pattern::Pcre(_) => "",
                };
                if key.len() < 3 {
                    return self.walk_search(cwd, outcome, Some((pin, None)));
                }
                ids.extend(candidates(pin, CONTENT, key)?);
            }
            candidates_by_id = Some(ids);
        }
        self.walk_search(cwd, outcome, Some((pin, candidates_by_id.as_ref())))
    }
}

fn original_offset(bytes: &[u8], decoded_offset: usize) -> usize {
    let mut remaining = decoded_offset;
    let mut original = 0;
    for chunk in bytes.utf8_chunks() {
        let valid = chunk.valid().len();
        if remaining <= valid {
            return original + remaining;
        }
        remaining -= valid + char::REPLACEMENT_CHARACTER.len_utf8();
        original += valid + chunk.invalid().len();
    }
    original
}

fn explicit_zero(pattern: &str) -> bool {
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            continue;
        }
        if chars.next() == Some('x') {
            let mut hex = chars.clone();
            if hex.next() == Some('0') && hex.next() == Some('0') {
                return true;
            }
        }
    }
    false
}
