//! Merge two sorted posting streams without allocating a list for every old
//! trigram or sorting the unchanged dictionary again.
use super::{ContentBuilder, ContentRun, SCALAR_MASK, invalid, number};
use std::io::{self, Write};

pub(crate) struct MergedPostings {
    keys: Vec<u8>,
    ends: Vec<u8>,
    ids: Vec<u8>,
}

impl MergedPostings {
    fn finish_key(&mut self, key: u64) {
        for shift in [42, 21, 0] {
            let scalar = ((key >> shift) & SCALAR_MASK) as u32;
            self.keys.extend_from_slice(&scalar.to_le_bytes());
        }
        self.ends
            .extend_from_slice(&(self.ids.len() as u64 / 8).to_le_bytes());
    }

    fn added_key(&mut self, key: u64, ids: Vec<u64>) {
        for id in ids {
            self.ids.extend_from_slice(&id.to_le_bytes());
        }
        self.finish_key(key);
    }

    #[cfg(test)]
    pub(crate) fn seal(self, out: impl Write) -> io::Result<()> {
        self.seal_layout(out, false)
    }
    pub(crate) fn seal_layout(self, mut out: impl Write, sampled: bool) -> io::Result<()> {
        if sampled {
            out.write_all(&[2])?;
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

        out.write_all(&[1])?;
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
        let mut identity_stop = vec![0; remap.len()];
        let mut stop = remap.len();
        for (id, &new) in remap.iter().enumerate().rev() {
            if new != Some(id) {
                stop = id;
            }
            identity_stop[id] = stop;
        }

        for slot in 0..prior.keys.len() / 12 {
            let [a, b, c] = prior.key(slot)?.map(u64::from);
            let key = (a << 42) | (b << 21) | c;
            while let Some((added, ids)) = changes.next_if(|(k, _)| *k < key) {
                out.added_key(added, ids);
            }
            let added = changes.next_if(|(k, _)| *k == key);
            let old = prior.values(slot)?;
            let first = usize::try_from(number(old.get(..8).ok_or_else(invalid)?))
                .map_err(|_| invalid())?;
            let last = usize::try_from(number(&old[old.len() - 8..])).map_err(|_| invalid())?;
            if added.is_none() && last < *identity_stop.get(first).ok_or_else(invalid)? {
                out.ids.extend_from_slice(old);
                out.finish_key(key);
                continue;
            }
            let mut new = added.into_iter().flat_map(|(_, ids)| ids).peekable();
            let before = out.ids.len();
            for bytes in old.as_chunks::<8>().0 {
                let id = usize::try_from(u64::from_le_bytes(*bytes)).map_err(|_| invalid())?;
                if let Some(mapped) = *remap.get(id).ok_or_else(invalid)? {
                    let mapped = mapped as u64;
                    while let Some(id) = new.next_if(|&id| id < mapped) {
                        out.ids.extend_from_slice(&id.to_le_bytes());
                    }
                    out.ids.extend_from_slice(&mapped.to_le_bytes());
                }
            }
            for id in new {
                out.ids.extend_from_slice(&id.to_le_bytes());
            }
            if out.ids.len() != before {
                out.finish_key(key);
            }
        }
        for (key, ids) in changes {
            out.added_key(key, ids);
        }
        Ok(out)
    }
}
