//! Keep unchanged document records and reverse-address lists in the predecessor.
use super::{
    Entry, Local, Previous,
    retained::{self, Run},
};
use crate::{
    application::index::{
        binary::{Decoder, bad, put, string},
        layout::{ExtentSink, MD},
        markdown_lookup, offsets,
    },
    domain::markdown::Parsed,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    ops::Range,
};

type Key = (String, String);
type Site = (usize, usize);
#[derive(Default)]
struct Changes {
    remove: Vec<Site>,
    add: Vec<Site>,
}
impl Changes {
    fn apply(self, old_sites: Option<&[u8]>) -> io::Result<BTreeSet<Site>> {
        let mut sites = match old_sites {
            Some(bytes) => decode_sites(bytes)?,
            None => BTreeSet::new(),
        };
        let removed = self.remove.into_iter().all(|site| sites.remove(&site));
        if !removed || !self.add.into_iter().all(|site| sites.insert(site)) {
            return Err(bad());
        }
        Ok(sites)
    }

    fn replace(self, prefix: &[u8], old_sites: Option<&[u8]>) -> io::Result<Vec<u8>> {
        let sites = self.apply(old_sites)?;
        if sites.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = prefix.to_vec();
        put(&mut out, sites.len())?;
        for (f, l) in sites {
            put(&mut out, f)?;
            put(&mut out, l)?;
        }
        Ok(out)
    }
}

fn decode_sites(bytes: &[u8]) -> io::Result<BTreeSet<Site>> {
    let mut d = Decoder(bytes);
    let mut sites = BTreeSet::new();
    for _ in 0..d.num()? {
        if !sites.insert((d.num()?, d.num()?)) {
            return Err(bad());
        }
    }
    d.end()?;
    Ok(sites)
}

fn read(p: &dyn Previous, r: Range<u64>) -> io::Result<Vec<u8>> {
    let mut b = Vec::new();
    p.copy_bytes(r, &mut b)?;
    Ok(b)
}
fn table(d: &mut Decoder, limit: u64) -> io::Result<Vec<u64>> {
    let n = d.num()?;
    if n == 0 {
        return Err(bad());
    }
    let raw = d.take(n.checked_mul(8).ok_or_else(bad)?)?;
    let values = raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| u64::from_le_bytes(*b))
        .collect::<Vec<_>>();
    if !values.is_sorted() || values[n - 1] > limit {
        return Err(bad());
    }
    Ok(values)
}
struct PriorRecord<'a> {
    prior: &'a dyn Previous,
    end: u64,
}
impl crate::domain::content_lookup::ExtentRead for PriorRecord<'_> {
    fn length(&self) -> u64 {
        self.end
    }
    fn read(&mut self, at: u64, mut dest: &mut [u8]) -> io::Result<()> {
        let end = at
            .checked_add(dest.len() as u64)
            .filter(|&n| n <= self.end)
            .ok_or_else(bad)?;
        self.prior.copy_bytes(at..end, &mut dest)?;
        if !dest.is_empty() {
            return Err(bad());
        }
        Ok(())
    }
}
fn keys(m: Option<&Parsed>) -> impl ExactSizeIterator<Item = (&str, &str)> {
    m.map_or(&[][..], |m| m.links.as_slice())
        .iter()
        .map(|l| (l.target.as_str(), l.anchor.as_str()))
}
fn update_links(
    id: usize,
    old: Option<&Parsed>,
    new: Option<&Parsed>,
    changes: &mut BTreeMap<Key, Changes>,
) {
    let mut old = keys(old);
    let mut new = keys(new);
    for ordinal in 0..old.len().max(new.len()) {
        let a = old.next();
        let b = new.next();
        if a == b {
            continue;
        }
        let site = (id, ordinal);
        append_site(changes, a, |edit| &mut edit.remove, site);
        append_site(changes, b, |edit| &mut edit.add, site);
    }
}

fn append_site(
    changes: &mut BTreeMap<Key, Changes>,
    address: Option<(&str, &str)>,
    kind: impl FnOnce(&mut Changes) -> &mut Vec<Site>,
    site: Site,
) {
    if let Some((path, anchor)) = address {
        kind(changes.entry((path.into(), anchor.into())).or_default()).push(site);
    }
}

struct DocumentRecord<'a> {
    id: usize,
    old: Option<Range<u64>>,
    next: Option<&'a Parsed>,
}

