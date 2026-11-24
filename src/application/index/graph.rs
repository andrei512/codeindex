use super::binary::{Decoder, bad, put, string};
use super::layout::ExtentSink;
use crate::domain::code::{ForwardNames, Site, Unit};
use std::io::Write;
use std::{io, ops::Range};

pub struct Decl {
    pub file: usize,
    pub span: Range<usize>,
    pub name: String,
    pub kind: String,
    pub label: Option<String>,
}
pub struct Use {
    pub is_call: bool,
    pub file: usize,
    pub span: Range<usize>,
    pub owner: Option<usize>,
    pub targets: Vec<usize>,
}
pub fn declaration_bases(units: &[Unit<'_>]) -> Vec<usize> {
    let mut next = 0;
    units
        .iter()
        .map(|unit| {
            let start = next;
            next += unit.analysis.declarations.len();
            start
        })
        .collect()
}

pub fn encode_symbols(
    units: &[Unit<'_>],
    names: &ForwardNames,
    base: &[usize],
    symbols: &mut impl super::layout::ExtentSink,
) -> io::Result<()> {
    let n: usize = units.iter().map(|u| u.analysis.declarations.len()).sum();
    let start = symbols.offset();
    let mut decl_offsets = Vec::with_capacity(n + 1);
    put(symbols, n)?;
    for (file, u) in units.iter().enumerate() {
        for d in &u.analysis.declarations {
            decl_offsets.push(symbols.offset() - start);
            put(symbols, file)?;
            put(symbols, d.span.start)?;
            put(symbols, d.span.end)?;
            string(
                symbols,
                if d.kind == "source_file" {
                    u.path.rsplit('/').next().unwrap_or(u.path)
                } else {
                    &u.text[d.span.clone()]
                },
            )?;
            string(symbols, d.kind)?;
        }
    }
    // Reopened name lookup directories select declaration ordinals.
    decl_offsets.push(symbols.offset() - start);
    let mut ordered: Vec<_> = names.by_name.iter().collect();
    ordered.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut name_offsets = Vec::with_capacity(ordered.len() + 1);
    put(symbols, ordered.len())?;
    for (name, sites) in ordered {
        name_offsets.push(symbols.offset() - start);
        string(symbols, name)?;
        put(symbols, sites.len())?;
        for s in sites {
            put(symbols, base[s.file] + s.local)?;
        }
    }
    name_offsets.push(symbols.offset() - start);
    super::public_labels::write(symbols, start, base, &names.public_labels)?;
    super::offsets::finish(symbols, start, [decl_offsets, name_offsets])?;
    Ok(())
}
pub fn encode_relations(
    units: &[Unit<'_>],
    names: &ForwardNames,
    base: &[usize],
    rel: &mut Vec<u8>,
) -> io::Result<()> {
    let start = rel.offset();
    let n: usize = units.iter().map(|u| u.analysis.declarations.len()).sum();
    let calls: usize = units.iter().map(|u| u.analysis.calls.len()).sum();
    let mut uses = Vec::with_capacity(calls + 1);
    let mut incoming = Vec::with_capacity(n + 1);
    // Count dense reverse rows once, then scatter in canonical call order.
    let mut cursor = vec![0usize; n];
    for targets in names.all_rows() {
        for t in targets {
            cursor[base[t.file] + t.local] += 1;
        }
    }
    let mut next = 0;
    for count in &mut cursor {
        let len = *count;
        *count = next;
        next += len;
    }
    // Each use contributes 41 record bytes and one 8-byte offset. Each
    // declaration owns a reverse-list length and offset. Each edge appears
    // in both directions. The seven remaining words are counts and sentinels.
    let size = calls
        .saturating_mul(49)
        .saturating_add(n.saturating_mul(16))
        .saturating_add(next.saturating_mul(16))
        .saturating_add(56);
    rel.try_reserve_exact(size).map_err(io::Error::other)?;
    let mut reverse = vec![0u64; next];
    put(rel, calls)?;
    for (file, u) in units.iter().enumerate() {
        for (local, c) in u.analysis.calls.iter().enumerate() {
            let call_index = uses.len() as u64;
            uses.push(rel.offset() - start);
            put(rel, file)?;
            put(rel, c.span.start)?;
            put(rel, c.span.end)?;
            rel.write_all(&[u8::from(c.is_call)])?;
            put(rel, c.owner.map_or(0, |i| base[file] + i + 1))?;
            let targets = names.row(Site { file, local });
            put(rel, targets.len())?;
            for t in targets {
                let id = base[t.file] + t.local;
                put(rel, id)?;
                reverse[cursor[id]] = call_index;
                cursor[id] += 1;
            }
        }
    }
    uses.push(rel.offset() - start);
    put(rel, n)?;
    let mut begin = 0;
    for stop in cursor {
        incoming.push(rel.offset() - start);
        put(rel, stop - begin)?;
        super::offsets::write_words(rel, &reverse[begin..stop])?;
        begin = stop;
    }
    incoming.push(rel.offset() - start);
    super::offsets::finish(rel, start, [uses, incoming])?;
    Ok(())
}
pub fn decode_decl(d: &mut Decoder<'_>, file_count: usize) -> io::Result<Decl> {
    let file = d.num()?;
    if file >= file_count {
        return Err(bad());
    }
    Ok(Decl {
        file,
        span: span(d)?,
        name: d.string()?.into(),
        kind: d.string()?.into(),
        label: None,
    })
}
pub fn decode_use(d: &mut Decoder<'_>, files: usize, decls: usize) -> io::Result<Use> {
    let file = d.num()?;
    if file >= files {
        return Err(bad());
    }
    let span = span(d)?;
    let is_call = match d.take(1)? {
        [0] => false,
        [1] => true,
        _ => return Err(bad()),
    };
    let owner = d.num()?.checked_sub(1);
    if owner.is_some_and(|o| o >= decls) {
        return Err(bad());
    }
    Ok(Use {
        file,
        span,
        is_call,
        owner,
        targets: ids(d, decls)?,
    })
}
fn span(d: &mut Decoder<'_>) -> io::Result<Range<usize>> {
    let r = d.num()?..d.num()?;
    if r.is_empty() {
        return Err(bad());
    }
    Ok(r)
}
pub fn ids(d: &mut Decoder<'_>, bound: usize) -> io::Result<Vec<usize>> {
    let n = d.num()?;
    let mut ids = Vec::new();
    for _ in 0..n {
        let id = d.num()?;
        if id >= bound {
            return Err(bad());
        }
        ids.push(id);
    }
    Ok(ids)
}
