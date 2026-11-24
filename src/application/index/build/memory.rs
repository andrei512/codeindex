//! Build only the query products needed during a single plain-folder invocation.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryQuery {
    Paths,
    Content,
    Markdown,
    Rank { breadcrumbs: bool },
    Symbols,
    Relationships,
}

pub(crate) fn memory_index(
    root: &Path,
    paths: Vec<ObservedPath>,
    query: MemoryQuery,
    capture: impl Fn(&str, [u64; 7]) -> io::Result<Option<String>>,
) -> io::Result<(Vec<Entry>, Directory, Vec<u8>)> {
    let graph = matches!(query, MemoryQuery::Symbols | MemoryQuery::Relationships);
    let mut files = Vec::with_capacity(paths.len());
    let mut source = String::new();
    for observed in paths {
        let body = match query {
            MemoryQuery::Paths => None,
            MemoryQuery::Markdown if !markdown::is_markdown(&observed.path) => None,
            _ => match observed.captured {
                Some(body) => body,
                None => capture(&observed.path, observed.stamp)?,
            },
        };
        let bytes = body.map(|text| {
            let start = source.len();
            source.push_str(&text);
            start as u64..source.len() as u64
        });
        files.push(Entry {
            observation: Observation {
                path: observed.path,
                stamp: observed.stamp,
            },
            bytes,
            line_ends: Vec::new(),
            original_stamp: None,
            catalog_row: 0..0,
            blob: None,
            evidence: 0..0,
        });
    }
    let mut dir = Directory::default();
    dir.0[SOURCE - 1] = 0..source.len() as u64;
    let text = |entry: &Entry| {
        entry
            .bytes
            .as_ref()
            .map_or("", |r| &source[r.start as usize..r.end as usize])
    };
    let project = (graph && files.iter().any(|f| c_project::database(f.path()))).then(|| {
        code::CProject::new(
            root,
            files
                .iter()
                .filter(|f| c_project::database(f.path()))
                .map(|f| (f.path(), text(f))),
            |path| {
                let i = files.binary_search_by(|f| f.path().cmp(path)).ok()?;
                Some((files[i].path(), text(&files[i])))
            },
        )
    });
    let locals: Vec<_> = files
        .iter()
        .map(|file| {
            analyze_products(
                file.path(),
                file.bytes.as_ref().map(|_| text(file)),
                project.as_ref(),
                graph,
                !graph,
            )
        })
        .collect();
    drop(project);
    let units: Vec<_> = files
        .iter()
        .zip(&locals)
        .map(|(f, l)| Unit {
            path: f.path(),
            text: text(f),
            analysis: &l.code,
        })
        .collect();
    let mut products = Vec::new();
    let mut write =
        |kind: usize, encode: &mut dyn FnMut(&mut Vec<u8>) -> io::Result<()>| -> io::Result<()> {
            let start = products.len();
            encode(&mut products)?;
            dir.0[kind - 1] = (source.len() + start) as u64..(source.len() + products.len()) as u64;
            Ok(())
        };
    if matches!(
        query,
        MemoryQuery::Content | MemoryQuery::Rank { breadcrumbs: true } | MemoryQuery::Markdown
    ) {
        write(MD, &mut |w| catalog::write_markdown(w, &locals))?;
    }
    if matches!(query, MemoryQuery::Rank { .. }) {
        write(RANK, &mut |w| {
            incremental::ranked(None, &units, &locals, &[], &None)?.write_to(w, None)
        })?;
    }
    if graph {
        let names = if query == MemoryQuery::Relationships {
            ForwardNames::forward(&units)
        } else {
            ForwardNames::declarations(&units)
        };
        let bases = graph::declaration_bases(&units);
        write(SYMBOLS, &mut |w| {
            graph::encode_symbols(&units, &names, &bases, w)
        })?;
        if query == MemoryQuery::Relationships {
            write(REL, &mut |w| {
                graph::encode_relations(&units, &names, &bases, w)
            })?;
        }
    }
    for (f, mut local) in files.iter_mut().zip(locals) {
        if local.partial_line {
            local.lf.pop();
        }
        f.line_ends = local.lf;
    }
    let mut data = source.into_bytes();
    data.extend(products);
    Ok((files, dir, data))
}
