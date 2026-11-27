use super::Pin;
use std::{
    fmt::Write,
    io,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub enum Display {
    #[default]
    Advanced,
    Json,
    Compact(Context),
}

#[derive(Clone, Copy)]
pub struct Context {
    column: bool,
    before: usize,
    after: usize,
}

impl Display {
    pub(super) fn compact(&self) -> bool {
        matches!(self, Self::Compact(_))
    }

    pub(super) fn public(&self) -> bool {
        !matches!(self, Self::Advanced)
    }

    pub(super) fn context(&self) -> Option<Context> {
        match *self {
            Self::Compact(context) => Some(context),
            _ => None,
        }
    }

    pub fn parse(flags: &[String]) -> Result<Self, String> {
        use clap::{Arg, ArgAction, Command, error::ContextKind};
        let mut command = Command::new("display").no_binary_name(true).disable_help_flag(true)
            .args_override_self(true)
            .arg(Arg::new("json").long("json").action(ArgAction::SetTrue))
            .arg(Arg::new("column").long("column").action(ArgAction::SetTrue));
        for (id, short) in [("after", 'A'), ("before", 'B'), ("context", 'C')] {
            command = command.arg(Arg::new(id).short(short).value_parser(clap::value_parser!(usize)));
        }
        let matches = command.try_get_matches_from(flags).map_err(|error| {
            let arg = error.get(ContextKind::InvalidArg).map(ToString::to_string).unwrap_or_default();
            if let Some(option) = arg.split_whitespace().next().filter(|s| ["-A","-B","-C"].contains(s)) {
                format!("expected a line count after {option}")
            } else { format!("unknown indexed display option {arg}") }
        })?;
        let column = matches.get_flag("column");
        if matches.get_flag("json") {
            if column || ["before", "after", "context"].iter().any(|id| matches.contains_id(id)) {
                return Err("--json cannot be combined with text display options".into());
            }
            return Ok(Self::Json);
        }
        let width = |side| {
            [side, "context"].into_iter().filter_map(|id| {
                Some((matches.index_of(id)?, *matches.get_one::<usize>(id)?))
            }).max().map_or(0, |(_, width)| width)
        };
        Ok(Self::Compact(Context { column, before: width("before"), after: width("after") }))
    }
}

pub(super) struct Position {
    pub line: usize,
    pub byte_column: usize,
    pub utf16_column: usize,
}

/// Adjacent hits on a line need only decode the bytes between them. Symbol
/// name lookup can visit sites out of order, in which case restart at line start.
#[derive(Default)]
pub(super) struct Positions {
    previous: Option<(usize, usize, usize, usize)>,
}

impl Positions {
    pub fn position(&mut self, pin: &Pin, file: usize, byte: usize) -> io::Result<Position> {
        let (line, byte_column) = pin.files[file].line_column(byte);
        let (from, base) = match self.previous {
            Some((id, l, from, col)) if (id, l) == (file, line) && from <= byte => (from, col),
            _ => (byte + 1 - byte_column, 1),
        };
        let mut utf16_column = base;
        if from < byte {
            utf16_column += pin
                .text_range(file, from as u64..byte as u64)?
                .encode_utf16()
                .count();
        }
        self.previous = Some((file, line, byte, utf16_column));
        Ok(Position {
            line,
            byte_column,
            utf16_column,
        })
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

pub(super) struct TextOutput<'a> {
    pub pin: &'a Pin,
    pub cwd: &'a Path,
    pub context: Context,
    pub ranked: bool,
}

impl TextOutput<'_> {
    pub fn render(&self, mut sites: Vec<(usize, usize)>) -> io::Result<String> {
        if !self.ranked {
            sites.sort_unstable();
        }
        let mut out = String::new();
        let mut window = Window::default();
        for (file, byte) in sites {
            let entry = &self.pin.files[file];
            let (line, column) = entry.line_column(byte);
            let first = line.saturating_sub(self.context.before).max(1);
            let len = entry.bytes.as_ref().map_or(0, |r| r.end - r.start);
            let total = entry.line_ends.len()
                + usize::from(entry.line_ends.last().copied().unwrap_or(0) < len as usize);
            let last = line.saturating_add(self.context.after).min(total.max(line));
            if window.ends_before(file, first, self.ranked) {
                window.write(self, &mut out)?;
                if self.context.before > 0 || self.context.after > 0 {
                    out.push_str("--\n");
                }
                window.anchors.clear();
            }
            window.add(file, first..=last, (line, column));
        }
        if !window.anchors.is_empty() {
            window.write(self, &mut out)?;
        }
        out.pop();
        Ok(out)
    }
}

/// Keep anchors only for the source window awaiting output. Ranked hits
/// start a new window even when their source ranges overlap.
#[derive(Default)]
struct Window {
    file: usize,
    first: usize,
    last: usize,
    anchors: Vec<(usize, usize)>,
}

impl Window {
    fn ends_before(&self, file: usize, first: usize, ranked: bool) -> bool {
        !self.anchors.is_empty()
            && (ranked || self.file != file || first > self.last.saturating_add(1))
    }

    fn add(
        &mut self,
        file: usize,
        lines: std::ops::RangeInclusive<usize>,
        (line, column): (usize, usize),
    ) {
        if self.anchors.is_empty() {
            self.file = file;
            self.first = *lines.start();
            self.last = *lines.end();
        } else {
            self.last = self.last.max(*lines.end());
        }
        if self
            .anchors
            .last()
            .is_none_or(|&(anchor, _)| anchor != line)
        {
            self.anchors.push((line, column));
        }
    }

    fn write(&self, output: &TextOutput<'_>, out: &mut String) -> io::Result<()> {
        let entry = &output.pin.files[self.file];
        let path = relative(&output.pin.root.join(entry.path()), output.cwd);
        let start = entry
            .line_start(self.first)
            .ok_or_else(|| io::Error::other("missing source line"))?;
        let end = entry
            .line_ends
            .get(self.last - 1)
            .map(|n| *n as u64)
            .unwrap_or_else(|| entry.bytes.as_ref().map_or(0, |r| r.end - r.start));
        let text = output.pin.text_range(self.file, start as u64..end)?;
        self.write_lines(out, &path, &text, output.context.column);
        Ok(())
    }

    fn write_lines(&self, out: &mut String, path: &Path, text: &str, column: bool) {
        let mut anchors = self.anchors.iter().peekable();
        for (j, body) in text.lines().enumerate() {
            let line = self.first + j;
            let anchor = anchors.next_if(|(matched, _)| *matched == line);
            Self::write_line(out, (path, column), (line, body), anchor);
        }
    }

    fn write_line(
        out: &mut String,
        (path, column): (&Path, bool),
        (line, body): (usize, &str),
        anchor: Option<&(usize, usize)>,
    ) {
        let separator = if anchor.is_some() { ':' } else { '-' };
        write!(out, "{}{separator}{line}{separator}", path.display()).unwrap();
        if let Some((_, col)) = anchor.filter(|_| column) {
            write!(out, "{col}:").unwrap();
        }
        writeln!(out, "{body}").unwrap();
    }
}
