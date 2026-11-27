//! Seek the first destination list. Its entries are sorted by source file and link
//! ordinal. Only a document-wide query spanning several anchors needs a merge.
use super::{ExtentRead, Lookup, Range, bad, io, row, word};

use std::{
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
};

pub(crate) const BACKLINK_LIMIT: usize = 50;
const WIDTH: usize = 16;
type Hit = (usize, usize);

impl Lookup {
    fn key(&self, r: &mut impl ExtentRead, id: u64) -> io::Result<(String, String, Range<u64>)> {
        let mut record = row(r, &self.destinations, id, self.end)?;
        let (path, anchor) = super::address(r, &mut record)?;
        let count = word(r, record.start)?;
        record.start += 8;
        if count
            .checked_mul(16)
            .and_then(|n| n.checked_add(record.start))
            != Some(record.end)
        {
            return Err(bad());
        }
        Ok((path, anchor, record))
    }

    fn first_list(
        &self,
        r: &mut impl ExtentRead,
        path: &str,
        anchor: Option<&str>,
        count: u64,
    ) -> io::Result<Option<(u64, Range<u64>)>> {
        let (mut low, mut high) = (0, count);
        let mut first = None;
        while low < high {
            let mid = low + (high - low) / 2;
            let (p, a, range) = self.key(r, mid)?;
            match (p.as_str(), a.as_str()).cmp(&(path, anchor.unwrap_or(""))) {
                Ordering::Equal => return Ok(Some((mid, range))),
                Ordering::Less => low = mid + 1,
                Ordering::Greater => {
                    high = mid;
                    if anchor.is_none() && p == path {
                        first = Some((mid, range));
                    }
                }
            }
        }
        Ok(first)
    }

    pub fn backlinks(
        &self,
        mut r: impl ExtentRead,
        path: &str,
        anchor: Option<&str>,
    ) -> io::Result<(Vec<Hit>, bool)> {
        let count = (self.destinations.end - self.destinations.start) / 8 - 1;
        let Some((id, first)) = self.first_list(&mut r, path, anchor, count)? else {
            return Ok((Vec::new(), false));
        };
        let mut merge = Merge::default();
        if anchor.is_none() {
            for next in id + 1..count {
                let (other, _, range) = self.key(&mut r, next)?;
                if other != path {
                    break;
                }
                merge.enqueue(&mut r, range)?;
            }
        }
        if merge.0.is_empty() {
            return single_page(&mut r, first);
        }
        merge.enqueue(&mut r, first)?;
        merge.page(&mut r)
    }
}

fn hit(bytes: &[u8]) -> Hit {
    let (file, link) = bytes.split_at(8);
    (
        u64::from_le_bytes(file.try_into().unwrap()) as usize,
        u64::from_le_bytes(link.try_into().unwrap()) as usize,
    )
}

fn single_page(r: &mut impl ExtentRead, range: Range<u64>) -> io::Result<(Vec<Hit>, bool)> {
    let len = range.end - range.start;
    let mut buffer = [0; WIDTH * BACKLINK_LIMIT];
    let used = len.min(buffer.len() as u64) as usize;
    r.read(range.start, &mut buffer[..used])?;
    Ok((
        buffer[..used].chunks_exact(WIDTH).map(hit).collect(),
        used as u64 != len,
    ))
}

#[derive(Default)]
struct Merge(BinaryHeap<Reverse<(Hit, u64, u64)>>);

impl Merge {
    fn enqueue(&mut self, r: &mut impl ExtentRead, range: Range<u64>) -> io::Result<()> {
        if !range.is_empty() {
            let mut bytes = [0; WIDTH];
            r.read(range.start, &mut bytes)?;
            self.0.push(Reverse((
                hit(&bytes),
                range.start + WIDTH as u64,
                range.end,
            )));
        }
        Ok(())
    }

    fn page(mut self, r: &mut impl ExtentRead) -> io::Result<(Vec<Hit>, bool)> {
        let mut hits = Vec::new();
        while let Some(Reverse((hit, start, end))) = self.0.pop() {
            hits.push(hit);
            if hits.len() == BACKLINK_LIMIT {
                return Ok((hits, start < end || !self.0.is_empty()));
            }
            self.enqueue(r, start..end)?;
        }
        Ok((hits, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_page_reads_its_postings_in_one_bounded_request() {
        struct Reader {
            bytes: Vec<u8>,
            requests: Vec<(u64, usize)>,
        }
        impl ExtentRead for Reader {
            fn length(&self) -> u64 {
                self.bytes.len() as u64
            }
            fn read(&mut self, offset: u64, dest: &mut [u8]) -> io::Result<()> {
                self.requests.push((offset, dest.len()));
                dest.copy_from_slice(&self.bytes[offset as usize..offset as usize + dest.len()]);
                Ok(())
            }
        }
        for count in [1, BACKLINK_LIMIT, BACKLINK_LIMIT + 1] {
            let mut r = Reader {
                bytes: vec![0; 8],
                requests: vec![],
            };
            for i in 0..count as u64 {
                r.bytes.extend(i.to_le_bytes());
                r.bytes.extend((i + 1).to_le_bytes());
            }
            let end = r.length();
            let (hits, extra) = single_page(&mut r, 8..end).unwrap();
            let n = count.min(BACKLINK_LIMIT);
            assert_eq!(hits, (0..n).map(|i| (i, i + 1)).collect::<Vec<_>>());
            assert_eq!(extra, count > n);
            assert_eq!(r.requests, [(8, n * WIDTH)]);
        }
    }
}