impl DocumentRecord<'_> {
    fn read(&self, prior: &dyn Previous) -> io::Result<(Vec<u8>, Option<Parsed>)> {
        let Some(range) = &self.old else { return Ok((Vec::new(), None)) };
        let bytes = read(prior, range.clone())?;
        let mut decoder = Decoder(&bytes);
        if decoder.num()? != self.id {
            return Err(bad());
        }
        let document = markdown_lookup::document_body(decoder.0)?;
        Ok((bytes, Some(document)))
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        if let Some(document) = self.next {
            put(&mut bytes, self.id)?;
            document.write_to(&mut bytes)?;
        }
        Ok(bytes)
    }
}

struct RecordEdit {
    old: Vec<u8>,
    previous: Option<Parsed>,
    next: Vec<u8>,
}

impl RecordEdit {
    /// Empty replacement bytes remove the document from the new stream.
    fn write(
        self,
        run: &mut Run<'_, impl ExtentSink>,
        previous: Option<Range<u64>>,
    ) -> io::Result<()> {
        match previous {
            _ if self.next.is_empty() => Ok(()),
            Some(record) => run.replacement(record.start, &self.old, &self.next),
            None => run.bytes(&self.next),
        }
    }

    fn load(prior: &dyn Previous, record: DocumentRecord<'_>) -> io::Result<Self> {
        let (old, previous) = record.read(prior)?;
        let next = record.encode()?;
        Ok(Self { old, previous, next })
    }

    fn fits(&self, next: &Parsed) -> bool {
        self.old.len() == self.next.len() && keys(self.previous.as_ref()).eq(keys(Some(next)))
    }
}
struct Footer {
    base: u64,
    tables: u64,
    old_footer: Vec<u8>,
    docs: Vec<u64>,
    reverse: Vec<u64>,
}

impl Footer {
    fn write(
        &self,
        run: &mut Run<'_, impl ExtentSink>,
        start: u64,
        directories: [Vec<u64>; 2],
    ) -> io::Result<()> {
        let tables_at = run.offset() - start;
        let mut out = Vec::new();
        offsets::write_tables(&mut out, tables_at, directories)?;
        run.replacement(self.base + self.tables, &self.old_footer, &out)?;
        Ok(())
    }

    fn write_reverse(
        &self,
        run: &mut Run<'_, impl ExtentSink>,
        prior: &dyn Previous,
        start: u64,
        changes: BTreeMap<Key, Changes>,
    ) -> io::Result<Vec<u64>> {
        if changes.is_empty() {
            let old_at = *self.docs.last().unwrap();
            let new_at = run.offset() - start;
            run.borrow(self.base + old_at..self.base + self.tables)?;
            return Ok(self.reverse.iter().map(|s| new_at + (s - old_at)).collect());
        }
        ReverseIndex {
            prior,
            records: Records {
                base: self.base,
                offsets: &self.reverse,
            },
        }
        .write(run, start, changes)
    }

    fn read(
        p: &dyn Previous,
        md: &Range<u64>,
    ) -> io::Result<Self> {
        let base = md.start;
        let trailer = md
            .end
            .checked_sub(8)
            .filter(|&at| at >= base)
            .ok_or_else(bad)?;
        let tables = u64::from_le_bytes(
            read(p, trailer..md.end)?
                .as_slice()
                .try_into()
                .map_err(|_| bad())?,
        );
        if tables > trailer - base {
            return Err(bad());
        }
        let footer_start = base + tables;
        let size = usize::try_from(md.end - footer_start).map_err(|_| bad())?;
        let mut old_footer = Vec::with_capacity(size);
        p.copy_bytes(footer_start..trailer, &mut old_footer)?;
        let mut directory = Decoder(&old_footer);
        let docs = table(&mut directory, tables)?;
        let reverse = table(&mut directory, tables)?;
        directory.end()?;
        if docs[0] != 8
            || reverse[0] != docs[docs.len() - 1].checked_add(8).ok_or_else(bad)?
            || reverse[reverse.len() - 1] != tables
            || docs.len() != p.entries().iter().filter(|f| relevant(f)).count() + 1
        {
            return Err(bad());
        }
        old_footer.extend_from_slice(&tables.to_le_bytes());
        Ok(Self {
            base,
            tables,
            old_footer,
            docs,
            reverse,
        })
    }
}

struct ReverseIndex<'a> {
    prior: &'a dyn Previous,
    records: Records<'a>,
}
struct Address {
    key: Key,
    sites: Range<u64>,
}
struct AddressPosition<'a> {
    slot: usize,
    existing: Option<&'a Address>,
}

