//! Merge two sorted posting streams without allocating a list for every old
//! trigram or sorting the unchanged dictionary again.
use super::{ContentBuilder, ContentRun, invalid, number};
use std::io::{self, Write};

pub(crate) struct MergedPostings {
    keys: Vec<u8>,
    ends: Vec<u8>,
    ids: Vec<u8>,
}

impl MergedPostings {
    fn finish_key(&mut self, key: u64) {
        self.keys.extend_from_slice(&super::encoded_key(key));
        self.ends
            .extend_from_slice(&(self.ids.len() as u64 / 8).to_le_bytes());
    }

    fn added_key(&mut self, key: u64, ids: Vec<u64>) {
        for id in ids {
            self.ids.extend_from_slice(&id.to_le_bytes());
        }
        self.finish_key(key);
    }

    fn merge_posting(
        &mut self,
        key: u64,
        old: &[u8],
        added: Option<Vec<u64>>,
        mapping: &Remapping<'_>,
    ) -> io::Result<()> {
        if added.is_none() && mapping.contains_posting(old)? {
            self.ids.extend_from_slice(old);
            self.finish_key(key);
            return Ok(());
        }
        self.merge_owners(key, old, added, mapping.rows)
    }

    fn merge_owners(
        &mut self,
        key: u64,
        old: &[u8],
        added: Option<Vec<u64>>,
        remap: &[Option<usize>],
    ) -> io::Result<()> {
        let mut new = added.into_iter().flatten().peekable();
        let before = self.ids.len();
        for bytes in old.as_chunks::<8>().0 {
            let id = usize::try_from(u64::from_le_bytes(*bytes)).map_err(|_| invalid())?;
            if let Some(mapped) = *remap.get(id).ok_or_else(invalid)? {
                let mapped = mapped as u64;
                while let Some(id) = new.next_if(|&id| id < mapped) {
                    self.ids.extend_from_slice(&id.to_le_bytes());
                }
                self.ids.extend_from_slice(&mapped.to_le_bytes());
            }
        }
        for id in new {
            self.ids.extend_from_slice(&id.to_le_bytes());
        }
        if self.ids.len() != before {
            self.finish_key(key);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn seal(self, out: impl Write) -> io::Result<()> {
        self.seal_layout(out, false)
    }
    pub(crate) fn seal_layout(self, mut out: impl Write, sampled: bool) -> io::Result<()> {
        if sampled {
            out.write_all(&crate::domain::content::PostingLayout::Sampled.tag())?;
            out.write_all(&((self.keys.len() / 12) as u64).to_le_bytes())?;
            out.write_all(&self.keys)?;
            let offsets = self.ends.as_chunks::<8>().0;
            let total = super::sampled_lengths(
                &mut out,
                offsets
                    .windows(2)
                    .map(|w| u64::from_le_bytes(w[1]) - u64::from_le_bytes(w[0])),
            )?;
            out.write_all(&total.to_le_bytes())?;
            return out.write_all(&self.ids);
        }

        out.write_all(&crate::domain::content::PostingLayout::Offsets.tag())?;
        for (bytes, width) in [(self.keys, 12), (self.ends, 8), (self.ids, 8)] {
            out.write_all(&(bytes.len() as u64 / width).to_le_bytes())?;
            out.write_all(&bytes)?;
        }
        Ok(())
    }
}

impl ContentBuilder {
    pub(crate) fn merge_prior(
        self,
        prior: &ContentRun<'_>,
        remap: &[Option<usize>],
    ) -> io::Result<MergedPostings> {
        let mut changes: Vec<_> = self.lists.into_iter().collect();
        changes.sort_unstable_by_key(|(key, _)| *key);
        let mut changes = changes.into_iter().peekable();
        let mut out = MergedPostings {
            keys: Vec::with_capacity(prior.keys.len()),
            ends: Vec::with_capacity(prior.starts.len()),
            ids: Vec::with_capacity(prior.ids.len()),
        };
        out.ends.extend_from_slice(&0u64.to_le_bytes());
        // An extent whose first and last owners fall in an identity interval
        // can copy its complete encoded list when the key has no new owners.
        let mapping = Remapping::new(remap);

        for record in prior.lists() {
            let (gram, old) = record?;
            let [a, b, c] = gram.map(u64::from);
            let key = (a << 42) | (b << 21) | c;
            while let Some((added, ids)) = changes.next_if(|(k, _)| *k < key) {
                out.added_key(added, ids);
            }
            let added = changes.next_if(|(k, _)| *k == key);
            out.merge_posting(key, old, added.map(|(_, ids)| ids), &mapping)?;
        }
        for (key, ids) in changes {
            out.added_key(key, ids);
        }
        Ok(out)
    }
}

struct Remapping<'a> {
    rows: &'a [Option<usize>],
    identity_ends: Vec<usize>,
}

impl<'a> Remapping<'a> {
    fn new(remap: &'a [Option<usize>]) -> Self {
        let mut stop = remap.len();
        let mut ends = vec![stop; remap.len()];
        for (id, &mapped) in remap.iter().enumerate().rev() {
            if mapped != Some(id) {
                stop = id;
            }
            ends[id] = stop;
        }
        Self {
            rows: remap,
            identity_ends: ends,
        }
    }

    fn contains_posting(&self, posting: &[u8]) -> io::Result<bool> {
        let first = number(posting.first_chunk::<8>().ok_or_else(invalid)?);
        let last = number(posting.last_chunk::<8>().ok_or_else(invalid)?);
        let start = usize::try_from(first).map_err(|_| invalid())?;
        Ok(last < *self.identity_ends.get(start).ok_or_else(invalid)? as u64)
    }
}
