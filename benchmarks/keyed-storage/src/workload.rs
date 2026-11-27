//! Generate both states with the real parsers and binder, then diff identical query projections.
use crate::{
    store::Result,
    tree::{Edits, Rows},
};
use codeindex::domain::{
    code::{self, Analysis, Language, Names, Unit},
    markdown,
};
use std::{collections::BTreeMap, fs, path::Path, process::Command};
type Sources = BTreeMap<String, String>;
fn sources(repo: &Path) -> Result<Sources> {
    let files = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(repo)
        .output()?;
    if !files.status.success() {
        return Err("git ls-files failed".into());
    }
    let mut inputs = Sources::new();
    let names = files.stdout.split(|b| *b == 0).filter(|x| !x.is_empty())
        .filter_map(|name| std::str::from_utf8(name).ok());
    for name in names {
        if let Some(text) = regular_utf8_source(&repo.join(name))? {
            inputs.insert(name.into(), text);
        }
    }
    Ok(inputs)
}
fn regular_utf8_source(path: &Path) -> Result<Option<String>> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Ok(None);
    }
    Ok(fs::read_to_string(path).ok())
}

fn key(kind: u8, parts: &[&str], slot: usize) -> Vec<u8> {
    let mut key = vec![kind];
    for part in parts {
        for b in part.bytes() {
            if b == 0 {
                key.extend([0, 255])
            } else {
                key.push(b)
            }
        }
        key.extend([0, 0]);
    }
    key.extend((slot as u64).to_be_bytes());
    key
}
fn location(start: usize, end: usize) -> Vec<u8> {
    [start as u64, end as u64]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect()
}
fn project(inputs: &Sources) -> Rows {
    let facts: Vec<Analysis> = inputs
        .iter()
        .map(|(p, s)| Language::for_path(p).map_or_else(Analysis::default, |l| code::analyze(l, s)))
        .collect();
    let units: Vec<_> = inputs
        .iter()
        .zip(&facts)
        .map(|((path, text), analysis)| Unit {
            path,
            text,
            analysis,
        })
        .collect();
    let bindings = Names::build(&units);
    let mut rows = Rows::new();
    for unit in &units {
        project_declarations(unit, &mut rows);
        project_markdown(unit, &mut rows);
    }
    project_references(&units, bindings, &mut rows);
    rows
}

fn project_declarations(unit: &Unit<'_>, rows: &mut Rows) {
    for (slot, declaration) in unit.analysis.declarations.iter().enumerate() {
        rows.insert(
            key(b'D', &[unit.path], slot),
            location(declaration.span.start, declaration.span.end),
        );
    }
    for (slot, declaration) in unit.analysis.declarations.iter().enumerate().filter(|(_, d)| d.searchable()) {
        let name = declaration_name(unit, declaration);
        rows.insert(key(b'N', &[name, unit.path], slot), declaration.kind.as_bytes().to_vec());
    }
}

fn declaration_name<'a>(unit: &Unit<'a>, declaration: &code::Declaration) -> &'a str {
    if declaration.kind == "source_file" {
        return unit.path.rsplit('/').next().unwrap();
    }
    &unit.text[declaration.span.clone()]
}

fn project_markdown(unit: &Unit<'_>, rows: &mut Rows) {
    if !markdown::is_markdown(unit.path) {
        return;
    }
    let doc = markdown::Parsed::parse(unit.path, unit.text);
    for (slot, heading) in doc.sections.iter().enumerate() {
        rows.insert(
            key(b'H', &[unit.path, &heading.anchor], slot),
            location(heading.start, heading.end),
        );
    }
    for (slot, link) in doc.links.iter().enumerate() {
        rows.insert(
            key(b'L', &[&link.target, &link.anchor, unit.path], slot),
            location(link.offset, link.section),
        );
    }
}

fn project_references(units: &[Unit<'_>], bindings: Names, rows: &mut Rows) {
    for (target, uses) in bindings.incoming {
        for site in uses {
            let source = &units[site.file];
            let call = &source.analysis.calls[site.local];
            let mut k = key(b'R', &[units[target.file].path], target.local);
            k.extend(key(b'U', &[source.path], site.local));
            rows.insert(k, location(call.span.start, call.span.end));
        }
    }
}
pub struct Trace {
    pub initial: Rows,
    pub edits: Edits,
    pub undo: Edits,
}
pub fn trace(repo: &Path) -> Result<Trace> {
    let mut files = sources(repo)?;
    for (name, body) in [
        (
            "__cx_replay__/consumer.ts",
            "import { hook } from './producer';\nhook();\n",
        ),
        (
            "__cx_replay__/producer.ts",
            "export function existing() {}\n",
        ),
        (
            "__cx_replay__/doc.md",
            "# Storage\n\n[See guide](guide.md#first)\n",
        ),
    ] {
        if files.insert(name.into(), body.into()).is_some() {
            return Err("fixture name collision".into());
        }
    }
    let initial = project(&files);
    files
        .get_mut("__cx_replay__/producer.ts")
        .unwrap()
        .push_str("export function hook() {}\n");
    files
        .get_mut("__cx_replay__/doc.md")
        .unwrap()
        .push_str("\n## Details\n\n[Another link](guide.md#second)\n");
    let after = project(&files);
    let (edits, undo) = row_changes(&initial, &after);
    assert!(
        [b'R', b'D', b'N', b'L', b'H']
            .iter()
            .all(|t| edits.keys().any(|k| k[0] == *t))
    );
    Ok(Trace {
        initial,
        edits,
        undo,
    })
}

fn row_changes(initial: &Rows, after: &Rows) -> (Edits, Edits) {
    let changed = initial.keys().chain(after.keys()).filter_map(|key| {
        let (old, new) = (initial.get(key), after.get(key));
        (old != new).then_some((key, old, new))
    });
    let (mut edits, mut undo) = (Edits::new(), Edits::new());
    for (key, old, new) in changed {
        edits.insert(key.clone(), new.cloned());
        undo.insert(key.clone(), old.cloned());
    }
    (edits, undo)
}
