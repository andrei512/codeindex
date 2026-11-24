//! Prove a single file's graph inputs before scheduling unchanged files.
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
    pub end_patch: Option<(u64, u64)>,
}

pub(super) fn same_inputs(p: &dyn Previous, id: usize, unit: &Unit, old: &str) -> io::Result<bool> {
    let Some(lang) = Language::for_path(unit.path) else {
        return Ok(false);
    };
    if !unit.analysis.same_binding_text(lang, old, unit.text) {
        return Ok(false);
    }
    equal_evidence(p, id, unit.analysis)
}

fn equal_evidence(p: &dyn Previous, id: usize, analysis: &crate::domain::code::Analysis) -> io::Result<bool> {
    let range = &p.entries()[id].evidence;
    let mut encoded = Vec::new();
    analysis.encode(&mut encoded);
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
        Some(Language::Rust | Language::Python | Language::Js)
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
    let mut local: Local = analyze(&file.path, Some(text));
    let mut end_patch = None;
    let same_graph = if Language::for_path(&file.path) == Some(Language::Rust) {
        // The Rust binder reads retained parser facts and the spelling at declaration
        // and use spans. A prefix-preserving suffix edit keeps those spellings when
        // the evidence matches, apart from the enclosing source_file end.
        if old_text.len() != text.len()
            && (old_text.starts_with(text) || text.starts_with(&old_text))
            && let Some(last) = local.code.declarations.last_mut()
            && last.kind == "source_file" && last.span == (0..text.len())
        {
            last.span.end = old_text.len();
            let equal = equal_evidence(p, id, &local.code)?;
            local.code.declarations.last_mut().unwrap().span.end = text.len();
            if equal && let Some(at) = p.source_end_word(id, old_text.len())? {
                end_patch = Some((at, text.len() as u64));
            }
        }
        end_patch.is_some()
    } else {
        let unit = Unit { path: &file.path, text, analysis: &local.code };
        same_inputs(p, id, &unit, &old_text)?
    };
    let (prior, blob) = identify(id, file, Some(text), None)?;
    let source = pool.retain(file.captured.take().flatten().expect("captured source"))?;
    Ok(Some(Prepared {
        id,
        old_text,
        same_graph,
        end_patch,
        capture: Captured {
            blob,
            source: Some(source),
            local: Some(local),
            prior,
        },
    }))
}
