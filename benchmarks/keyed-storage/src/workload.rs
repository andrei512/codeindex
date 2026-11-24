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
    for name in files.stdout.split(|b| *b == 0).filter(|x| !x.is_empty()) {
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        let path = repo.join(name);
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file() {
            continue;
        }
        if let Ok(text) = fs::read_to_string(path) {
            inputs.insert(name.into(), text);
        }
    }
    Ok(inputs)
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
        for (slot, d) in unit.analysis.declarations.iter().enumerate() {
            let name = if d.kind == "source_file" {
                unit.path.rsplit('/').next().unwrap()
            } else {
                &unit.text[d.span.clone()]
            };
            rows.insert(
                key(b'D', &[unit.path], slot),
                location(d.span.start, d.span.end),
            );
            if d.searchable() {
                rows.insert(
                    key(b'N', &[name, unit.path], slot),
                    d.kind.as_bytes().to_vec(),
                );
            }
        }
        if markdown::is_markdown(unit.path) {
            let doc = markdown::Parsed::parse(unit.path, unit.text);
            for (slot, h) in doc.sections.iter().enumerate() {
                rows.insert(
                    key(b'H', &[unit.path, &h.anchor], slot),
                    location(h.start, h.end),
                );
            }
            for (slot, l) in doc.links.iter().enumerate() {
                rows.insert(
                    key(b'L', &[&l.target, &l.anchor, unit.path], slot),
                    location(l.offset, l.section),
                );
            }
        }
    }
    for (target, uses) in bindings.incoming {
        for site in uses {
            let source = &units[site.file];
            let call = &source.analysis.calls[site.local];
            let mut k = key(b'R', &[units[target.file].path], target.local);
            k.extend(key(b'U', &[source.path], site.local));
            rows.insert(k, location(call.span.start, call.span.end));
        }
    }
    rows
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
    let (mut edits, mut undo) = (Edits::new(), Edits::new());
    for k in initial.keys().chain(after.keys()) {
        if initial.get(k) != after.get(k) {
            edits.insert(k.clone(), after.get(k).cloned());
            undo.insert(k.clone(), initial.get(k).cloned());
        }
    }
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
