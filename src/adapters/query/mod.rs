mod display;
mod json;
use display::Display;
mod lines;
mod navigation;
mod relationships;
use crate::{
    adapters::snapshot::Pin,
    adapters::snapshot::manifest::*,
    domain::{
        content_lookup::{Cursor, ExtentRead},
        markdown::is_markdown,
        pattern::{Glob, Regex},
        ranked::RankedIndex,
    },
};
use json::{array, object, quote};
use navigation::Markdown;
use std::{collections::BTreeSet, io, path::Path};

pub enum Request<'a> {
    Lines(lines::Lines<'a>),
    Search(&'a str),
    Symbol(&'a str),
    Content(&'a str),
    Regex(Regex),
    Path(&'a str),
    PathRegex(Regex),
    Exact(&'a str),
    Glob(Glob),
    Section(&'a str, bool),
    Backlinks(&'a str),
    NamedRelationship(&'a str, &'a str),
    Relationship(&'a str, &'a str, usize, usize),
}
impl<'a> Request<'a> {
    pub fn parse(args: &'a [String]) -> Result<Self, String> {
        let mut v: Vec<_> = args.iter().map(String::as_str).collect();
        if let Some(verb) = v.first_mut() {
            *verb = match *verb {
                "--refs" | "--reference" => "references",
                "--symbol" => "symbol",
                original => original,
            };
        }
        let pos = |s: &str| {
            s.parse::<usize>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| "expected a positive position".to_string())
        };
        Ok(match v.as_slice() {
            ["-k" | "--keywords" | "rank", q] if !q.trim().is_empty() => Self::Search(q),
            ["content", q] if !q.is_empty() => Self::Content(q),
            ["regex", q] if !q.is_empty() => Self::Regex(Regex::new(q)?),
            ["path", q] if !q.is_empty() => Self::Path(q),
            ["path-regex", q] if !q.is_empty() => Self::PathRegex(Regex::new(q)?),
            ["path-exact", q] if !q.is_empty() => Self::Exact(q),
            ["glob", q] if !q.is_empty() => Self::Glob(Glob::new(q)?),
            ["symbol", q] if !q.is_empty() => Self::Symbol(q),
            ["section", q] => Self::Section(q, false),
            ["section", q, "--parent"] => Self::Section(q, true),
            ["backlinks", q] => Self::Backlinks(q),
            [op @ ("callers" | "references"), name] if !name.is_empty() => {
                Self::NamedRelationship(op, name)
            }
            [
                op @ ("definition" | "references" | "callers"),
                path,
                line,
                col,
            ] => Self::Relationship(op, path, pos(line)?, pos(col)?),
            _ => return Err("invalid query arguments".into()),
        })
    }
}
fn candidates(pin: &Pin, product: usize, q: &str) -> io::Result<Vec<usize>> {
    let chars: Vec<_> = q.chars().take(18).collect();
    if chars.len() < 3 {
        return Ok((0..pin.files.len()).collect());
    }
    let keys: BTreeSet<_> = chars.windows(3).map(|x| [x[0], x[1], x[2]]).collect();
    Cursor::open(pin.reader(product)?)?.candidates(keys, pin.files.len())
}
fn add_crumbs(
    pin: &Pin,
    md: &mut Option<Markdown>,
    file: usize,
    offset: usize,
    fields: &mut Vec<(&str, String)>,
) -> io::Result<()> {
    if is_markdown(pin.files[file].path()) {
        if md.is_none() {
            *md = Some(Markdown::read(pin)?);
        }
        let crumbs = md.as_mut().unwrap().hit_crumbs(pin, file, offset)?;
        fields.push(("breadcrumbs", crumbs));
    }
    Ok(())
}
#[derive(Default)]
pub struct Outcome {
    pub code: u8,
    pub diagnostics: String,
}
pub struct Invocation<'a> {
    pub request: Request<'a>,
    pub mode: &'a str,
    display: Display,
}
impl<'a> Invocation<'a> {
    pub fn parse(args: &'a [String]) -> Result<Self, String> {
        let mode = args.first().ok_or("missing pattern")?.as_str();
        if mode == "--cx" {
            let tail = &args[1..];
            let mode = tail.first().ok_or("missing indexed command")?.as_str();
            if !super::cli::is_command(mode) && mode != "rank" {
                return Err("unknown indexed command".into());
            }
            return Ok(Self {
                request: Request::parse(tail)?,
                mode,
                display: Display::default(),
            });
        }
        if matches!(
            mode,
            "--reference" | "--refs" | "--symbol" | "-k" | "--keywords"
        ) {
            let count = if matches!(mode, "--reference" | "--refs")
                && args.get(2).is_some_and(|s| !s.starts_with('-'))
            {
                4
            } else {
                2
            };
            if args.len() < count {
                return Err("missing indexed query operand".into());
            }
            return Ok(Self {
                request: Request::parse(&args[..count])?,
                mode,
                display: Display::parse(&args[count..])?,
            });
        }
        let argv: Vec<_> = args.iter().map(String::as_str).collect();
        Ok(Self {
            request: Request::Lines(lines::Lines::parse(&argv)?),
            mode,
            display: Display::default(),
        })
    }
}
pub fn evaluate(
    pin: &Pin,
    invocation: Invocation<'_>,
    cwd: &Path,
    outcome: &mut Outcome,
) -> io::Result<String> {
    let Invocation {
        request: req,
        mut mode,
        display,
    } = invocation;
    let compact = display.public && !display.json;
    let mut sites = Vec::new();
    let mut hits = Vec::new();
    let mut truncated = false;
    let mut md = None;
    match req {
        Request::Lines(lines) => return lines.run(pin, cwd, outcome),
        Request::NamedRelationship(op, name) => {
            return relationships::query(
                pin,
                cwd,
                op,
                relationships::Subject::Name(name),
                &display,
                outcome,
            );
        }
        Request::Relationship(op, path, line, col) => {
            return relationships::query(
                pin,
                cwd,
                op,
                relationships::Subject::Position(path, line, col),
                &display,
                outcome,
            );
        }
        Request::Section(s, parent) => return Markdown::read(pin)?.section(pin, s, parent),
        Request::Backlinks(s) => return Markdown::read(pin)?.backlinks(pin, s),
        Request::Search(q) => {
            mode = "search";
            let mut reader = pin.reader(RANK)?;
            let (rows, extra) = RankedIndex::search_extent(
                reader.length(),
                |at, bytes| reader.read(at, bytes),
                pin.files.len(),
                |i| pin.files[i].path(),
                |i, r| pin.source_slice(i, r),
                q,
            )?;
            truncated = extra;
            for h in rows {
                let entry = &pin.files[h.file];
                if compact {
                    sites.push((h.file, h.anchor));
                    continue;
                }
                let mut fields = vec![
                    ("path", quote(entry.path())),
                    ("line", h.line.to_string()),
                    ("last_line", h.last_line.to_string()),
                    ("score", h.score.to_string()),
                    ("excerpt", quote(&h.excerpt)),
                ];
                add_crumbs(
                    pin,
                    &mut md,
                    h.file,
                    entry
                        .line_start(h.line)
                        .ok_or_else(|| io::Error::other("bad chunk line"))?,
                    &mut fields,
                )?;
                hits.push(object(&fields));
            }
        }
        Request::Symbol(q) => {
            mode = "symbol";
            let mut lookup = crate::application::index::symbol_lookup::SymbolLookup::open(
                pin.reader(SYMBOLS)?,
                pin.files.len(),
            )?;
            let (rows, extra) = lookup.get(q, 50)?;
            truncated = extra;
            for s in rows {
                if compact {
                    sites.push((s.file, s.span.start));
                    continue;
                }
                let f = &pin.files[s.file];
                let (l, c) = f.line_column(s.span.start);
                let start = s.span.start + 1 - c;
                let col = if start == s.span.start {
                    1
                } else {
                    1 + pin
                        .source_slice(s.file, start as u64..s.span.start as u64)?
                        .encode_utf16()
                        .count()
                };
                hits.push(object(&[
                    ("path", quote(f.path())),
                    ("line", l.to_string()),
                    ("byte_column", c.to_string()),
                    ("col", col.to_string()),
                    ("name", quote(&s.name)),
                    ("kind", quote(&s.kind)),
                ]));
            }
        }
        Request::Content(_) | Request::Regex(_) => {
            let ids = match &req {
                Request::Content(q) => candidates(pin, CONTENT, q)?,
                Request::Regex(re) => candidates(pin, CONTENT, re.required_literal())?,
                _ => unreachable!(),
            };
            let mut scan = (ids.len() == pin.files.len()).then(|| pin.source_window());
            'files: for id in ids {
                let f = &pin.files[id];
                if f.bytes.is_none() {
                    continue;
                }
                let owned;
                let text = if let Some(window) = &mut scan {
                    window.get(id)?
                } else {
                    owned = pin.source(id)?;
                    &owned
                };
                let found: Box<dyn Iterator<Item = usize> + '_> = match &req {
                    Request::Content(q) => Box::new(text.match_indices(q).map(|(i, _)| i)),
                    Request::Regex(re) => Box::new(re.matches(text).map(|r| r.start)),
                    _ => unreachable!(),
                };
                for byte in found {
                    if hits.len() == 50 {
                        truncated = true;
                        break 'files;
                    }
                    let (l, c) = f.line_column(byte);
                    let mut fields = vec![
                        ("path", quote(f.path())),
                        ("line", l.to_string()),
                        ("byte_column", c.to_string()),
                    ];
                    add_crumbs(pin, &mut md, id, byte, &mut fields)?;
                    hits.push(object(&fields));
                }
            }
        }
        Request::Path(_) | Request::PathRegex(_) | Request::Exact(_) | Request::Glob(_) => {
            let ids = match &req {
                Request::Exact(q) => pin
                    .files
                    .binary_search_by(|f| f.path().cmp(q))
                    .ok()
                    .into_iter()
                    .collect(),
                Request::Path(q) => candidates(pin, PATHGRAM, q)?,
                Request::PathRegex(re) => candidates(pin, PATHGRAM, re.required_literal())?,
                _ => (0..pin.files.len()).collect(),
            };
            for id in ids {
                let path = pin.files[id].path();
                let ok = match &req {
                    Request::Glob(g) => g.is_match(path),
                    Request::PathRegex(re) => re.matches(path).next().is_some(),
                    Request::Path(s) => path.contains(s),
                    _ => true,
                };
                if ok {
                    if hits.len() == 50 {
                        truncated = true;
                        break;
                    }
                    hits.push(object(&[("path", quote(path))]));
                }
            }
        }
    }
    if display.public {
        outcome.code = u8::from(hits.is_empty() && sites.is_empty());
        if compact {
            if truncated {
                outcome.diagnostics.push_str("cx: results truncated\n");
            }
            return display::render(pin, cwd, sites, &display, mode == "search");
        }
    }
    Ok(object(&[
        ("root", quote(&pin.root.to_string_lossy())),
        ("mode", quote(mode)),
        ("generation", pin.generation.to_string()),
        ("reused", pin.reused.to_string()),
        (
            "coverage",
            object(&[
                ("paths", pin.files.len().to_string()),
                ("text_files", pin.coverage_counts().0.to_string()),
                ("parsed_files", pin.coverage_counts().1.to_string()),
                ("files_with_parse_errors", "null".into()),
                ("skipped_paths", "null".into()),
                ("symbols", quote("partial")),
            ]),
        ),
        ("relationship_status", quote("partial")),
        ("truncated", truncated.to_string()),
        ("hits", array(hits)),
    ]))
}
