//! On a same-path edit, only lists in the symmetric difference of that
//! file's trigram sets need changes. Splice their individual owner IDs into
//! the predecessor's packed arrays. Other lists stay in their old segments.
use super::{Previous, retained::Run};
use crate::{
    application::index::{
        binary::bad,
        layout::{CONTENT, ExtentSink},
    },
    domain::{code::Unit, content::{PostingLayout, grams}},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    ops::Range,
};

mod sampled;

struct Edit {
    id: u64,
    add: bool,
}
pub(super) enum Delta {
    Sampled(sampled::Patch),
    Legacy(Legacy),
}
pub(super) struct Legacy {
    old: Range<u64>,
    keys: Vec<u8>,
    lengths: Vec<u64>,
    key_base: u64,
    old_keys: Vec<u8>,
    membership: MemberEdits,
    values: u64,
    total: u64,
}
fn read(p: &dyn Previous, r: Range<u64>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(r.end - r.start).map_err(|_| bad())?)
        .map_err(io::Error::other)?;
    p.copy_bytes(r, &mut bytes)?;
    Ok(bytes)
}
fn num(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}
fn gram(bytes: &[u8]) -> io::Result<u64> {
    let mut key = 0;
    for b in bytes.as_chunks::<4>().0 {
        let scalar = u32::from_le_bytes(*b);
        char::from_u32(scalar).ok_or_else(bad)?;
        key = key << 21 | u64::from(scalar);
    }
    Ok(key)
}
fn end(base: u64, count: u64, width: u64, limit: u64) -> io::Result<u64> {
    count
        .checked_mul(width)
        .and_then(|n| n.checked_add(base))
        .filter(|&n| n <= limit)
        .ok_or_else(bad)
}
/// Packed posting columns follow their count headers contiguously.
struct Columns<'a> {
    prior: &'a dyn Previous,
    at: u64,
    limit: u64,
}
impl Columns<'_> {
    fn take(&mut self, count: u64, width: u64) -> io::Result<Range<u64>> {
        let start = self.at;
        self.at = end(start, count, width, self.limit)?;
        Ok(start..self.at)
    }
    fn count(&mut self) -> io::Result<u64> {
        let word = self.take(1, 8)?;
        Ok(num(&read(self.prior, word)?))
    }
    fn counted(&mut self, count: u64, width: u64) -> io::Result<Range<u64>> {
        if self.count()? != count {
            return Err(bad());
        }
        self.take(count, width)
    }
    fn values(&mut self) -> io::Result<(u64, u64)> {
        let count = self.count()?;
        let values = self.take(count, 8)?;
        if values.end != self.limit {
            return Err(bad());
        }
        Ok((values.start, count))
    }
}

fn membership_changes(
    units: &[Unit],
    before: &BTreeMap<usize, String>,
) -> BTreeMap<u64, Vec<Edit>> {
    let mut changes = BTreeMap::<u64, Vec<Edit>>::new();
    for (&i, raw) in before {
        let unit = &units[i];
        let before = grams(raw).collect::<BTreeSet<_>>();
        let after = grams(unit.text).collect::<BTreeSet<_>>();
        for &key in before.symmetric_difference(&after) {
            changes.entry(key).or_default().push(Edit {
                id: i as u64,
                add: after.contains(&key),
            });
        }
    }
    changes
}
impl Delta {
    pub(super) fn plan(
        prior: &dyn Previous,
        units: &[Unit],
        before: &BTreeMap<usize, String>,
    ) -> io::Result<Option<Self>> {
        let changes = membership_changes(units, before);
        if changes.is_empty() {
            return Ok(None);
        }
        let old = prior.extent(CONTENT);
        let header = read(prior, old.start..end(old.start, 9, 1, old.end)?)?;
        let count = num(&header[1..]);
        let layout = PostingLayout::from_tag(header[0]).ok_or_else(bad)?;
        let delta = match layout {
            PostingLayout::Sampled => Self::Sampled(sampled::Patch::plan(prior, old, count, changes)?),
            PostingLayout::Offsets => Self::Legacy(Legacy::plan(prior, old, count, changes)?),
        };
        Ok(Some(delta))
    }
    pub(super) fn write(self, prior: &dyn Previous, w: &mut impl ExtentSink) -> io::Result<()> {
        match self {
            Self::Legacy(p) => p.write(prior, w),
            Self::Sampled(p) => p.write(prior, w),
        }
    }
}
/// Small lists cost one contiguous read; long lists probe one on-disk ID at a time.
struct PostingList<'a> {
    prior: &'a dyn Previous,
    bytes: Range<u64>,
    local: Option<Vec<u8>>,
}

