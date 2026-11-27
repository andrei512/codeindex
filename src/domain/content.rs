//! One content partition, built from documents in increasing FileId order.
pub(crate) mod fresh;
mod incremental;
pub(crate) use incremental::MergedPostings;
use std::collections::HashMap;
use std::io::{self, Write};

use super::Document;

pub type Trigram = [char; 3];
pub(crate) const SAMPLE: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostingLayout {
    Offsets,
    Sampled,
}

impl PostingLayout {
    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Offsets),
            2 => Some(Self::Sampled),
            _ => None,
        }
    }

    pub(crate) fn tag(self) -> [u8; 1] {
        match self {
            Self::Offsets => [1],
            Self::Sampled => [2],
        }
    }

    pub(crate) fn boundaries(self, keys: u64) -> Option<u64> {
        keys.checked_add(u64::from(self == Self::Offsets))
    }
}

/// Lengths remain valid when a different key gains an owner. Sampling their
/// prefix sums bounds the number of additions required for a successful lookup.
pub(crate) fn sampled_lengths(
    out: &mut impl Write,
    lengths: impl ExactSizeIterator<Item = u64>,
) -> io::Result<u64> {
    let mut anchors = Vec::with_capacity(lengths.len().div_ceil(SAMPLE));
    out.write_all(&(lengths.len() as u64).to_le_bytes())?;
    let mut total = 0u64;
    for (slot, n) in lengths.enumerate() {
        if slot % SAMPLE == 0 {
            anchors.push(total)
        }
        out.write_all(&n.to_le_bytes())?;
        total = total.checked_add(n).ok_or_else(invalid)?;
    }
    out.write_all(&(anchors.len() as u64).to_le_bytes())?;
    for offset in anchors {
        out.write_all(&offset.to_le_bytes())?;
    }
    Ok(total)
}

// Three UTF-8 scalars fit in 63 bits. Numeric order matches scalar-tuple
// order, so sorting the packed keys also orders the durable dictionary.
const SCALAR_MASK: u64 = (1 << 21) - 1;
pub(crate) fn encoded_key(key: u64) -> [u8; 12] {
    let mut bytes = [0; 12];
    for (word, shift) in bytes.chunks_exact_mut(4).zip([42, 21, 0]) {
        word.copy_from_slice(&(((key >> shift) & SCALAR_MASK) as u32).to_le_bytes());
    }
    bytes
}
fn ascii_grams(text: &str) -> impl Iterator<Item = [u8; 3]> + '_ {
    text.as_bytes().array_windows::<3>().copied()
}
pub(crate) fn grams(text: &str) -> impl Iterator<Item = u64> + '_ {
    let mut chars = text.chars();
    let mut prefix = chars
        .next()
        .zip(chars.next())
        .map(|(a, b)| (u64::from(a) << 21) | u64::from(b));
    std::iter::from_fn(move || {
        let key = (prefix? << 21) | u64::from(chars.next()?);
        prefix = Some(key & ((1 << 42) - 1));
        Some(key)
    })
}

/// Per-document ASCII membership shared by ordinary and sharded posting builders.
#[derive(Default)]
struct SeenAscii {
    words: Vec<u64>,
    touched: Vec<usize>,
}
impl SeenAscii {
    fn allocate(&mut self) {
        if self.words.is_empty() {
            self.words.resize(128 * 128 * 128 / 64, 0);
        }
    }

    #[inline]
    fn insert(&mut self, gram: usize) -> bool {
        let slot = gram / 64;
        let mask = 1 << (gram % 64);
        let old = self.words[slot];
        if old & mask != 0 {
            return false;
        }
        if old == 0 {
            self.touched.push(slot);
        }
        self.words[slot] = old | mask;
        true
    }

    fn clear(&mut self) {
        for slot in self.touched.drain(..) {
            self.words[slot] = 0;
        }
    }
}

#[derive(Default)]
pub struct ContentBuilder {
    lists: HashMap<u64, Vec<u64>>,
    last_id: Option<u64>,
    seen_ascii: SeenAscii,
}

