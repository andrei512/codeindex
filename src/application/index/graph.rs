use super::binary::{Decoder, bad, put, string};
use crate::domain::code::{Names, Site, Unit};
use std::{collections::BTreeMap, io, ops::Range};

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
pub fn encode_symbols(
    units: &[Unit<'_>],
    names: &Names,
    symbols: &mut impl super::layout::ExtentSink,
) -> io::Result<Vec<usize>> {
    let mut base = Vec::new();
    let mut n = 0;
    for u in units {
        base.push(n);
        n += u.analysis.declarations.len();
    }
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
    let ordered: BTreeMap<_, _> = names.by_name.iter().collect();
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
    super::public_labels::write(symbols, start, &base, &names.public_labels)?;
    super::offsets::finish(symbols, start, [decl_offsets, name_offsets])?;
    Ok(base)
}
pub fn encode_relations(
    units: &[Unit<'_>],
    names: &Names,
    base: &[usize],
    rel: &mut impl super::layout::ExtentSink,
) -> io::Result<()> {
    let start = rel.offset();
    let mut uses = Vec::new();
    let mut incoming = Vec::new();
    let n = units.iter().map(|u| u.analysis.declarations.len()).sum();
    put(rel, units.iter().map(|u| u.analysis.calls.len()).sum())?;
    let mut call_base = Vec::with_capacity(units.len());
    let mut call_index = 0;
    for (file, u) in units.iter().enumerate() {
        call_base.push(call_index);
        for (local, c) in u.analysis.calls.iter().enumerate() {
            uses.push(rel.offset() - start);
            call_index += 1;
            put(rel, file)?;
            put(rel, c.span.start)?;
            put(rel, c.span.end)?;
            rel.write_all(&[u8::from(c.is_call)])?;
            put(rel, c.owner.map_or(0, |i| base[file] + i + 1))?;
            let targets = names
                .targets
                .get(&Site { file, local })
                .map_or(&[][..], Vec::as_slice);
            put(rel, targets.len())?;
            for t in targets {
                put(rel, base[t.file] + t.local)?;
            }
        }
    }
    uses.push(rel.offset() - start);
    put(rel, n)?;
    for (file, u) in units.iter().enumerate() {
        for local in 0..u.analysis.declarations.len() {
            incoming.push(rel.offset() - start);
            let calls = names
                .incoming
                .get(&Site { file, local })
                .map_or(&[][..], Vec::as_slice);
            put(rel, calls.len())?;
            for c in calls {
                put(rel, call_base[c.file] + c.local)?;
            }
        }
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