impl<'a> PostingList<'a> {
    fn open(prior: &'a dyn Previous, bytes: Range<u64>) -> io::Result<Self> {
        let local = if bytes.end - bytes.start <= 256 * 8 {
            Some(read(prior, bytes.clone())?)
        } else {
            None
        };
        Ok(Self {
            prior,
            bytes,
            local,
        })
    }

    fn id_at(&self, index: u64) -> io::Result<u64> {
        if let Some(data) = &self.local {
            return Ok(num(&data[index as usize * 8..][..8]));
        }
        let at = self.bytes.start + index * 8;
        let mut word = [0; 8];
        let mut dest = &mut word[..];
        self.prior.copy_bytes(at..at + 8, &mut dest)?;
        if !dest.is_empty() {
            return Err(bad());
        }
        Ok(u64::from_le_bytes(word))
    }

    fn position(&self, id: u64) -> io::Result<(u64, bool)> {
        let (mut lo, mut hi) = (0, (self.bytes.end - self.bytes.start) / 8);
        let mut present = false;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let owner = self.id_at(mid)?;
            match owner.cmp(&id) {
                std::cmp::Ordering::Less => lo = mid + 1,
                order => {
                    present |= order.is_eq();
                    hi = mid;
                }
            }
        }
        Ok((lo, present))
    }
}

/// Membership replacements are sorted by position in the predecessor, including EOF inserts.
enum MemberChange {
    Insert(u64),
    Remove,
}

impl MemberChange {
    fn checked(edit: Edit, present: bool) -> io::Result<Self> {
        match (edit.add, present) {
            (true, false) => Ok(Self::Insert(edit.id)),
            (false, true) => Ok(Self::Remove),
            _ => Err(bad()),
        }
    }

    fn adjust(&self, len: u64) -> io::Result<u64> {
        match self {
            Self::Insert(_) => len.checked_add(1),
            Self::Remove => len.checked_sub(1),
        }.ok_or_else(bad)
    }

    fn write(self, run: &mut Run<'_, impl ExtentSink>, offset: u64) -> io::Result<u64> {
        if let Self::Insert(id) = self {
            run.bytes(&id.to_le_bytes())?;
            return Ok(offset);
        }
        Ok(offset + 8)
    }
}

#[derive(Default)]
struct MemberEdits(Vec<(u64, MemberChange)>);

impl MemberEdits {
    fn record(
        &mut self,
        prior: &dyn Previous,
        range: Range<u64>,
        edits: Vec<Edit>,
    ) -> io::Result<u64> {
        let offset = range.start;
        let mut len = (range.end - range.start) / 8;
        let owners = PostingList::open(prior, range)?;
        for edit in edits {
            let (index, present) = owners.position(edit.id)?;
            let change = MemberChange::checked(edit, present)?;
            len = change.adjust(len)?;
            self.0.push((offset + 8 * index, change));
        }
        Ok(len)
    }