impl ContentBuilder {
    pub(crate) fn append_later(&mut self, later: Self) {
        for (key, mut owners) in later.lists {
            match self.lists.entry(key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().append(&mut owners)
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(owners);
                }
            }
        }
        self.last_id = later.last_id.or(self.last_id);
    }

    pub fn add(&mut self, doc: Document<'_>) -> io::Result<()> {
        if self.last_id.is_some_and(|id| id >= doc.file.0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "content input IDs must increase",
            ));
        }
        self.last_id = Some(doc.file.0);
        // The regular source alphabet has only 128³ triples. Reject repeated
        // ASCII triples before probing a posting vector for each occurrence.
        if doc.text.len() < 256 {
            for key in grams(doc.text) {
                self.insert_owner(key, doc.file.0);
            }
            return Ok(());
        }
        self.seen_ascii.allocate();
        if doc.text.is_ascii() {
            self.add_ascii_document(doc);
        } else {
            self.add_unicode(doc);
        }
        self.seen_ascii.clear();
        Ok(())
    }

    fn add_ascii_document(&mut self, doc: Document<'_>) {
        for [a, b, c] in ascii_grams(doc.text) {
            self.add_ascii(a.into(), b.into(), c.into(), doc.file.0);
        }
    }

    fn add_unicode(&mut self, doc: Document<'_>) {
        let ascii = (127 << 42) | (127 << 21) | 127;
        for key in grams(doc.text) {
            self.add_unicode_gram(key, doc.file.0, ascii);
        }
    }

    fn add_unicode_gram(&mut self, key: u64, id: u64, ascii: u64) {
        if key & !ascii == 0 {
            self.add_ascii(key >> 42, (key >> 21) & 127, key & 127, id);
        } else {
            self.insert_owner(key, id);
        }
    }

    fn insert_owner(&mut self, key: u64, id: u64) {
        let owners = self.lists.entry(key).or_default();
        if owners.last() != Some(&id) {
            owners.push(id);
        }
    }

    #[inline]
    fn add_ascii(&mut self, a: u64, b: u64, c: u64, id: u64) {
        let compact = ((a << 14) | (b << 7) | c) as usize;
        if self.seen_ascii.insert(compact) {
            self.lists
                .entry((a << 42) | (b << 21) | c)
                .or_default()
                .push(id);
        }
    }

    /// Content product 2, codec 3, stream 1. The caller supplies a buffered sink.
    pub fn seal(self, out: impl Write) -> io::Result<()> {
        self.seal_layout(out, false)
    }
    pub(crate) fn seal_layout(self, out: impl Write, sampled: bool) -> io::Result<()> {
        let mut out = io::BufWriter::with_capacity(64 * 1024, out);
        let mut entries: Vec<_> = self.lists.into_iter().collect();
        entries.sort_unstable_by_key(|(key, _)| *key);
        let layout = if sampled {
            PostingLayout::Sampled
        } else {
            PostingLayout::Offsets
        };
        out.write_all(&layout.tag())?;
        let count = entries.len() as u64;
        out.write_all(&count.to_le_bytes())?;
        for (key, _) in &entries {
            out.write_all(&encoded_key(*key))?;
        }
        let end = if sampled {
            sampled_lengths(&mut out, entries.iter().map(|(_, list)| list.len() as u64))?
        } else {
            write_offsets(&mut out, entries.iter().map(|(_, list)| list.len() as u64))?
        };
        out.write_all(&end.to_le_bytes())?;
        for id in entries.into_iter().flat_map(|(_, owners)| owners) {
            out.write_all(&id.to_le_bytes())?;
        }
        out.flush()
    }
}

fn write_offsets(
    out: &mut impl Write,
    lengths: impl ExactSizeIterator<Item = u64>,
) -> io::Result<u64> {
    let count = PostingLayout::Offsets
        .boundaries(lengths.len() as u64)
        .ok_or_else(invalid)?;
    out.write_all(&count.to_le_bytes())?;
    let mut end = 0u64;
    out.write_all(&end.to_le_bytes())?;
    for len in lengths {
        end += len;
        out.write_all(&end.to_le_bytes())?;
    }
    Ok(end)
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid content extent")
}

fn vector<'a>(input: &mut &'a [u8], width: usize) -> io::Result<&'a [u8]> {
    let (count, rest) = input.split_at_checked(8).ok_or_else(invalid)?;
    let count = usize::try_from(number(count)).map_err(|_| invalid())?;
    let bytes = count.checked_mul(width).ok_or_else(invalid)?;
    let (items, rest) = rest.split_at_checked(bytes).ok_or_else(invalid)?;
    *input = rest;
    Ok(items)
}

fn number(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

/// Borrows a pinned or privately sealed extent. Lookups decode just the search
/// path and its posting range. Full validation is available at seal/recovery.
pub struct ContentRun<'a> {
    keys: &'a [u8],
    starts: std::borrow::Cow<'a, [u8]>,
    ids: &'a [u8],
}

fn sampled_starts(data: &[u8], anchors: &[u8], n: usize) -> io::Result<Vec<u8>> {
    if data.len() / 8 != n || anchors.len() / 8 != n.div_ceil(SAMPLE) {
        return Err(invalid());
    }
    let mut cumulative = Vec::with_capacity(data.len() + 8);
    let mut total = 0u64;
    cumulative.extend(total.to_le_bytes());
    for (block, anchor) in data.chunks(SAMPLE * 8).zip(anchors.as_chunks::<8>().0) {
        if total != u64::from_le_bytes(*anchor) {
            return Err(invalid());
        }
        total = decode_lengths(block, total, &mut cumulative)?;
    }
    Ok(cumulative)
}

