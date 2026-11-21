//! Source ranges for Markdown navigation. Every byte offset refers to the original UTF-8.
mod blocks;
mod inline;
mod storage;
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

impl Parsed {
    pub fn parse(path: &str, text: &str) -> Self {
        blocks::parse(path, text)
    }

    pub fn relocate(&mut self, path: &str) {
        self.path.clear();
        self.path.push_str(path);
        self.sections[0].title = path.rsplit('/').next().unwrap_or("").to_owned();
        self.links.clear();
        let file = std::path::Path::new(path);
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
        self.sections.iter().position(|s| s.anchor == anchor)
    }

    pub fn section_at(&self, offset: usize) -> usize {
        self.sections
            .partition_point(|s| s.start <= offset)
            .saturating_sub(1)
    }

    pub fn breadcrumbs(&self, mut id: usize) -> Vec<&Section> {
        let mut path = Vec::new();
        while let Some(section) = self.sections.get(id) {
            path.push(section);
            let Some(parent) = section.parent else { break };
            id = parent;
        }
        path.reverse();
        path
    }

    pub fn chunks(&self, text: &str) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        for (id, section) in self.sections.iter().enumerate() {
            let stop = self
                .sections
                .get(id + 1)
                .map_or(text.len(), |next| next.start);
            let mut window = section.start;
            let mut line = section.line;
            let mut count = 0;
            let mut cursor = window;
            let mut overlap = window;
            for fragment in text[window..stop].split_inclusive('\n') {
                count += 1;
                cursor += fragment.len();
                if count == 50 {
                    overlap = cursor;
                }
                if count == 60 && cursor < stop {
                    chunks.push(Chunk {
                        section: id,
                        start: window,
                        end: cursor,
                        line,
                    });
                    window = overlap;
                    line += 50;
                    count = 10;
                }
            }
            if window < stop {
                chunks.push(Chunk {
                    section: id,
                    start: window,
                    end: stop,
                    line,
                });
            }
        }
        chunks
    }
}

#[cfg(test)]
mod tests;
