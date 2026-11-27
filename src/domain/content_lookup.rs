//! Random-access cursor over content codec 3. Open reads only vector lengths.
//! A query probes each distinct key and intersects the shortest lists first.
use super::content::{PostingLayout, Trigram};
use std::{collections::BTreeSet, io, ops::Range};

pub trait ExtentRead {
    fn length(&self) -> u64;
    fn read(&mut self, offset: u64, dest: &mut [u8]) -> io::Result<()>;
}
impl<T: ExtentRead + ?Sized> ExtentRead for Box<T> {
    fn length(&self) -> u64 {
        (**self).length()
    }
    fn read(&mut self, offset: u64, dest: &mut [u8]) -> io::Result<()> {
        (**self).read(offset, dest)
    }
}
impl ExtentRead for io::Cursor<Vec<u8>> {
    fn length(&self) -> u64 {
        self.get_ref().len() as u64
    }
    fn read(&mut self, offset: u64, dest: &mut [u8]) -> io::Result<()> {
        let from = usize::try_from(offset).map_err(|_| bad())?;
        let s = self
            .get_ref()
            .get(from..)
            .and_then(|s| s.get(..dest.len()))
            .ok_or_else(bad)?;
        dest.copy_from_slice(s);
        Ok(())
    }
}
fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid posting directory")
}
fn word(reader: &mut impl ExtentRead, offset: u64) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read(offset, &mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn vector_end(start: u64, count: u64, width: u64) -> io::Result<u64> {
    count
        .checked_mul(width)
        .and_then(|v| start.checked_add(v))
        .ok_or_else(bad)
}

fn find_slot(
    mut slots: Range<u64>,
    mut compare: impl FnMut(u64) -> io::Result<std::cmp::Ordering>,
) -> io::Result<Option<u64>> {
    while slots.start < slots.end {
        let mid = slots.start + (slots.end - slots.start) / 2;
        match compare(mid)? {
            std::cmp::Ordering::Less => slots.start = mid + 1,
            std::cmp::Ordering::Greater => slots.end = mid,
            std::cmp::Ordering::Equal => return Ok(Some(mid)),
        }
    }
    Ok(None)
}

fn posting_counts(reader: &mut impl ExtentRead, layout: PostingLayout, ends: u64, keys: u64) -> io::Result<(Option<u64>, u64)> {
    if layout == PostingLayout::Offsets {
        return Ok((None, ends));
    }
    let count = word(reader, ends)?;
    if count != keys.div_ceil(super::content::SAMPLE as u64) {
        return Err(bad());
    }
    let base = ends.checked_add(8).ok_or_else(bad)?;
    Ok((Some(base), vector_end(base, count, 8)?))
}

struct Owners {
    file_count: usize,
    previous: Option<usize>,
}

impl Owners {
    fn read_block(&mut self, bytes: &[u8], visit: &mut impl FnMut(usize) -> bool) -> io::Result<bool> {
        let mut done = false;
        for word in bytes.as_chunks::<8>().0 {
            let id = usize::try_from(u64::from_le_bytes(*word)).map_err(|_| bad())?;
            if id >= self.file_count || self.previous.is_some_and(|p| p >= id) {
                return Err(bad());
            }
            self.previous = Some(id);
            done = visit(id);
        }
        Ok(done)
    }
}

struct Intersection<'a> {
    hits: &'a mut Vec<usize>,
    next: usize,
    keep: usize,
}

impl Intersection<'_> {
    fn visit(&mut self, id: usize) -> bool {
        while self.hits.get(self.next).is_some_and(|&hit| hit < id) {
            self.next += 1;
        }
        if self.hits.get(self.next) == Some(&id) {
            self.hits[self.keep] = id;
            self.next += 1;
            self.keep += 1;
        }
        self.next == self.hits.len()
    }

    fn finish(self) {
        self.hits.truncate(self.keep);
    }
}

