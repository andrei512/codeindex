use super::{
    Pin,
    json::{Object, ObjectArray},
};
use crate::{
    adapters::snapshot::manifest::MD,
    application::index::{
        binary::bad,
        markdown_lookup::{BACKLINK_LIMIT, Lookup},
    },
    domain::markdown::{Link, Section, SectionLookup, format_handle, parse_handle},
};
use std::{collections::BTreeMap, io};

pub enum DocumentQuery<'a> {
    Section { handle: &'a str, parent: bool },
    Backlinks(&'a str),
}

impl<'a> DocumentQuery<'a> {
    pub(super) fn run(self, pin: Option<&Pin>, cwd: &std::path::Path) -> io::Result<String> {
        if let Some(pin) = pin {
            let mut doc = Markdown::read(pin)?;
            return match self {
                Self::Section { handle, parent } => doc.section(pin, handle, parent),
                Self::Backlinks(handle) => doc.backlinks(pin, handle),
            };
        }
        match self {
            Self::Section { handle, parent } => Markdown::plain_section(cwd, handle, parent),
            Self::Backlinks(handle) => Markdown::plain_backlinks(cwd, handle),
        }
    }
}

pub struct Markdown {
    headings: BTreeMap<usize, Vec<Section>>,
    lookup: Lookup,
}
impl Markdown {
    pub fn read(pin: &Pin) -> io::Result<Self> {
        Ok(Self {
            headings: BTreeMap::new(),
            lookup: Lookup::open(pin.reader(MD)?)?,
        })
    }
    fn headings(&mut self, pin: &Pin, id: usize) -> io::Result<&[Section]> {
        use std::collections::btree_map::Entry;
        Ok(match self.headings.entry(id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(self.lookup.headings(pin.reader(MD)?, id)?),
        })
    }
    fn crumbs(path: &str, doc: &[Section], sec: usize) -> String {
        let mut crumbs = ObjectArray::default();
        for section in doc.breadcrumbs(sec) {
            crumbs.push(|row| {
                row.string("anchor", &section.anchor);
                row.string("handle", &format_handle(path, &section.anchor));
                row.number("level", section.level);
                row.number("line", section.line);
                row.string("title", &section.title);
            });
        }
        crumbs.finish()
    }
    pub fn hit_crumbs(&mut self, pin: &Pin, file: usize, byte: usize) -> io::Result<String> {
        let doc = self.headings(pin, file)?;
        Ok(Self::crumbs(
            pin.files[file].path(),
            doc,
            doc.section_at(byte),
        ))
    }
    pub fn plain_section(
        root: &std::path::Path,
        selector: &str,
        parent: bool,
    ) -> io::Result<String> {
        use crate::adapters::repository;
        use crate::domain::markdown::{Parsed, is_markdown};
        let (path, anchor) =
            parse_handle(selector).ok_or_else(|| io::Error::other("invalid document handle"))?;
        let mut files = repository::plain_paths(root, false)?;
        let id = files
            .binary_search_by(|f| f.path.cmp(&path))
            .map_err(|_| io::Error::other("unknown document"))?;
        let file = files.swap_remove(id);
        drop(files);
        let body = if is_markdown(&path) {
            match file.captured {
                Some(text) => text,
                None => repository::CaptureRoot::new(root)?.capture(&path, file.stamp)?,
            }
        } else {
            None
        }
        .ok_or_else(|| io::Error::other("not a Markdown document"))?;
        let headings = Parsed::headings(&path, &body);
        Self::section_result(
            (&path, anchor.as_deref()),
            parent,
            &headings.sections,
            |range| {
                body.get(range)
                    .map(std::borrow::Cow::Borrowed)
                    .ok_or_else(bad)
            },
        )
    }

    pub fn section(&mut self, pin: &Pin, selector: &str, parent: bool) -> io::Result<String> {
        let (path, anchor) =
            parse_handle(selector).ok_or_else(|| io::Error::other("invalid document handle"))?;
        let id = pin
            .files
            .binary_search_by(|f| f.path().cmp(&path))
            .map_err(|_| io::Error::other("unknown document"))?;
        let doc = self.headings(pin, id)?;
        Self::section_result((&path, anchor.as_deref()), parent, doc, |range| {
            pin.text_range(id, range.start as u64..range.end as u64)
        })
    }

    fn section_result<'a>(
        (path, anchor): (&str, Option<&str>),
        parent: bool,
        doc: &[Section],
        read: impl FnOnce(std::ops::Range<usize>) -> io::Result<std::borrow::Cow<'a, str>>,
    ) -> io::Result<String> {
        let mut i = doc
            .section(anchor.unwrap_or(""))
            .ok_or_else(|| io::Error::other("unknown section"))?;
        if parent {
            let Some(p) = doc[i].parent else {
                return Ok("null".into());
            };
            i = p;
        }
        let s = &doc[i];
        let body = read(s.start..s.end)?;
        let mut out = Object::new();
        out.string("anchor", &s.anchor);
        out.string("path", path);
        out.string("title", &s.title);
        out.number("level", s.level);
        out.number("line", s.line);
        out.number("start", s.start);
        out.number("end", s.end);
        out.string("handle", &format_handle(path, &s.anchor));
        match s.parent {
            Some(p) => out.string("parent", &format_handle(path, &doc[p].anchor)),
            None => out.raw("parent", "null"),
        }
        out.raw("breadcrumbs", &Self::crumbs(path, doc, i));
        out.string("body", &body);
        Ok(out.finish())
    }
    pub fn backlinks(&mut self, pin: &Pin, selector: &str) -> io::Result<String> {
        let (path, anchor) =
            parse_handle(selector).ok_or_else(|| io::Error::other("invalid document handle"))?;
        let (matched, truncated) =
            self.lookup
                .backlinks(pin.reader(MD)?, &path, anchor.as_deref())?;
        let mut hits = ObjectArray::default();
        for group in matched.chunk_by(|a, b| a.0 == b.0) {
            let f = group[0].0;
            let path = pin.files.get(f).ok_or_else(bad)?.path();
            let (headings, links) = self.lookup.links(
                pin.reader(MD)?,
                f,
                group.iter().map(|&(_, ordinal)| ordinal),
            )?;
            append_backlinks(&mut hits, path, &headings, &links);
        }
        Ok(backlink_result(selector, hits, truncated))
    }

    pub fn plain_backlinks(root: &std::path::Path, selector: &str) -> io::Result<String> {
        use crate::adapters::repository::{CaptureRoot, plain_paths};
        use crate::domain::markdown::{Parsed, is_markdown};
        let (target, anchor) =
            parse_handle(selector).ok_or_else(|| io::Error::other("invalid document handle"))?;
        let files = plain_paths(root, false)?;
        let capture = CaptureRoot::new(root)?;
        let mut hits = ObjectArray::default();
        let mut truncated = false;
        // The persisted posting order is file ordinal, then link ordinal. The
        // sorted plain walk and parser already produce links in that order.
        let documents = files.into_iter().filter(|f| is_markdown(&f.path)).filter_map(|file| {
            let source = file.captured
                .map_or_else(|| capture.capture(&file.path, file.stamp), Ok);
            source.transpose().map(|text| text.map(|text| (file.path, text)))
        });
        for document in documents {
            let (path, text) = document?;
            // Keep capture failures observable even after the page is full.
            if truncated { continue; }
            // One extra match distinguishes a full page from a truncated one.
            let (sections, links) = Parsed::backlinks(
                &path, &text, (&target, anchor.as_deref()), BACKLINK_LIMIT + 1 - hits.len,
            );
            let count = links.len().min(BACKLINK_LIMIT - hits.len);
            truncated = count < links.len();
            append_backlinks(&mut hits, &path, &sections, &links[..count]);
        }
        Ok(backlink_result(selector, hits, truncated))
    }
}

fn append_backlinks(hits: &mut ObjectArray, path: &str, headings: &[Section], links: &[Link]) {
    for group in links.chunk_by(|a, b| a.section == b.section) {
        let section = group[0].section;
        let handle = format_handle(path, &headings[section].anchor);
        let crumbs = Markdown::crumbs(path, headings, section);
        for link in group {
            hits.push(|row| {
                row.string("path", path);
                row.number("line", link.line);
                row.number("offset", link.offset);
                row.string("handle", &handle);
                row.string("target", &format_handle(&link.target, &link.anchor));
                row.raw("breadcrumbs", &crumbs);
            });
        }
    }
}

fn backlink_result(selector: &str, hits: ObjectArray, truncated: bool) -> String {
    let mut out = Object::new();
    out.string("target", selector);
    out.raw("hits", &hits.finish());
    out.raw("truncated", if truncated { "true" } else { "false" });
    out.finish()
}