fn decode_lengths(block: &[u8], mut total: u64, cumulative: &mut Vec<u8>) -> io::Result<u64> {
    for word in block.as_chunks::<8>().0 {
        let length = std::num::NonZeroU64::new(u64::from_le_bytes(*word)).ok_or_else(invalid)?;
        total = total.checked_add(length.get()).ok_or_else(invalid)?;
        cumulative.extend(total.to_le_bytes());
    }
    Ok(total)
}

fn sorted_posting(values: &[u8]) -> bool {
    let ids = values.as_chunks::<8>().0;
    !ids.is_empty()
        && ids
            .iter()
            .copied()
            .map(u64::from_le_bytes)
            .is_sorted_by(|a, b| a < b)
}

impl<'a> ContentRun<'a> {
    pub fn open(bytes: &'a [u8]) -> io::Result<Self> {
        let (&tag, mut rest) = bytes.split_first().ok_or_else(invalid)?;
        let layout = PostingLayout::from_tag(tag).ok_or_else(invalid)?;
        let keys = vector(&mut rest, 12)?;
        let data = vector(&mut rest, 8)?;
        let starts: std::borrow::Cow<'_, [u8]> = if layout == PostingLayout::Offsets {
            data.into()
        } else {
            let anchors = vector(&mut rest, 8)?;
            sampled_starts(data, anchors, keys.len() / 12)?.into()
        };
        let ids = vector(&mut rest, 8)?;
        if !rest.is_empty()
            || starts.len() / 8 != keys.len() / 12 + 1
            || number(&starts[..8]) != 0
            || number(&starts[starts.len() - 8..]) != (ids.len() / 8) as u64
        {
            return Err(invalid());
        }
        Ok(Self { keys, starts, ids })
    }

    fn key(&self, index: usize) -> io::Result<Trigram> {
        let mut key = ['\0'; 3];
        for (slot, bytes) in key
            .iter_mut()
            .zip(self.keys[index * 12..][..12].as_chunks::<4>().0)
        {
            *slot = char::from_u32(u32::from_le_bytes(*bytes)).ok_or_else(invalid)?;
        }
        Ok(key)
    }

    fn boundary(&self, index: usize) -> io::Result<usize> {
        usize::try_from(number(&self.starts[index * 8..][..8]))
            .ok()
            .and_then(|n| n.checked_mul(8))
            .ok_or_else(invalid)
    }
    fn values(&self, index: usize) -> io::Result<&'a [u8]> {
        self.ids
            .get(self.boundary(index)?..self.boundary(index + 1)?)
            .ok_or_else(invalid)
    }

    fn lists(&self) -> impl Iterator<Item = io::Result<(Trigram, &'a [u8])>> + '_ {
        let mut start = 0;
        (0..self.keys.len() / 12).map(move |slot| {
            let key = self.key(slot)?;
            let end = self.boundary(slot + 1)?;
            let values = self.ids.get(start..end).ok_or_else(invalid)?;
            start = end;
            Ok((key, values))
        })
    }

    pub fn lookup(&self, key: Trigram) -> io::Result<impl Iterator<Item = u64> + 'a> {
        let mut left = 0;
        let mut right = self.keys.len() / 12;
        let mut values = &[][..];
        while left < right {
            let mid = left + (right - left) / 2;
            match self.key(mid)?.cmp(&key) {
                std::cmp::Ordering::Less => left = mid + 1,
                std::cmp::Ordering::Greater => right = mid,
                std::cmp::Ordering::Equal => {
                    values = self.values(mid)?;
                    break;
                }
            }
        }
        Ok(values
            .as_chunks::<8>()
            .0
            .iter()
            .copied()
            .map(u64::from_le_bytes))
    }

    pub fn validate(&self) -> io::Result<()> {
        let mut previous = None;
        for record in self.lists() {
            let (key, values) = record?;
            if previous.is_some_and(|p| p >= key) || !sorted_posting(values) {
                return Err(invalid());
            }
            previous = Some(key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FileId;

    #[test]
    fn golden_and_corruption() {
        let mut b = ContentBuilder::default();
        b.add(Document {
            file: FileId(7),
            text: "abc",
        })
        .unwrap();
        let mut bytes = Vec::new();
        b.seal(&mut bytes).unwrap();
        let mut golden = vec![1];
        golden.extend(1u64.to_le_bytes());
        for c in [97u32, 98, 99] {
            golden.extend(c.to_le_bytes());
        }
        for n in [2u64, 0, 1, 1, 7] {
            golden.extend(n.to_le_bytes());
        }
        assert_eq!(bytes, golden);
        assert_eq!(bytes.len(), 61);
        for n in 0..bytes.len() {
            assert!(ContentRun::open(&bytes[..n]).is_err());
        }
        bytes[37] = 2;
        assert!(ContentRun::open(&bytes).is_err());
    }

    #[test]
    fn sampled_anchors_cover_unicode_keys_and_reject_bad_offsets() {
        let mut builder = ContentBuilder::default();
        for id in 0..130 {
            let ch = char::from_u32(0x400 + id as u32).unwrap();
            let text = format!("λ{ch}λ");
            builder
                .add(Document {
                    file: FileId(id),
                    text: &text,
                })
                .unwrap();
        }
        let mut bytes = Vec::new();
        builder.seal_layout(&mut bytes, true).unwrap();
        let run = ContentRun::open(&bytes).unwrap();
        run.validate().unwrap();
        for id in [0, 63, 64, 127, 128, 129] {
            let ch = char::from_u32(0x400 + id as u32).unwrap();
            assert_eq!(
                run.lookup(['λ', ch, 'λ']).unwrap().collect::<Vec<_>>(),
                [id]
            );
        }
        let second_anchor = 9 + 130 * 12 + 8 + 130 * 8 + 8 + 8;
        bytes[second_anchor] ^= 1;
        assert!(ContentRun::open(&bytes).is_err());
    }

    #[test]
    fn bulk_ascii_and_unicode_membership_matches_scalar_reference_bytes() {
        let texts = [
            "hello hello hello".repeat(500),
            "a🙂 bÉ and ascii repeat and ascii repeat".repeat(100),
            "\0\u{7f}~ ~~λ".repeat(254),
            "hello hello world".into(),
        ];
        let mut reference = ContentBuilder::default();
        let mut batches = [ContentBuilder::default(), ContentBuilder::default()];
        for (file, text) in texts.iter().enumerate() {
            for key in grams(text) {
                let owners = reference.lists.entry(key).or_default();
                if owners.last() != Some(&(file as u64)) {
                    owners.push(file as u64);
                }
            }
            batches[file / 2]
                .add(Document {
                    file: FileId(file as u64),
                    text,
                })
                .unwrap();
        }
        let [mut first, second] = batches;
        first.append_later(second);
        let mut want = Vec::new();
        let mut got = Vec::new();
        first.seal_layout(&mut got, true).unwrap();
        reference.seal_layout(&mut want, true).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn unicode_membership_and_repeats() {
        let mut b = ContentBuilder::default();
        for (id, text) in [(1, "🙂🙂🙂🙂"), (3, ""), (8, "x🙂🙂🙂")] {
            b.add(Document {
                file: FileId(id),
                text,
            })
            .unwrap();
        }
        let mut bytes = Vec::new();
        b.seal(&mut bytes).unwrap();
        let r = ContentRun::open(&bytes).unwrap();
        r.validate().unwrap();
        assert_eq!(r.lookup(['🙂'; 3]).unwrap().collect::<Vec<_>>(), [1, 8]);
        assert_eq!(r.lookup(['a'; 3]).unwrap().count(), 0);
    }
    #[test]
    fn retained_memberships_survive_insert_edit_and_delete() {
        fn add(b: &mut ContentBuilder, id: usize, text: &str) {
            b.add(Document {
                file: FileId(id as u64),
                text,
            })
            .unwrap();
        }
        fn encode(index: ContentBuilder) -> Vec<u8> {
            let mut bytes = Vec::new();
            index.seal(&mut bytes).unwrap();
            bytes
        }
        let mut prior = ContentBuilder::default();
        for (id, text) in ["first common", "🧶🧶🧶", "old removed", "second common"]
            .iter()
            .enumerate()
        {
            add(&mut prior, id, text);
        }
        let mut fresh = ContentBuilder::default();
        let updated = [
            "insert common",
            "first common",
            "new common🧶🧶🧶",
            "second common",
        ];
        for (id, text) in updated.iter().enumerate() {
            add(&mut fresh, id, text);
        }
        let mut changed = ContentBuilder::default();
        add(&mut changed, 0, updated[0]);
        add(&mut changed, 2, updated[2]);
        let prior = encode(prior);
        let merged = changed
            .merge_prior(
                &ContentRun::open(&prior).unwrap(),
                &[Some(1), None, None, Some(3)],
            )
            .unwrap();
        let mut output = Vec::new();
        merged.seal(&mut output).unwrap();
        assert_eq!(output, encode(fresh));
    }
}