    fn write(self, mut run: Run<'_, impl ExtentSink>, range: Range<u64>) -> io::Result<()> {
        let mut last = range.start;
        for (offset, edit) in self.0 {
            run.borrow(last..offset)?;
            last = edit.write(&mut run, offset)?;
        }
        run.borrow(last..range.end)?;
        run.flush()
    }
}
/// Merge incoming keys with the legacy dictionary. Each stored key and
/// successive offset is decoded once, including runs of inserted keys.
struct LegacyKeys<'a> {
    remaining: std::slice::ChunksExact<'a, u8>,
    ends: std::slice::ChunksExact<'a, u8>,
    next: Option<(u64, &'a [u8])>,
    start: u64,
    limit: u64,
}
impl<'a> LegacyKeys<'a> {
    fn open(keys: &'a [u8], offsets: &'a [u8], limit: u64) -> io::Result<Self> {
        let mut ends = offsets.chunks_exact(8);
        if ends.next().map(num) != Some(0) || num(&offsets[offsets.len() - 8..]) != limit {
            return Err(bad());
        }
        let mut cursor = Self {
            remaining: keys.chunks_exact(12),
            ends,
            next: None,
            start: 0,
            limit,
        };
        cursor.advance()?;
        Ok(cursor)
    }
    fn advance(&mut self) -> io::Result<()> {
        self.next = self
            .remaining
            .next()
            .map(|raw| gram(raw).map(|key| (key, raw)))
            .transpose()?;
        Ok(())
    }
    fn take(&mut self, incoming: Option<u64>) -> io::Result<Option<(u64, [u8; 12], Range<u64>)>> {
        let Some(key) = self
            .next
            .map(|(key, _)| key)
            .into_iter()
            .chain(incoming)
            .min()
        else {
            return Ok(None);
        };
        let start = self.start;
        let bytes = if let Some((_, raw)) = self.next.filter(|&(old, _)| old == key) {
            self.start = num(self.ends.next().ok_or_else(bad)?);
            if self.start < start || self.start > self.limit {
                return Err(bad());
            }
            self.advance()?;
            raw.try_into().unwrap()
        } else {
            crate::domain::content::encoded_key(key)
        };
        Ok(Some((key, bytes, start..self.start)))
    }
}
impl MemberEdits {
    fn legacy_len(
        &mut self,
        prior: &dyn Previous,
        (values, owners): (u64, Range<u64>),
        edits: Option<Vec<Edit>>,
    ) -> io::Result<u64> {
        match edits {
            Some(edits) => self.record(prior, values + 8 * owners.start..values + 8 * owners.end, edits),
            None => Ok(owners.end - owners.start),
        }
    }
}

