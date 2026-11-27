//! Indexed search output. Compact rank and symbol queries retain source sites only.
use super::*;

struct PageOutput<'a> {
    pin: &'a Pin,
    cwd: &'a Path,
    mode: &'a str,
    coverage: Coverage,
    display: &'a Display,
}

pub(super) enum Coverage {
    Text,
    Symbols,
    Paths,
}

impl Coverage {
    fn counts(self, pin: &Pin) -> (Option<usize>, usize) {
        if pin.is_memory() {
            match self {
                Self::Paths => return (None, 0),
                Self::Text => return (Some(pin.files.iter().filter(|file| file.bytes.is_some()).count()), 0),
                Self::Symbols => {}
            }
        }
        let (text, parsed) = pin.coverage_counts();
        (Some(text), parsed)
    }
}

#[derive(Default)]
pub(super) struct Page {
    hits: json::ObjectArray,
    sites: Vec<(usize, usize)>,
    truncated: bool,
    markdown: Option<Markdown>,
}

impl PageQuery<'_> {
    pub(super) fn run(
        self,
        source: &mut QuerySource<'_>,
        mode: &str,
        display: &Display,
        outcome: &mut Outcome,
    ) -> io::Result<String> {
        let cwd = source.cwd;
        let compact = display.compact();
        let (product, mode, coverage) = self.shape(mode, compact);
        let pin = source.index(|| product)?;
        let mut page = Page::default();
        self.collect(&mut page, pin, compact)?;
        page.render(PageOutput { pin, cwd, mode, coverage, display }, outcome)
    }

    fn shape<'a>(&self, mode: &'a str, compact: bool) -> (crate::application::index::build::MemoryQuery<'a>, &'a str, Coverage) {
        use crate::application::index::build::MemoryQuery;
        match self {
            Self::Search(_) => (MemoryQuery::Rank { breadcrumbs: !compact }, "search", Coverage::Text),
            Self::Symbol(_) => (MemoryQuery::Symbols, "symbol", Coverage::Symbols),
            Self::Content(_) | Self::Regex(_) => (MemoryQuery::Content, mode, Coverage::Text),
            Self::Paths(_) => (MemoryQuery::Paths, mode, Coverage::Paths),
        }
    }

    fn collect(self, page: &mut Page, pin: &Pin, compact: bool) -> io::Result<()> {
        match self {
            Self::Search(q) => page.ranked(pin, q, compact),
            Self::Symbol(q) => page.symbols(pin, q, compact),
            Self::Content(q) => page.content(pin, q, |page, id, text| {
                page.locations(pin, id, text.match_indices(q).map(|(at, _)| at))
            }),
            Self::Regex(re) => {
                let mut scratch = Scratch::default();
                page.content(pin, re.required_literal(), |page, id, text| {
                    page.locations(pin, id, re.matches_with(text, &mut scratch).map(|hit| hit.start))
                })
            }
            Self::Paths(query) => query.collect(pin, page),
        }
    }
}

impl Page {
    pub(super) fn ranked(&mut self, pin: &Pin, q: &str, compact: bool) -> io::Result<()> {
        if compact {
            let (sites, extra) = ranked_rows(pin, q, |h| (h.file, h.anchor()))?;
            self.sites = sites;
            self.truncated = extra;
            return Ok(());
        }
        let (rows, extra) = ranked_rows(pin, q, |h| {
            (h.file, h.line, h.last_line, h.score, h.excerpt())
        })?;
        self.truncated = extra;
        for (file, line, last_line, score, excerpt) in rows {
            let entry = &pin.files[file];
            let crumbs = breadcrumbs(
                pin,
                &mut self.markdown,
                file,
                entry
                    .line_start(line)
                    .ok_or_else(|| io::Error::other("bad chunk line"))?,
            )?;
            self.hits.push(|row| {
                row.string("path", entry.path());
                row.number("line", line);
                row.number("last_line", last_line);
                row.decimal("score", score);
                row.string("excerpt", &excerpt);
                add_breadcrumbs(row, crumbs.as_deref());
            });
        }
        Ok(())
    }
    pub(super) fn symbols(&mut self, pin: &Pin, q: &str, compact: bool) -> io::Result<()> {
        let mut positions = display::Positions::default();
        let mut lookup = crate::application::index::symbol_lookup::SymbolLookup::open(
            pin.reader(SYMBOLS)?,
            pin.files.len(),
        )?;
        self.truncated = lookup.visit_named(q, 50, |s| {
            if compact {
                self.sites.push((s.file, s.span.start));
                return Ok(());
            }
            let f = &pin.files[s.file];
            let position = positions.position(pin, s.file, s.span.start)?;
            self.hits.push(|row| {
                row.string("path", f.path());
                row.number("line", position.line);
                row.number("byte_column", position.byte_column);
                row.number("col", position.utf16_column);
                row.string("name", s.name);
                row.string("kind", s.kind);
            });
            Ok(())
        })?;
        Ok(())
    }

