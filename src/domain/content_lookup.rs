//! Random-access cursor over content codec 3. Open reads only vector lengths.
//! A query probes each distinct key and intersects the shortest lists first.
use super::content::Trigram;
use std::{collections::BTreeSet, io, ops::Range};

pub trait ExtentRead {
    fn length(&self) -> u64;
    fn read(&mut self, offset: u64, dest: &mut [u8]) -> io::Result<()>;
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
        if !matches!(stream, [1] | [2]) {
            return Err(bad());
        }
        let keys = word(&mut reader, 1)?;
        let starts_len = vector_end(9, keys, 12)?;
        let n = word(&mut reader, starts_len)?;
        if n != keys.checked_add(u64::from(stream == [1])).ok_or_else(bad)? {
            return Err(bad());
        }
        let starts = starts_len.checked_add(8).ok_or_else(bad)?;
        let ends = vector_end(starts, n, 8)?;
        let (anchors, count_offset) = if stream == [2] {
            let count = word(&mut reader, ends)?;
            if count != keys.div_ceil(super::content::SAMPLE as u64) {
                return Err(bad());
            }
            let base = ends.checked_add(8).ok_or_else(bad)?;
            (Some(base), vector_end(base, count, 8)?)
        } else {
            (None, ends)
        };
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
        let mut lo = 0;
        let mut hi = self.keys;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mut raw = [0; 12];
            self.reader.read(9 + mid * 12, &mut raw)?;
            let mut code = [0; 3];
            for (v, b) in code.iter_mut().zip(raw.as_chunks::<4>().0) {
                *v = u32::from_le_bytes(*b);
            }
            match code.cmp(&key.map(u32::from)) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
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
                    return Ok(a..b);
                }
            }
        }
        Ok(0..0)
    }
    pub fn candidates(
        &mut self,
        keys: BTreeSet<Trigram>,
        file_count: usize,
    ) -> io::Result<Vec<usize>> {
        if keys.is_empty() {
            return Ok((0..file_count).collect());
        }
        let mut lists = Vec::new();
        for key in keys {
            let r = self.range(key)?;
            if r.is_empty() {
                return Ok(Vec::new());
            }
            lists.push(r);
        }
        lists.sort_unstable_by_key(|r| r.end - r.start);
        let mut hits = Vec::new();
        let mut scratch = [0u8; 8192];
        for (index, r) in lists.into_iter().enumerate() {
            let mut pos = r.start;
            let mut read_hit = 0;
            let mut keep = 0;
            let mut prev = None;
            while pos < r.end {
                let size = (r.end - pos).min(scratch.len() as u64 / 8) as usize * 8;
                self.reader
                    .read(self.values + pos * 8, &mut scratch[..size])?;
                for b in scratch[..size].as_chunks::<8>().0 {
                    let id = usize::try_from(u64::from_le_bytes(*b)).map_err(|_| bad())?;
                    if id >= file_count || prev.is_some_and(|p| p >= id) {
                        return Err(bad());
                    }
                    prev = Some(id);
                    if index == 0 {
                        hits.push(id);
                    } else {
                        while hits.get(read_hit).is_some_and(|p| *p < id) {
                            read_hit += 1;
                        }
                        if hits.get(read_hit) == Some(&id) {
                            hits[keep] = id;
                            keep += 1;
                            read_hit += 1;
                        }
                    }
                }
                pos += size as u64 / 8;
                if index != 0 && read_hit == hits.len() {
                    break;
                }
            }
            if index != 0 {
                hits.truncate(keep);
                if hits.is_empty() {
                    break;
                }
            }
        }
        Ok(hits)
    }
}
