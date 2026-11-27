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

fn declaration_name<'a>(unit: &Unit<'a>, declaration: &crate::domain::code::Declaration) -> &'a str {
    if declaration.kind == "source_file" {
        return unit.path.rsplit('/').next().unwrap_or(unit.path);
    }
    &unit.text[declaration.span.clone()]
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
    let declarations = units.iter().enumerate().flat_map(|(file, unit)| {
        unit.analysis.declarations.iter().map(move |declaration| (file, unit, declaration))
    });
    for (file, unit, declaration) in declarations {
        decl_offsets.push(symbols.offset() - start);
        put(symbols, file)?;
        put(symbols, declaration.span.start)?;
        put(symbols, declaration.span.end)?;
        string(symbols, declaration_name(unit, declaration))?;
        string(symbols, declaration.kind)?;
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
        sites.iter().try_for_each(|site| put(symbols, base[site.file] + site.local))?;
    }
    name_offsets.push(symbols.offset() - start);
    super::public_labels::write(symbols, start, base, &names.public_labels)?;
    super::offsets::finish(symbols, start, [decl_offsets, name_offsets])?;
    Ok(())
}
/// Dense reverse lists share one allocation. While calls are emitted, each
/// declaration's cursor advances from its list start to its final end.
struct IncomingEdges {
    calls: Vec<u64>,
    ends: Vec<usize>,
}

impl IncomingEdges {
    fn new(n: usize, names: &ForwardNames, base: &[usize]) -> Self {
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
        Self {
            calls: vec![0; next],
            ends: cursor,
        }
    }
    fn record(&mut self, id: usize, call: u64) {
        self.calls[self.ends[id]] = call;
        self.ends[id] += 1;
    }
    fn write(
        self,
        rel: &mut Vec<u8>,
        start: u64,
        base: &[usize],
        names: &ForwardNames,
    ) -> io::Result<Vec<u64>> {
        let mut incoming = Vec::with_capacity(self.ends.len() + 1);
        put(rel, self.ends.len())?;
        let mut begin = 0;
        let contexts: std::collections::HashMap<_, _> = names
            .translation_definitions
            .iter()
            .map(|(site, ctx)| (base[site.file] + site.local, ctx))
            .collect();
        for (decl, stop) in self.ends.into_iter().enumerate() {
            incoming.push(rel.offset() - start);
            put(rel, stop - begin)?;
            super::offsets::write_words(rel, &self.calls[begin..stop])?;
            if let Some(ids) = contexts.get(&decl) {
                put(rel, ids.len())?;
                super::offsets::write_positions(rel, ids)?;
            }
            begin = stop;
        }
        incoming.push(rel.offset() - start);
        Ok(incoming)
    }
}

struct UseRecord<'a> {
    location: UseLocation,
    targets: &'a [Site],
}

impl<'a> UseRecord<'a> {
    fn new(
        (span, is_call, lexical_owner): (Range<usize>, bool, Option<usize>),
        site: Site,
        names: &'a ForwardNames,
        bases: &[usize],
    ) -> Self {
        let translation = (!names.translation_uses.is_empty())
            .then(|| names.translation_uses.get(&site)).flatten();
        let context = translation.map(|(context, _)| *context);
        let owner = translation.and_then(|(_, owner)| *owner)
            .or_else(|| lexical_owner.map(|local| Site { file: site.file, local }));
        let location = UseLocation {
            context,
            file: site.file,
            span,
            is_call,
            owner: owner.map(|label| bases[label.file] + label.local),
        };
        Self { location, targets: names.row(site) }
    }