impl Legacy {
    fn plan(
        prior: &dyn Previous,
        old: Range<u64>,
        count: u64,
        changes: BTreeMap<u64, Vec<Edit>>,
    ) -> io::Result<Self> {
        let mut columns = Columns {
            prior,
            at: old.start + 9,
            limit: old.end,
        };
        let key_range = columns.take(count, 12)?;
        let start_range = columns.counted(count.checked_add(1).ok_or_else(bad)?, 8)?;
        let (values, old_total) = columns.values()?;
        let key_base = key_range.start;
        let old_keys = read(prior, key_range)?;
        let old_starts = read(prior, start_range)?;
        let mut dictionary = LegacyKeys::open(&old_keys, &old_starts, old_total)?;
        let mut keys = Vec::with_capacity(old_keys.len());
        let mut lengths = Vec::with_capacity(old_starts.len() / 8 - 1);
        let mut membership = MemberEdits::default();
        let mut total = 0u64;
        let mut changes = changes.into_iter().peekable();
        while let Some((key, raw, owners)) = dictionary.take(changes.peek().map(|(key, _)| *key))? {
            let edits = changes.next_if(|(k, _)| *k == key).map(|(_, edits)| edits);
            let len = membership.legacy_len(prior, (values, owners), edits)?;
            if len == 0 {
                continue;
            }
            total = total.checked_add(len).ok_or_else(bad)?;
            keys.extend_from_slice(&raw);
            lengths.push(len);
        }
        Ok(Self {
            old,
            keys,
            lengths,
            key_base,
            old_keys,
            membership,
            values,
            total,
        })
    }
    fn write(self, prior: &dyn Previous, w: &mut impl ExtentSink) -> io::Result<()> {
        let mut run = Run::new(Some(prior), w);
        // Transcode just the directory on the first changed legacy product.
        // The keys and all unaffected owner IDs stay in their original extents.
        run.bytes(&PostingLayout::Sampled.tag())?;
        run.bytes(&((self.keys.len() / 12) as u64).to_le_bytes())?;
        run.replacement(self.key_base, &self.old_keys, &self.keys)?;
        let mut directory =
            Vec::with_capacity(self.lengths.len() * 8 + self.lengths.len() / 8 + 24);
        crate::domain::content::sampled_lengths(&mut directory, self.lengths.into_iter())?;
        run.bytes(&directory)?;
        run.bytes(&self.total.to_le_bytes())?;
        self.membership.write(run, self.values..self.old.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::index::{
        Entry,
        build::{Local, tests::MemorySink},
    };
    use crate::domain::{
        Document, FileId,
        code::Analysis,
        content::{ContentBuilder, ContentRun},
    };

    struct Prior(Vec<u8>, std::sync::atomic::AtomicU64);
    impl Previous for Prior {
        fn extent(&self, kind: usize) -> Range<u64> {
            assert_eq!(kind, CONTENT);
            0..self.0.len() as u64
        }
        fn same_bytes(&self, range: Range<u64>, expected: &[u8]) -> io::Result<bool> {
            Ok(&self.0[range.start as usize..range.end as usize] == expected)
        }
        fn copy_bytes(&self, range: Range<u64>, out: &mut dyn io::Write) -> io::Result<()> {
            self.1.fetch_add(
                range.end - range.start,
                std::sync::atomic::Ordering::Relaxed,
            );
            out.write_all(&self.0[range.start as usize..range.end as usize])
        }
        fn entries(&self) -> &[Entry] {
            unreachable!()
        }
        fn local(&self, _: usize, _: bool, _: bool) -> io::Result<Local> {
            unreachable!()
        }
        fn product(&self, _: usize) -> io::Result<Vec<u8>> {
            unreachable!()
        }
    }
    fn encode(files: &[String], sampled: bool) -> Vec<u8> {
        let mut builder = ContentBuilder::default();
        for (id, text) in files.iter().enumerate() {
            builder
                .add(Document {
                    file: FileId(id as u64),
                    text,
                })
                .unwrap();
        }
        let mut bytes = Vec::new();
        builder.seal_layout(&mut bytes, sampled).unwrap();
        bytes
    }
    #[test]
    fn editing_either_codec_yields_fresh_sampled_product() {
        let files: Vec<_> = (0..320)
            .map(|i| format!("shared old member {i:03}"))
            .collect();
        let mut after = files.clone();
        let mut before = BTreeMap::new();
        for (id, text) in [(0, ""), (128, "unique🙂change"), (259, ""), (319, "zzzzzz")] {
            before.insert(id, std::mem::replace(&mut after[id], text.into()));
        }
        let facts = Analysis::default();
        let units: Vec<_> = after
            .iter()
            .map(|text| Unit {
                path: "",
                text,
                analysis: &facts,
            })
            .collect();
        let expected = encode(&after, true);
        ContentRun::open(&expected).unwrap().validate().unwrap();
        for sampled in [false, true] {
            let prior = Prior(encode(&files, sampled), Default::default());
            let mut sink = MemorySink::default();
            Delta::plan(&prior, &units, &before)
                .unwrap()
                .unwrap()
                .write(&prior, &mut sink)
                .unwrap();
            assert!(sink.0 == expected, "legacy={}", !sampled);
        }
    }
    #[test]
    fn changing_one_frequent_legacy_gram_reads_a_bounded_number_of_owner_ids() {
        let mut files = vec![String::from("abc"); 4096];
        let prior = Prior(encode(&files, false), Default::default());
        let before = BTreeMap::from([(2048, std::mem::take(&mut files[2048]))]);
        let facts = Analysis::default();
        let units: Vec<_> = files
            .iter()
            .map(|text| Unit {
                path: "",
                text,
                analysis: &facts,
            })
            .collect();
        let plan = Delta::plan(&prior, &units, &before).unwrap().unwrap();
        // The dictionary and binary search fit in 512 bytes. The original list is 32 KiB.
        let read = prior.1.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("legacy owner update planned in {read} bytes");
        assert!(read <= 512, "{read} bytes");
        let mut sink = MemorySink::default();
        plan.write(&prior, &mut sink).unwrap();
        assert_eq!(sink.0, encode(&files, true));
    }
}