struct ReverseEdit {
    slot: usize,
    old: Vec<u8>,
    new: Vec<u8>,
}
impl ReverseEdit {
    fn load(
        prior: &dyn Previous,
        position: AddressPosition<'_>,
        key: Key,
        changes: Changes,
    ) -> io::Result<Self> {
        let mut old = Vec::new();
        string(&mut old, &key.0)?;
        string(&mut old, &key.1)?;
        let prefix = old.len();
        let new = if let Some(address) = position.existing {
            prior.copy_bytes(address.sites.clone(), &mut old)?;
            changes.replace(&old[..prefix], Some(&old[prefix..]))?
        } else {
            let new = changes.replace(&old, None)?;
            old.clear();
            new
        };
        Ok(Self { slot: position.slot, old, new })
    }
}
/// Changed keys arrive in sorted order. Keep decoded rows and the last lower
/// bound together so a later address lookup never restarts at the first row.
#[derive(Default)]
struct AddressSearch {
    cache: BTreeMap<usize, Address>,
    lower: usize,
}
impl AddressSearch {
    fn get(&mut self, index: &ReverseIndex<'_>, slot: usize) -> io::Result<&Address> {
        use std::collections::btree_map::Entry;
        match self.cache.entry(slot) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => Ok(entry.insert(index.address(slot)?)),
        }
    }

    fn locate(
        &mut self,
        index: &ReverseIndex<'_>,
        key: &Key,
    ) -> io::Result<AddressPosition<'_>> {
        let ordinals = self.lower as u64..index.records.len() as u64;
        let lo = offsets::lower_bound(ordinals, |slot| Ok(self.get(index, slot as usize)?.key < *key))? as usize;
        self.lower = lo;
        Ok(AddressPosition { slot: lo, existing: self.cache.get(&lo).filter(|a| a.key == *key) })
    }
}

impl ReverseIndex<'_> {
    fn address(&self, slot: usize) -> io::Result<Address> {
        let mut range = self.records.range(slot..slot + 1);
        let mut reader = PriorRecord {
            prior: self.prior,
            end: range.end,
        };
        let key = markdown_lookup::address(&mut reader, &mut range)?;
        Ok(Address { key, sites: range })
    }
    fn plan(&self, changes: BTreeMap<Key, Changes>) -> io::Result<(usize, Vec<ReverseEdit>)> {
        let mut count = self.records.len();
        let mut search = AddressSearch::default();
        let mut updates = Vec::with_capacity(changes.len());
        for (key, changes) in changes {
            let position = search.locate(self, &key)?;
            let edit = ReverseEdit::load(self.prior, position, key, changes)?;
            count = count - usize::from(!edit.old.is_empty()) + usize::from(!edit.new.is_empty());
            updates.push(edit);
        }
        Ok((count, updates))
    }
    fn write(
        &self,
        run: &mut Run<'_, impl ExtentSink>,
        start: u64,
        changes: BTreeMap<Key, Changes>,
    ) -> io::Result<Vec<u64>> {
        let (new_count, updates) = self.plan(changes)?;
        let previous = &self.records;
        let mut next_destinations = Vec::with_capacity(new_count + 1);
        run.bytes(&(new_count as u64).to_le_bytes())?;
        let mut next = 0usize;
        for ReverseEdit {
            slot,
            old,
            new,
        } in updates
        {
            previous.retain(next..slot, run, start, &mut next_destinations)?;
            if !new.is_empty() {
                next_destinations.push(run.offset() - start);
                run.replacement(previous.at(slot), &old, &new)?
            }
            next = slot + usize::from(!old.is_empty());
        }
        previous.retain(next..previous.len(), run, start, &mut next_destinations)?;
        next_destinations.push(run.offset() - start);
        Ok(next_destinations)
    }
}

/// Offsets are relative to the start of the old Markdown product.
struct Records<'a> {
    base: u64,
    offsets: &'a [u64],
}
impl Records<'_> {
    fn len(&self) -> usize {
        self.offsets.len() - 1
    }
    fn at(&self, slot: usize) -> u64 {
        self.base + self.offsets[slot]
    }

    fn range(&self, slots: Range<usize>) -> Range<u64> {
        self.at(slots.start)..self.at(slots.end)
    }

    fn retain(
        &self,
        slots: Range<usize>,
        run: &mut Run<'_, impl ExtentSink>,
        start: u64,
        next: &mut Vec<u64>,
    ) -> io::Result<()> {
        if slots.is_empty() {
            return Ok(());
        }
        let range = self.range(slots.clone());
        let first = self.offsets[slots.start];
        let dest = run.offset() - start;
        next.extend(self.offsets[slots].iter().map(|&at| dest + (at - first)));
        run.borrow(range)
    }
}

fn relevant(f: &Entry) -> bool {
    f.has_markdown()
}

pub(super) struct Documents<'a> {
    pub prior: &'a dyn Previous,
    pub files: &'a [Entry],
    pub locals: &'a [Local],
    pub reuse: &'a [Option<usize>],
}

