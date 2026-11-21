mod json;
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
    Status,
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
    Relationship(&'a str, &'a str, usize, usize),
}
impl<'a> Request<'a> {
    pub fn parse(args: &'a [String]) -> Result<Self, String> {
        let v: Vec<_> = args.iter().map(String::as_str).collect();
        let pos = |s: &str| {
            s.parse::<usize>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| "expected a positive position".to_string())
        };
        Ok(match v.as_slice() {
            ["status"] => Self::Status,
            ["search", q] if !q.trim().is_empty() => Self::Search(q),
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
pub fn evaluate(pin: &Pin, req: Request<'_>, mode: &str, cwd: &Path) -> io::Result<String> {
    let mut hits = Vec::new();
    let mut truncated = false;
    let mut md = None;
    match req {
        Request::Status => {
            let mut products = Vec::new();
            for (name, id) in [
                ("paths", PATHGRAM),
                ("content", CONTENT),
                ("ranked", RANK),
                ("symbols", SYMBOLS),
                ("relationships", REL),
                ("markdown", MD),
            ] {
                products.push((
                    name,
                    object(&[
                        ("state", quote("published")),
                        ("bytes", pin.reader(id)?.length().to_string()),
                    ]),
                ));
            }
            return Ok(object(&[
                ("products", object(&products)),
                ("product_generation",pin.product_generation().to_string()),
                ("committed_text_files",pin.committed_files().to_string()),
                ("indexed_commit",pin.indexed_commit.as_deref().map_or_else(||"null".into(),quote)),
                ("source_bytes", pin.reader(SOURCE)?.length().to_string()),
                ("root", quote(&pin.root.to_string_lossy())),
                ("generation", pin.generation.to_string()),
                ("reused", pin.reused.to_string()),
                ("indexed_paths", pin.files.len().to_string()),
                (
                    "text_files",
                    pin.coverage_counts().0
                        .to_string(),
                ),
                (
                    "relationships",
                    object(&[
                        ("status", quote("partial")),
                        ("generation", pin.generation.to_string()),
                        ("reused", pin.reused.to_string()),
                        ("reason", quote("native language coverage incomplete")),
                    ]),
                ),
            ]));
        }
        Request::Relationship(op, path, line, col) => {
            return relationships::query(pin, cwd, op, path, line, col);
        }
        Request::Section(s, parent) => return Markdown::read(pin)?.section(pin, s, parent),
        Request::Backlinks(s) => return Markdown::read(pin)?.backlinks(pin, s),
        Request::Search(q) => {
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
            let mut lookup = crate::application::index::symbol_lookup::SymbolLookup::open(
                pin.reader(SYMBOLS)?,
                pin.files.len(),
            )?;
            let (rows, extra) = lookup.get(q, 50)?;
            truncated = extra;
            for s in rows {
                let f = &pin.files[s.file];
                let (l, c) = f.line_column(s.span.start);
                hits.push(object(&[
                    ("path", quote(f.path())),
                    ("line", l.to_string()),
                    ("byte_column", c.to_string()),
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
            let mut scan=(ids.len()==pin.files.len()).then(||pin.source_window());
            'files: for id in ids {
                let f = &pin.files[id];
                if f.bytes.is_none() {
                    continue;
                }
                let owned;
                let text=if let Some(window)=&mut scan{window.get(id)?}else{owned=pin.source(id)?;&owned};
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
    Ok(object(&[
        ("root", quote(&pin.root.to_string_lossy())),
        ("mode", quote(mode)),
        ("generation", pin.generation.to_string()),
        ("reused", pin.reused.to_string()),
        (
            "coverage",
            object(&[
                ("paths", pin.files.len().to_string()),
                (
                    "text_files",
                    pin.coverage_counts().0
                        .to_string(),
                ),
                (
                    "parsed_files",
                    pin.coverage_counts().1
                        .to_string(),
                ),
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