    fn write(&self, rel: &mut Vec<u8>, edges: &mut IncomingEdges, bases: &[usize], call_index: u64) -> io::Result<()> {
        let location = &self.location;
        put(rel, location.file)?;
        put(rel, location.span.start)?;
        put(rel, location.span.end)?;
        let tag = u8::from(location.is_call) | (u8::from(location.context.is_some()) << 1);
        rel.write_all(&[tag])?;
        if let Some(context) = location.context {
            put(rel, context)?;
        }
        put(rel, location.owner.map_or(0, |owner| owner + 1))?;
        put(rel, self.targets.len())?;
        for target in self.targets {
            let id = bases[target.file] + target.local;
            put(rel, id)?;
            edges.record(id, call_index);
        }
        Ok(())
    }
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
    let mut reverse = IncomingEdges::new(n, names, base);
    // Each use contributes 41 record bytes and one 8-byte offset. Each
    // declaration owns a reverse-list length and offset. Each edge appears
    // in both directions. The seven remaining words are counts and sentinels.
    let size = calls
        .saturating_mul(49)
        .saturating_add(n.saturating_mul(16))
        .saturating_add(reverse.calls.len().saturating_mul(16))
        .saturating_add(56);
    rel.try_reserve_exact(size).map_err(io::Error::other)?;
    put(rel, calls)?;
    let calls = units.iter().enumerate().flat_map(|(file, unit)| {
        unit.analysis.calls.iter().enumerate().map(move |(local, call)| (Site { file, local }, call))
    });
    for (site, call) in calls {
        let index = uses.len() as u64;
        uses.push(rel.offset() - start);
        let record = UseRecord::new((call.span.clone(), call.is_call, call.owner), site, names, base);
        record.write(rel, &mut reverse, base, index)?;
    }
    uses.push(rel.offset() - start);
    let incoming = reverse.write(rel, start, base, names)?;
    if names.context_keys.is_empty() {
        return super::offsets::finish(rel, start, [uses, incoming]).map(|_| ());
    }
    let keys = context_offsets(rel, start, &names.context_keys)?;
    super::offsets::finish(rel, start, [uses, incoming, keys]).map(|_| ())
}

fn context_offsets(rel: &mut Vec<u8>, start: u64, context_keys: &[String]) -> io::Result<Vec<u64>> {
    let mut offsets = Vec::with_capacity(context_keys.len() + 1);
    for key in context_keys {
        offsets.push(rel.offset() - start);
        string(rel, key)?;
    }
    offsets.push(rel.offset() - start);
    Ok(offsets)
}

pub(crate) struct DeclRef<'a> {
    pub file: usize,
    pub span: Range<usize>,
    pub name: &'a str,
    pub kind: &'a str,
}

impl<'a> DeclRef<'a> {
    pub fn read(d: &mut Decoder<'a>, file_count: usize) -> io::Result<Self> {
        let file = d.num()?;
        if file >= file_count {
            return Err(bad());
        }
        Ok(Self {
            file,
            span: span(d)?,
            name: d.string()?,
            kind: d.string()?,
        })
    }

    pub fn into_owned(self) -> Decl {
        Decl {
            file: self.file,
            span: self.span,
            name: self.name.to_owned(),
            kind: self.kind.to_owned(),
            label: None,
        }
    }
}
pub(crate) struct UseLocation {
    pub context: Option<usize>,
    pub file: usize,
    pub span: Range<usize>,
    pub is_call: bool,
    pub owner: Option<usize>,
}
pub(super) fn decode_location(
    d: &mut Decoder<'_>,
    files: usize,
    decls: usize,
    contexts: usize,
) -> io::Result<UseLocation> {
    let file = d.num()?;
    if file >= files {
        return Err(bad());
    }
    let span = span(d)?;
    let tag = d.take(1)?[0];
    if tag > 3 {
        return Err(bad());
    }
    let context = (tag & 2 != 0).then(|| d.num()).transpose()?;
    if context.is_some_and(|c| c >= contexts) {
        return Err(bad());
    }
    let is_call = tag & 1 != 0;
    let owner = d.num()?.checked_sub(1);
    if owner.is_some_and(|o| o >= decls) {
        return Err(bad());
    }
    Ok(UseLocation {
        context,
        file,
        span,
        is_call,
        owner,
    })
}
fn span(d: &mut Decoder<'_>) -> io::Result<Range<usize>> {
    let r = d.num()?..d.num()?;
    if r.is_empty() {
        return Err(bad());
    }
    Ok(r)
}
pub(super) fn visit_ids(
    d: &mut Decoder<'_>,
    bound: usize,
    mut visit: impl FnMut(usize),
) -> io::Result<()> {
    for word in d.words()? {
        let id = usize::try_from(u64::from_le_bytes(*word)).map_err(|_| bad())?;
        if id >= bound {
            return Err(bad());
        }
        visit(id);
    }
    Ok(())
}