impl Documents<'_> {
    pub fn write_product(&self, w: &mut impl ExtentSink) -> io::Result<()> {
        let p = self.prior;
        let md = p.extent(MD);
        let mut edits = self.edits().peekable();
        let Some(first) = edits.next() else {
            return retained::range(p, w, md);
        };
        // The first and peeked rows remain available to the writer; classification
        // of reused source slots and mixed non-Markdown edits is never repeated.
        let prepared = match self.plan(first.0, edits.peek().is_none())? {
            Plan::Retain => return retained::range(p, w, md),
            Plan::Splice(record, edit) => {
                let mut run = Run::new(Some(p), w);
                run.borrow(md.start..record.start)?;
                run.replacement(record.start, &edit.old, &edit.next)?;
                run.borrow(record.end..md.end)?;
                return run.flush();
            }
            Plan::Rebuild(prepared) => prepared,
        };
        let footer = Footer::read(p, &md)?;
        let mut run = Run::new(Some(p), w);
        let start = run.offset();
        let (next_docs, changes) = self.write(
            &mut run,
            &footer,
            std::iter::once(first).chain(edits),
            prepared,
        )?;
        let reverse = footer.write_reverse(&mut run, p, start, changes)?;
        footer.write(&mut run, start, [next_docs, reverse])?;
        run.flush()
    }
}

enum Plan {
    Retain,
    Splice(Range<u64>, RecordEdit),
    Rebuild(Option<RecordEdit>),
}

impl Documents<'_> {
    /// A document edit identifies one path and its zero or one predecessor slots.
    /// Retained Markdown advances the slot. Other source edits have no directory entry.
    fn edits(&self) -> impl Iterator<Item = (usize, Range<usize>)> + '_ {
        let mut old_slot = 0;
        self.files
            .iter()
            .zip(self.prior.entries())
            .zip(self.reuse)
            .enumerate()
            .filter_map(move |(id, ((file, old), reuse))| {
                let start = old_slot;
                let was_document = relevant(old);
                old_slot += usize::from(was_document);
                (reuse.is_none() && (was_document || relevant(file)))
                    .then_some((id, start..old_slot))
            })
    }

    fn plan(&self, id: usize, single: bool) -> io::Result<Plan> {
        // Equal-size records keep every directory offset. Equal destination sequences
        // keep every (file, link ordinal) reverse posting, even when link spans move.
        let Some(next) = self.locals[id].markdown.as_deref().filter(|_| single) else {
            return Ok(Plan::Rebuild(None));
        };
        let Some(record) = self.prior.markdown_record(id)? else {
            return Ok(Plan::Rebuild(None));
        };
        let edit = RecordEdit::load(self.prior, DocumentRecord {
            id,
            old: Some(record.clone()),
            next: Some(next),
        })?;
        if edit.old == edit.next {
            return Ok(Plan::Retain);
        }
        if edit.fits(next) {
            return Ok(Plan::Splice(record, edit));
        }
        Ok(Plan::Rebuild(Some(edit)))
    }

    fn write(
        &self,
        run: &mut Run<'_, impl ExtentSink>,
        footer: &Footer,
        edits: impl Iterator<Item = (usize, Range<usize>)>,
        mut prepared: Option<RecordEdit>,
    ) -> io::Result<(Vec<u64>, BTreeMap<Key, Changes>)> {
        let start = run.offset();
        let mut next_docs = Vec::new();
        let n = self.files.iter().filter(|f| relevant(f)).count();
        run.bytes(&(n as u64).to_le_bytes())?;
        let previous = Records {
            base: footer.base,
            offsets: &footer.docs,
        };
        let mut first_unchanged = 0;
        let mut changes = BTreeMap::new();
        for (id, slots) in edits {
            previous.retain(first_unchanged..slots.start, run, start, &mut next_docs)?;
            first_unchanged = slots.end;
            let old_range = (!slots.is_empty()).then(|| previous.range(slots));
            if relevant(&self.files[id]) {
                next_docs.push(run.offset() - start);
            }
            let new = self.locals[id].markdown.as_deref();
            let edit = prepared.take().map_or_else(
                || RecordEdit::load(self.prior, DocumentRecord { id, old: old_range.clone(), next: new }),
                Ok,
            )?;
            update_links(id, edit.previous.as_ref(), new, &mut changes);
            edit.write(run, old_range)?;
        }
        previous.retain(first_unchanged..previous.len(), run, start, &mut next_docs)?;
        next_docs.push(run.offset() - start);
        Ok((next_docs, changes))
    }
}