pub struct Cursor<R> {
    reader: R,
    keys: u64,
    starts: u64,
    values: u64,
    total: u64,
    anchors: Option<u64>,
}
impl<R: ExtentRead> Cursor<R> {
    pub fn open(mut reader: R) -> io::Result<Self> {
        let mut stream = [0];
        reader.read(0, &mut stream)?;
        let layout = PostingLayout::from_tag(stream[0]).ok_or_else(bad)?;
        let keys = word(&mut reader, 1)?;
        let starts_len = vector_end(9, keys, 12)?;
        let n = word(&mut reader, starts_len)?;
        if n != layout.boundaries(keys).ok_or_else(bad)? {
            return Err(bad());
        }
        let starts = starts_len.checked_add(8).ok_or_else(bad)?;
        let ends = vector_end(starts, n, 8)?;
        let (anchors, count_offset) = posting_counts(&mut reader, layout, ends, keys)?;
        let total = word(&mut reader, count_offset)?;
        let values = count_offset.checked_add(8).ok_or_else(bad)?;
        if vector_end(values, total, 8)? != reader.length()
            || (anchors.is_none()
                && (word(&mut reader, starts)? != 0
                    || word(&mut reader, count_offset - 8)? != total))
        {
            return Err(bad());
        }
        Ok(Self {
            reader,
            keys,
            starts,
            values,
            total,
            anchors,
        })
    }
    fn range(&mut self, key: Trigram) -> io::Result<Range<u64>> {
        let key = key.map(u32::from);
        let slot = find_slot(0..self.keys, |mid| {
            let mut raw = [0; 12];
            self.reader.read(9 + mid * 12, &mut raw)?;
            let mut code = [0; 3];
            for (v, b) in code.iter_mut().zip(raw.as_chunks::<4>().0) {
                *v = u32::from_le_bytes(*b);
            }
            Ok(code.cmp(&key))
        })?;
        let Some(mid) = slot else { return Ok(0..0) };
        let (a, b) = if let Some(anchors) = self.anchors {
            const SAMPLE: usize = super::content::SAMPLE;
            let group = mid / SAMPLE as u64;
            let mut a = word(&mut self.reader, anchors + group * 8)?;
            let mut scratch = [0u8; SAMPLE * 8];
            let index = (mid % SAMPLE as u64) as usize;
            self.reader.read(
                self.starts + group * (SAMPLE * 8) as u64,
                &mut scratch[..(index + 1) * 8],
            )?;
            let lengths = scratch.as_chunks::<8>().0;
            for len in &lengths[..index] {
                a = a.checked_add(u64::from_le_bytes(*len)).ok_or_else(bad)?;
            }
            (
                a,
                a.checked_add(u64::from_le_bytes(lengths[index]))
                    .ok_or_else(bad)?,
            )
        } else {
            (
                word(&mut self.reader, self.starts + mid * 8)?,
                word(&mut self.reader, self.starts + (mid + 1) * 8)?,
            )
        };
        if a >= b || b > self.total {
            return Err(bad());
        }
        Ok(a..b)
    }
    /// Test one file without materializing the intersection for the entire catalog.
    pub fn contains(
        &mut self,
        keys: BTreeSet<Trigram>,
        id: usize,
        file_count: usize,
    ) -> io::Result<bool> {
        if id >= file_count {
            return Ok(false);
        }
        let id = id as u64;
        for key in keys {
            let bounds = self.range(key)?;
            let hit = find_slot(bounds, |slot| self.owner_order(slot, file_count, id))?;
            if hit.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn owner_order(&mut self, slot: u64, file_count: usize, expected: u64) -> io::Result<std::cmp::Ordering> {
        let owner = word(&mut self.reader, self.values + slot * 8)?;
        if owner >= file_count as u64 {
            return Err(bad());
        }
        Ok(owner.cmp(&expected))
    }

    // Finish validating each block already read, including the block where
    // the intersection exhausts its candidates.
    fn visit_owners(
        &mut self,
        range: Range<u64>,
        file_count: usize,
        scratch: &mut [u8],
        mut visit: impl FnMut(usize) -> bool,
    ) -> io::Result<()> {
        let mut pos = range.start;
        let mut owners = Owners { file_count, previous: None };
        while pos < range.end {
            let words = (range.end - pos).min(scratch.len() as u64 / 8);
            let block = &mut scratch[..words as usize * 8];
            self.reader.read(self.values + pos * 8, block)?;
            let done = owners.read_block(block, &mut visit)?;
            if done {
                break;
            }
            pos += words;
        }
        Ok(())
    }

    fn posting_lists(&mut self, keys: BTreeSet<Trigram>) -> io::Result<Option<Vec<Range<u64>>>> {
        let mut lists = Vec::new();
        for key in keys {
            let range = self.range(key)?;
            if range.is_empty() {
                return Ok(None);
            }
            lists.push(range);
        }
        Ok(Some(lists))
    }

    pub fn candidates(
        &mut self,
        keys: BTreeSet<Trigram>,
        file_count: usize,
    ) -> io::Result<Vec<usize>> {
        if keys.is_empty() {
            return Ok((0..file_count).collect());
        }
        let Some(mut lists) = self.posting_lists(keys)? else {
            return Ok(Vec::new());
        };
        lists.sort_unstable_by_key(|r| r.end - r.start);
        let mut hits = Vec::new();
        let mut scratch = [0u8; 8192];
        let mut lists = lists.into_iter();
        self.visit_owners(lists.next().unwrap(), file_count, &mut scratch, |id| {
            hits.push(id);
            false
        })?;
        for range in lists {
            let mut intersection = Intersection { hits: &mut hits, next: 0, keep: 0 };
            self.visit_owners(range, file_count, &mut scratch, |id| intersection.visit(id))?;
            intersection.finish();
            if hits.is_empty() {
                break;
            }
        }
        Ok(hits)
    }
}
