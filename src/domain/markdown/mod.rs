//! Source ranges for Markdown navigation. Every byte offset refers to the original UTF-8.
mod blocks;
pub(crate) use blocks::{CommentLines, visit_heading_lines};
mod inline;
mod reference;
mod storage;
pub(crate) use storage::{read_headings, read_selected_links};
mod target;
pub use target::{format_handle, parse_handle};
mod entities;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub anchor: String,
    pub title: String,
    pub level: usize,
    pub parent: Option<usize>,
    pub start: usize,
    pub end: usize,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: String,
    pub anchor: String,
    pub offset: usize,
    pub line: usize,
    pub section: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub section: usize,
    pub start: usize,
    pub end: usize,
    pub line: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub sections: Vec<Section>,
    pub links: Vec<Link>,
    path: String,
    // Decoded local addresses before qualification. Even an address that
    // escapes today's worktree can become a valid backlink after a move.
    destinations: Vec<Link>,
}

pub fn is_markdown(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| {
        ["md", "markdown", "mdown", "mkd"]
            .iter()
            .any(|s| ext.eq_ignore_ascii_case(s))
    })
}

/// Section starts and rendered titles used by standalone maintainability queries.
pub(crate) fn score_sections(path: &str, text: &str) -> Vec<Section> {
    blocks::parse::<false>(path, text, 0, |_| None).sections
}

pub(crate) trait SectionLookup {
    fn section(&self, anchor: &str) -> Option<usize>;
    fn section_at(&self, offset: usize) -> usize;
    fn breadcrumbs(&self, id: usize) -> Vec<&Section>;
}

impl SectionLookup for [Section] {
    fn section(&self, anchor: &str) -> Option<usize> {
        self.iter().position(|s| s.anchor == anchor)
    }

    fn section_at(&self, offset: usize) -> usize {
        self
            .partition_point(|s| s.start <= offset)
            .saturating_sub(1)
    }

    fn breadcrumbs(&self, mut id: usize) -> Vec<&Section> {
        let mut path = Vec::new();
        while let Some(section) = self.get(id) {
            path.push(section);
            let Some(parent) = section.parent else { break };
            id = parent;
        }
        path.reverse();
        path
    }
}

impl Parsed {
    pub fn parse(path: &str, text: &str) -> Self {
        let mut doc = blocks::parse::<true>(path, text, usize::MAX, |url| url.local_address());
        doc.resolve_links();
        doc
    }

    /// A one-shot query needs qualified links only for its requested destination.
    pub(crate) fn backlinks(
        path: &str,
        text: &str,
        destination: (&str, Option<&str>),
        limit: usize,
    ) -> (Vec<Section>, Vec<Link>) {
        let (target, anchor) = destination;
        let file = std::path::Path::new(path);
        let doc = blocks::parse::<true>(path, text, limit, |url| {
            let (address, fragment) = url.local_address()?;
            if anchor.is_some_and(|a| a != fragment) {
                return None;
            }
            let qualified = target::qualify(file, &address)?;
            if qualified != target {
                return None;
            }
            Some((qualified, fragment))
        });
        (doc.sections, doc.destinations)
    }

    /// Section handles and display titles, without backlink records.
    pub(crate) fn headings(path: &str, text: &str) -> Self {
        blocks::parse::<true>(path, text, 0, |_| None)
    }

    pub fn relocate(&mut self, path: &str) {
        self.path.clear();
        self.path.push_str(path);
        path.rsplit('/').next().unwrap_or("").clone_into(&mut self.sections[0].title);
        self.resolve_links();
    }

    fn resolve_links(&mut self) {
        self.links.clear();
        let file = std::path::Path::new(&self.path);
        for link in &self.destinations {
            if let Some(target) = target::qualify(file, &link.target) {
                self.links.push(Link {
                    target,
                    anchor: link.anchor.clone(),
                    offset: link.offset,
                    line: link.line,
                    section: link.section,
                });
            }
        }
    }

    pub fn section(&self, anchor: &str) -> Option<usize> {
        self.sections.section(anchor)
    }

    pub fn section_at(&self, offset: usize) -> usize {
        self.sections.section_at(offset)
    }

    pub fn breadcrumbs(&self, id: usize) -> Vec<&Section> {
        self.sections.breadcrumbs(id)
    }

    pub fn chunks(&self, text: &str) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        for (id, section) in self.sections.iter().enumerate() {
            let stop = self
                .sections
                .get(id + 1)
                .map_or(text.len(), |next| next.start);
            section.append_chunks(id, text, stop, &mut chunks);
        }
        chunks
    }
}

impl Section {
    fn append_chunks(&self, id: usize, text: &str, stop: usize, chunks: &mut Vec<Chunk>) {
        use super::ranked::{CHUNK_LINES, CHUNK_STRIDE};
        let mut chunk = Chunk { section: id, start: self.start, end: self.start, line: self.line };
        let mut overlap = self.start;
        let mut count = 0;
        for fragment in text[self.start..stop].split_inclusive('\n') {
            chunk.end += fragment.len();
            count += 1;
            match count {
                CHUNK_STRIDE => overlap = chunk.end,
                CHUNK_LINES if chunk.end < stop => {
                    chunks.push(chunk.clone());
                    chunk.start = overlap;
                    chunk.line += CHUNK_STRIDE;
                    count -= CHUNK_STRIDE;
                }
                _ => {}
            }
        }
        if chunk.start < stop {
            chunks.push(chunk);
        }
    }
}

#[cfg(test)]
mod tests;
