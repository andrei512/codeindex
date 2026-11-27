//! Build only the query products needed during a single plain-folder invocation.
use super::*;
mod bytes;
mod scoring;
pub(crate) use bytes::MemoryBytes;

#[derive(Clone, Copy)]
pub(crate) enum MemoryQuery<'a> {
    Score {
        scope: &'a str,
        test_rules: bool,
        inline: bool,
        policy: &'a code::score::policy::Project,
    },
    Source,
    Paths,
    Content,
    Rank {
        breadcrumbs: bool,
    },
    Symbols,
    Relationships,
}

fn capture_sources(
    paths: Vec<ObservedPath>,
    paths_only: bool,
    capture: impl Fn(&str, [u64; 7]) -> io::Result<Option<String>>,
) -> io::Result<(Vec<Entry>, String)> {
    let mut files = Vec::with_capacity(paths.len());
    let mut source = String::new();
    for mut observed in paths {
        let body = capture_memory_body(&mut observed, &capture, paths_only)?;
        let bytes = body.map(|text| append_source(&mut source, text));
        files.push(entry(observed.path, observed.stamp, bytes));
    }
    Ok((files, source))
}

fn capture_memory_body(
    observed: &mut ObservedPath,
    capture: impl Fn(&str, [u64; 7]) -> io::Result<Option<String>>,
    paths_only: bool,
) -> io::Result<Option<String>> {
    if paths_only {
        return Ok(None);
    }
    observed
        .captured
        .take()
        .map_or_else(|| capture(&observed.path, observed.stamp), Ok)
}

fn append_source(source: &mut String, text: String) -> std::ops::Range<u64> {
    let start = source.len();
    if source.is_empty() {
        *source = text;
    } else {
        source.push_str(&text);
    }
    start as u64..source.len() as u64
}

pub(crate) fn memory_index(
    root: &Path,
    paths: Vec<ObservedPath>,
    query: MemoryQuery,
    capture: impl Fn(&str, [u64; 7]) -> io::Result<Option<String>> + Sync,
) -> io::Result<(Vec<Entry>, Directory, MemoryBytes)> {
    // A translation unit can include an arbitrary repository-relative file.
    // Other file scores need only the requested sources.
    if let MemoryQuery::Score {
        scope,
        test_rules,
        inline,
        policy,
    } = query
    {
        let request = scoring::Request {
            rows: scoring::selected_rows(&paths, scope),
            test_rules,
            inline,
            policy,
        };
        return score_products(request, root, paths, capture);
    }
    let (files, source) = capture_sources(paths, matches!(query, MemoryQuery::Paths), capture)?;
    let mut dir = Directory::default();
    dir.0[SOURCE - 1] = 0..source.len() as u64;
    if matches!(query, MemoryQuery::Source | MemoryQuery::Paths) {
        return Ok((
            files,
            dir,
            MemoryBytes {
                source: source.into_bytes(),
                products: Vec::new(),
            },
        ));
    }
    memory_products(root, (files, source), dir, ProductRoute::select(query))
}

fn memory_products(
    root: &Path,
    (mut files, source): (Vec<Entry>, String),
    dir: Directory,
    route: ProductRoute,
) -> io::Result<(Vec<Entry>, Directory, MemoryBytes)> {
    let text = |entry: &Entry| source_text(&source, entry);
    let project = memory_project(root, &files, &source, route);
    let facts = route.facts();
    let locals: Vec<_> = files
        .iter()
        .map(|file| {
            facts.read(
                file.path(),
                file.bytes.as_ref().map(|_| text(file)),
                project.as_ref(),
            )
        })
        .collect();
    let configured = project.is_some();
    drop(project);
    let units = || {
        files
            .iter()
            .zip(&locals)
            .map(|(f, l)| Unit {
                path: f.path(),
                text: text(f),
                analysis: &l.code,
            })
            .collect::<Vec<_>>()
    };
    let mut products = Products {
        bytes: Vec::new(),
        offset: source.len() as u64,
        directory: dir,
    };
    products.write_route(route, &locals, configured, units)?;
    for (f, local) in files.iter_mut().zip(locals) {
        f.line_ends = local.line_ends;
    }
    Ok((
        files,
        products.directory,
        MemoryBytes {
            source: source.into_bytes(),
            products: products.bytes,
        },
    ))
}

fn score_products(
    request: scoring::Request<'_>,
    root: &Path,
    paths: Vec<ObservedPath>,
    capture: impl Fn(&str, [u64; 7]) -> io::Result<Option<String>> + Sync,
) -> io::Result<(Vec<Entry>, Directory, MemoryBytes)> {
    let (files, dir, source) = if request.needs_project(&paths) {
        request.with_project(root, paths, capture)
    } else {
        request.local(paths, capture)
    }?;
    Ok((
        files,
        dir,
        MemoryBytes {
            source,
            products: Vec::new(),
        },
    ))
}