    pub(super) fn content(
        &mut self,
        pin: &Pin,
        literal: &str,
        mut visit: impl FnMut(&mut Self, usize, &str) -> io::Result<()>,
    ) -> io::Result<()> {
        let ids = candidates(pin, CONTENT, literal)?;
        let mut scan = (ids.len() == pin.files.len()).then(|| pin.source_window());
        let ids = ids.into_iter().filter(|&id| pin.files[id].bytes.is_some());
        for id in ids {
            let text = scan.as_mut().map_or_else(
                || pin.source_text(id),
                |window| window.get(id).map(std::borrow::Cow::Borrowed),
            )?;
            visit(self, id, &text)?;
            if self.truncated { break; }
        }
        Ok(())
    }

    pub(super) fn locations(
        &mut self,
        pin: &Pin,
        id: usize,
        bytes: impl Iterator<Item = usize>,
    ) -> io::Result<()> {
        let entry = &pin.files[id];
        for byte in bytes {
            if self.full() {
                break;
            }
            let (line, column) = entry.line_column(byte);
            let crumbs = breadcrumbs(pin, &mut self.markdown, id, byte)?;
            self.hits.push(|row| {
                row.string("path", entry.path());
                row.number("line", line);
                row.number("byte_column", column);
                add_breadcrumbs(row, crumbs.as_deref());
            });
        }
        Ok(())
    }

    fn full(&mut self) -> bool {
        self.truncated = self.hits.len == 50;
        self.truncated
    }

    pub(super) fn path(&mut self, path: &str) {
        self.hits.push(|row| row.string("path", path));
    }

    pub(super) fn paths(
        &mut self,
        pin: &Pin,
        ids: impl IntoIterator<Item = usize>,
        mut matches: impl FnMut(&str) -> bool,
    ) {
        for path in ids
            .into_iter()
            .map(|i| pin.files[i].path())
            .filter(|path| matches(path))
        {
            if self.full() {
                break;
            }
            self.path(path);
        }
    }

    fn render(self, output: PageOutput<'_>, outcome: &mut Outcome) -> io::Result<String> {
        let PageOutput { pin, cwd, mode, coverage, display } = output;
        if display.public() {
            outcome.code = u8::from(self.hits.len == 0 && self.sites.is_empty());
        }
        if let Some(context) = display.context() {
            if self.truncated {
                outcome.diagnostics.push_str("cx: results truncated\n");
            }
            return display::TextOutput { pin, cwd, context, ranked: mode == "search" }.render(self.sites);
        }
        let (text_files, parsed_files) = coverage.counts(pin);
        Ok(object(&[
            ("root", quote(&pin.root.to_string_lossy())),
            ("mode", quote(mode)),
            ("generation", pin.generation.to_string()),
            ("reused", pin.reused.to_string()),
            (
                "coverage",
                object(&[
                    ("paths", pin.files.len().to_string()),
                    ("text_files", text_files.map_or_else(|| "null".into(), |n| n.to_string())),
                    ("parsed_files", parsed_files.to_string()),
                    ("files_with_parse_errors", "null".into()),
                    ("skipped_paths", "null".into()),
                    ("symbols", quote("partial")),
                ]),
            ),
            ("relationship_status", quote("partial")),
            ("truncated", self.truncated.to_string()),
            ("hits", self.hits.finish()),
        ]))
    }
}

fn add_breadcrumbs(row: &mut json::Object<&mut String>, crumbs: Option<&str>) {
    if let Some(crumbs) = crumbs {
        row.raw("breadcrumbs", crumbs);
    }
}

fn ranked_rows<T>(
    pin: &Pin,
    query: &str,
    project: impl Fn(crate::domain::ranked::MatchedChunk<'_>) -> T,
) -> io::Result<(Vec<T>, bool)> {
    RankedIndex::project_extent(
        pin.reader(RANK)?,
        crate::domain::ranked::ExtentCatalog {
            count: pin.files.len(),
            path: |i: usize| pin.files[i].path(),
            source: |i, r| pin.source_slice(i, r),
        },
        query,
        project,
    )
}
