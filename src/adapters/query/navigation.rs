use super::{
    Pin,
    json::{array, object, quote},
};
use crate::{
    adapters::snapshot::manifest::MD,
    application::index::{binary::bad, markdown_lookup::Lookup},
    domain::markdown::{Parsed, format_handle, parse_handle},
};
use std::{collections::BTreeMap, io};

pub struct Markdown {
    docs: BTreeMap<usize, Parsed>,
    lookup: Lookup,
}
impl Markdown {
    pub fn read(pin: &Pin) -> io::Result<Self> {
        Ok(Self {
            docs: BTreeMap::new(),
            lookup: Lookup::open(pin.reader(MD)?)?,
        })
    }
    fn document(&mut self, pin: &Pin, id: usize) -> io::Result<&Parsed> {
        if let std::collections::btree_map::Entry::Vacant(e) = self.docs.entry(id) {
            e.insert(
                self.lookup
                    .document(pin.reader(MD)?, id)?
                    .ok_or_else(|| io::Error::other("not a Markdown document"))?,
            );
        }
        Ok(&self.docs[&id])
    }
    fn crumbs(path: &str, doc: &Parsed, sec: usize) -> String {
        array(doc.breadcrumbs(sec).into_iter().map(|s| {
            object(&[
                ("anchor", quote(&s.anchor)),
                ("handle", quote(&format_handle(path, &s.anchor))),
                ("level", s.level.to_string()),
                ("line", s.line.to_string()),
                ("title", quote(&s.title)),
            ])
        }))
    }
    pub fn hit_crumbs(&mut self, pin: &Pin, file: usize, byte: usize) -> io::Result<String> {
        let doc = self.document(pin, file)?;
        Ok(Self::crumbs(
            pin.files[file].path(),
            doc,
            doc.section_at(byte),
        ))
    }
    pub fn section(&mut self, pin: &Pin, selector: &str, parent: bool) -> io::Result<String> {
        let (path, anchor) =
            parse_handle(selector).ok_or_else(|| io::Error::other("invalid document handle"))?;
        let id = pin
            .files
            .binary_search_by(|f| f.path().cmp(&path))
            .map_err(|_| io::Error::other("unknown document"))?;
        let doc = self.document(pin, id)?;
        let mut i = doc
            .section(anchor.as_deref().unwrap_or(""))
            .ok_or_else(|| io::Error::other("unknown section"))?;
        if parent {
            let Some(p) = doc.sections[i].parent else {
                return Ok("null".into());
            };
            i = p;
        }
        let s = &doc.sections[i];
        let body = pin.source_slice(id, s.start as u64..s.end as u64)?;
        Ok(object(&[
            ("anchor", quote(&s.anchor)),
            ("path", quote(&path)),
            ("title", quote(&s.title)),
            ("level", s.level.to_string()),
            ("line", s.line.to_string()),
            ("start", s.start.to_string()),
            ("end", s.end.to_string()),
            ("handle", quote(&format_handle(&path, &s.anchor))),
            (
                "parent",
                s.parent.map_or_else(
                    || "null".into(),
                    |p| quote(&format_handle(&path, &doc.sections[p].anchor)),
                ),
            ),
            ("breadcrumbs", Self::crumbs(&path, doc, i)),
            ("body", quote(&body)),
        ]))
    }
    pub fn backlinks(&mut self, pin: &Pin, selector: &str) -> io::Result<String> {
        let (path, anchor) =
            parse_handle(selector).ok_or_else(|| io::Error::other("invalid document handle"))?;
        let (matched, truncated) =
            self.lookup
                .backlinks(pin.reader(MD)?, &path, anchor.as_deref())?;
        let mut hits = Vec::with_capacity(matched.len());
        for (f, l) in matched {
            let path = pin.files.get(f).ok_or_else(bad)?.path();
            let doc = self.document(pin, f)?;
            let link = doc.links.get(l).ok_or_else(bad)?;
            hits.push(object(&[
                ("path", quote(path)),
                ("line", link.line.to_string()),
                ("offset", link.offset.to_string()),
                (
                    "handle",
                    quote(&format_handle(path, &doc.sections[link.section].anchor)),
                ),
                ("target", quote(&format_handle(&link.target, &link.anchor))),
                ("breadcrumbs", Self::crumbs(path, doc, link.section)),
            ]));
        }
        Ok(object(&[
            ("target", quote(selector)),
            ("hits", array(hits)),
            ("truncated", truncated.to_string()),
        ]))
    }
}
