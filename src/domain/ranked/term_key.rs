//! A dictionary probe reads the length and the start of a term in one read.
//! Short keys fit in the probe. Longer ones need their suffix only on a tie.
use super::storage::{get, invalid};
use std::{borrow::Cow, cmp::Ordering, io, ops::Range};

pub(super) const PROBE: usize = 10;

pub(super) struct Key<'a> {
    known: &'a [u8],
    suffix: Range<u64>,
    posting_prefix: &'a [u8],
    postings: Range<u64>,
}

impl<'a> Key<'a> {
    pub fn parse(record: Range<u64>, probe: &'a [u8]) -> io::Result<Self> {
        if record
            .end
            .checked_sub(record.start)
            .is_none_or(|len| (probe.len() as u64) > len)
        {
            return Err(invalid());
        }
        let mut data = probe;
        let len = get(&mut data)?;
        let start = record
            .start
            .checked_add((probe.len() - data.len()) as u64)
            .ok_or_else(invalid)?;
        let end = start.checked_add(len as u64).ok_or_else(invalid)?;
        if len == 0 || len > super::token::MAX_KEY || end > record.end {
            return Err(invalid());
        }
        let (known, posting_prefix) = data.split_at(data.len().min(len));
        Ok(Self {
            known,
            suffix: start + known.len() as u64..end,
            posting_prefix,
            postings: end..record.end,
        })
    }

    pub fn compare(
        &self,
        key: &[u8],
        fetch: impl FnOnce(Range<u64>) -> io::Result<Vec<u8>>,
    ) -> io::Result<Ordering> {
        let overlap = self.known.len().min(key.len());
        let order = self.known[..overlap].cmp(&key[..overlap]);
        if order != Ordering::Equal {
            return Ok(order);
        }
        if self.suffix.is_empty() || overlap == key.len() {
            let len = self.known.len() + (self.suffix.end - self.suffix.start) as usize;
            return Ok(len.cmp(&key.len()));
        }
        Ok(fetch(self.suffix.clone())?.as_slice().cmp(&key[overlap..]))
    }

    pub fn owned(
        &self,
        fetch: impl FnOnce(Range<u64>) -> io::Result<Vec<u8>>,
    ) -> io::Result<Vec<u8>> {
        if self.suffix.is_empty() {
            return Ok(self.known.to_vec());
        }
        let mut key =
            Vec::with_capacity(self.known.len() + (self.suffix.end - self.suffix.start) as usize);
        key.extend_from_slice(self.known);
        key.extend_from_slice(&fetch(self.suffix.clone())?);
        Ok(key)
    }

    pub fn postings(
        &self,
        read: impl FnOnce(u64, &mut [u8]) -> io::Result<()>,
    ) -> io::Result<Cow<'a, [u8]>> {
        let size =
            usize::try_from(self.postings.end - self.postings.start).map_err(|_| invalid())?;
        let cached = self.posting_prefix.len();
        if size == cached {
            return Ok(Cow::Borrowed(self.posting_prefix));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(io::Error::other)?;
        bytes.extend_from_slice(self.posting_prefix);
        bytes.resize(size, 0);
        read(self.postings.start + cached as u64, &mut bytes[cached..])?;
        Ok(Cow::Owned(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_skips_proven_misses_and_reads_only_an_unknown_suffix() {
        let record = b"\x0cabcdefghijklDATA";
        let key = Key::parse(0..record.len() as u64, &record[..PROBE]).unwrap();
        for (needle, expected) in [
            (b"aa".as_slice(), Ordering::Greater),
            (b"b", Ordering::Less),
            (b"abcdefghi", Ordering::Greater),
        ] {
            assert_eq!(
                key.compare(needle, |_| panic!("known prefix decides the ordering"))
                    .unwrap(),
                expected
            );
        }
        assert_eq!(
            key.compare(b"abcdefghijkl", |range| {
                assert_eq!(range, 10..13);
                Ok(b"jkl".to_vec())
            })
            .unwrap(),
            Ordering::Equal
        );
        let short = b"\x03cat123456789";
        let key = Key::parse(0..short.len() as u64, &short[..PROBE]).unwrap();
        assert_eq!(
            key.owned(|_| panic!("short key is already resident"))
                .unwrap(),
            b"cat"
        );
        assert_eq!(
            &*key
                .postings(|at, out| {
                    assert_eq!(at, PROBE as u64);
                    out.copy_from_slice(&short[PROBE..]);
                    Ok(())
                })
                .unwrap(),
            b"123456789"
        );
    }
}
