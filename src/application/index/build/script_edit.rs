//! Prove a script edit's graph inputs before scheduling unchanged files.
use super::{Captured, Identify, Local, ObservedPath, Previous, analyze, source::Pool};
use crate::{
    application::index::layout::{REL, SOURCE, SYMBOLS},
    domain::code::{Language, Unit},
};
use std::io;

pub(super) struct Prepared {
    pub id: usize,
    pub old_text: String,
    pub capture: Captured,
    pub same_graph: bool,
}

pub(super) fn same_inputs(p: &dyn Previous, id: usize, unit: &Unit, old: &str) -> io::Result<bool> {
    let Some(lang @ (Language::Python | Language::Js)) = Language::for_path(unit.path) else {
        return Ok(false);
    };
    if !unit.analysis.same_binding_text(lang, old, unit.text) {
        return Ok(false);
    }
    let range = &p.entries()[id].evidence;
    let mut encoded = Vec::new();
    unit.analysis.encode(&mut encoded);
    if range.end - range.start != encoded.len() as u64 {
        return Ok(false);
    }
    let mut previous = Vec::with_capacity(encoded.len());
    p.copy_bytes(range.clone(), &mut previous)?;
    Ok(previous == encoded)
}

pub(super) fn prepare(
    p: &dyn Previous,
    obs: &mut [ObservedPath],
    reuse: &[Option<usize>],
    pool: &Pool,
    allowance: usize,
    identify: &Identify,
) -> io::Result<Option<Prepared>> {
    if !p.compatible(SYMBOLS) || !p.compatible(REL) {
        return Ok(None);
    }
    let mut changed = reuse
        .iter()
        .enumerate()
        .filter(|(_, prior)| prior.is_none());
    let Some((id, _)) = changed.next() else {
        return Ok(None);
    };
    if changed.next().is_some() {
        return Ok(None);
    }
    let file = &mut obs[id];
    let Some(text) = file.captured.as_ref().and_then(Option::as_ref) else {
        return Ok(None);
    };
    if !matches!(
        Language::for_path(&file.path),
        Some(Language::Python | Language::Js)
    ) {
        return Ok(None);
    }
    let Some(range) = &p.entries()[id].bytes else {
        return Ok(None);
    };
    let old_size = range.end - range.start;
    if old_size.saturating_add(text.len() as u64) > allowance as u64 {
        return Ok(None);
    }
    let base = p.extent(SOURCE).start;
    let mut bytes = Vec::with_capacity(old_size as usize);
    p.copy_bytes(base + range.start..base + range.end, &mut bytes)?;
    let old_text = String::from_utf8(bytes).map_err(io::Error::other)?;
    let local: Local = analyze(&file.path, Some(text));
    let unit = Unit {
        path: &file.path,
        text,
        analysis: &local.code,
    };
    let same_graph = same_inputs(p, id, &unit, &old_text)?;
    let (prior, blob) = identify(id, file, Some(text), None)?;
    let source = pool.retain(file.captured.take().flatten().expect("captured script"))?;
    Ok(Some(Prepared {
        id,
        old_text,
        same_graph,
        capture: Captured {
            blob,
            source: Some(source),
            local: Some(local),
            prior,
        },
    }))
}