#[derive(Clone, Copy)]
enum ProductRoute {
    Markdown,
    Rank { breadcrumbs: bool },
    Graph { relationships: bool },
}

impl ProductRoute {
    fn select(query: MemoryQuery<'_>) -> Self {
        match query {
            MemoryQuery::Content => Self::Markdown,
            MemoryQuery::Rank { breadcrumbs } => Self::Rank { breadcrumbs },
            MemoryQuery::Symbols => Self::Graph {
                relationships: false,
            },
            MemoryQuery::Relationships => Self::Graph {
                relationships: true,
            },
            _ => unreachable!("source, path and score queries return before local analysis"),
        }
    }

    fn facts(self) -> Facts {
        match self {
            Self::Graph { .. } => Facts::Code,
            Self::Rank { breadcrumbs: false } => Facts::Lines,
            _ => Facts::Headings,
        }
    }
}

fn source_text<'a>(source: &'a str, file: &Entry) -> &'a str {
    file.bytes.as_ref().map_or("", |range| {
        &source[range.start as usize..range.end as usize]
    })
}

fn memory_project<'a>(
    root: &Path,
    files: &'a [Entry],
    source: &'a str,
    route: ProductRoute,
) -> Option<code::CProject<'a>> {
    if !matches!(route, ProductRoute::Graph { .. })
        || !files.iter().any(|file| c_project::database(file.path()))
    {
        return None;
    }
    Some(code::CProject::new(
        root,
        files
            .iter()
            .filter(|file| c_project::database(file.path()))
            .map(|file| (file.path(), source_text(source, file))),
        |path| {
            let id = files.binary_search_by(|file| file.path().cmp(path)).ok()?;
            Some((files[id].path(), source_text(source, &files[id])))
        },
    ))
}

struct Products {
    bytes: Vec<u8>,
    offset: u64,
    directory: Directory,
}
impl Products {
    fn write_route<'a>(
        &mut self,
        route: ProductRoute,
        locals: &[Local],
        configured: bool,
        units: impl FnOnce() -> Vec<Unit<'a>>,
    ) -> io::Result<()> {
        match route {
            ProductRoute::Markdown => self.write(MD, |out| catalog::write_markdown(out, locals)),
            ProductRoute::Rank { breadcrumbs } => {
                if breadcrumbs {
                    self.write(MD, |out| catalog::write_markdown(out, locals))?;
                }
                self.write(RANK, |out| {
                    memory_rank(&units(), locals, !breadcrumbs)?.write_to(out, None)
                })
            }
            ProductRoute::Graph { relationships } => {
                self.graph(&units(), configured, relationships)
            }
        }
    }

    fn write(
        &mut self,
        kind: usize,
        encode: impl FnOnce(&mut Vec<u8>) -> io::Result<()>,
    ) -> io::Result<()> {
        let start = self.offset + self.bytes.len() as u64;
        encode(&mut self.bytes)?;
        self.directory.0[kind - 1] = start..self.offset + self.bytes.len() as u64;
        Ok(())
    }

    fn graph(&mut self, units: &[Unit], configured: bool, relationships: bool) -> io::Result<()> {
        let projection = if configured {
            code::Projection::new(units)?
        } else {
            code::Projection::default()
        };
        let projected = projection.units(units);
        let units = projected.as_deref().unwrap_or(units);
        let mut names = if relationships {
            ForwardNames::forward(units)
        } else {
            ForwardNames::declarations(units)
        };
        projection.bind(&mut names);
        let bases = graph::declaration_bases(units);
        self.write(SYMBOLS, |w| graph::encode_symbols(units, &names, &bases, w))?;
        if relationships {
            self.write(REL, |w| graph::encode_relations(units, &names, &bases, w))?;
        }
        Ok(())
    }
}

fn memory_rank(
    units: &[Unit],
    locals: &[Local],
    compact_rank: bool,
) -> io::Result<incremental::Rank> {
    if compact_rank {
        incremental::fresh_rank(units, |id| {
            let unit = &units[id];
            crate::domain::ranked::from_sections(
                &locals[id].line_ends,
                locals[id].byte_len,
                |heading| {
                    if markdown::is_markdown(unit.path) {
                        markdown::visit_heading_lines(unit.text, heading);
                    }
                },
            )
        })
    } else {
        incremental::fresh_rank(units, |id| incremental::rank_chunks(&locals[id]))
    }
}

fn entry(path: String, stamp: [u64; 7], bytes: Option<std::ops::Range<u64>>) -> Entry {
    Entry {
        observation: Observation { path, stamp },
        bytes,
        score_marker: false,
        score: None,
        test_score: None,
        line_ends: Vec::new(),
        catalog_row: 0..0,
        original_stamp: None,
        blob: None,
        evidence: 0..0,
    }
}
