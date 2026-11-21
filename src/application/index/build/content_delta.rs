//! On a same-path edit, only lists in the symmetric difference of that
//! file's trigram sets need changes. Splice their individual owner IDs into
//! the predecessor's packed arrays. Other lists stay in their old segments.
use super::{Previous, retained::Run};
use crate::{
    application::index::{
        binary::bad,
        layout::{CONTENT, ExtentSink},
    },
    domain::{code::Unit, content::grams},
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
    starts: Vec<u8>,
    key_base: u64,
    old_keys: Vec<u8>,
    // Edits in ascending old physical position, including inserts at EOF.
    membership: Vec<(u64, Edit)>,
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
impl Delta {
    pub(super) fn plan(
        prior: &dyn Previous,
        units: &[Unit],
        before: &BTreeMap<usize, String>,
    ) -> io::Result<Option<Self>> {
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
        if changes.is_empty() {
            return Ok(None);
        }
        let old = prior.extent(CONTENT);
        let header = read(prior, old.start..end(old.start, 9, 1, old.end)?)?;
        if header[0] == 2 {
            return sampled::Patch::plan(prior, old, num(&header[1..]), changes)
                .map(|p| Some(Self::Sampled(p)));
        }
        if header[0] != 1 {
            return Err(bad());
        }
        let count = num(&header[1..]);
        let key_base = old.start + 9;
        let keys_end = end(key_base, count, 12, old.end)?;
        let start_base = end(keys_end, 8, 1, old.end)?;
        let starts_end = end(
            start_base,
            count.checked_add(1).ok_or_else(bad)?,
            8,
            old.end,
        )?;
        let values = end(starts_end, 8, 1, old.end)?;
        let old_keys = read(prior, key_base..keys_end)?;
        let old_starts = read(prior, start_base..starts_end)?;
        let old_total = num(&old_starts[old_starts.len() - 8..]);
        if num(&old_starts[..8]) != 0
            || num(&read(prior, keys_end..start_base)?) != count + 1
            || num(&read(prior, starts_end..values)?) != old_total
            || end(values, old_total, 8, old.end)? != old.end
        {
            return Err(bad());
        }
        let mut keys = Vec::with_capacity(old_keys.len());
        let mut starts = Vec::with_capacity(old_starts.len());
        starts.extend_from_slice(&0u64.to_le_bytes());
        let mut membership = Vec::new();
        let mut total = 0u64;
        let mut changes = changes.into_iter().peekable();
        let mut slot = 0;
        let start = |i: usize| num(&old_starts[i * 8..][..8]);
        while slot < old_keys.len() / 12 || changes.peek().is_some() {
            let raw = old_keys.get(slot * 12..slot * 12 + 12);
            let old_key = raw.map(gram).transpose()?;
            let key = match (old_key, changes.peek()) {
                (Some(a), Some((b, _))) => a.min(*b),
                (Some(a), None) => a,
                (None, Some((b, _))) => *b,
                _ => unreachable!(),
            };
            let a = start(slot);
            let b = if old_key == Some(key) {
                slot += 1;
                start(slot)
            } else {
                a
            };
            if a > b || b > old_total {
                return Err(bad());
            }
            let mut len = b - a;
            if let Some((_, edits)) = changes.next_if(|(k, _)| *k == key) {
                let ids = read(prior, values + 8 * a..values + 8 * b)?;
                let words = ids.as_chunks::<8>().0;
                for edit in edits {
                    let index = words.partition_point(|w| u64::from_le_bytes(*w) < edit.id);
                    let present = words
                        .get(index)
                        .is_some_and(|w| u64::from_le_bytes(*w) == edit.id);
                    if present == edit.add {
                        return Err(bad());
                    }
                    if edit.add {
                        len += 1
                    } else {
                        len -= 1
                    }
                    membership.push((values + 8 * (a + index as u64), edit));
                }
            }
            if len == 0 {
                continue;
            }
            total = total.checked_add(len).ok_or_else(bad)?;
            if old_key == Some(key) {
                keys.extend_from_slice(raw.unwrap())
            } else {
                for shift in [42, 21, 0] {
                    keys.extend_from_slice(&(((key >> shift) & 0x1f_ffff) as u32).to_le_bytes())
                }
            }
            starts.extend_from_slice(&total.to_le_bytes());
        }
        Ok(Some(Self::Legacy(Legacy {
            old,
            keys,
            starts,
            key_base,
            old_keys,
            membership,
            values,
            total,
        })))
    }
    pub(super) fn write(self, prior: &dyn Previous, w: &mut impl ExtentSink) -> io::Result<()> {
        match self {
            Self::Legacy(p) => p.write(prior, w),
            Self::Sampled(p) => p.write(prior, w),
        }
    }
}
impl Legacy {
    fn write(self, prior: &dyn Previous, w: &mut impl ExtentSink) -> io::Result<()> {
        let mut run = Run::new(Some(prior), w);
        // Transcode just the directory on the first changed legacy product.
        // The keys and all unaffected owner IDs stay in their original extents.
        run.bytes(&[2])?;
        run.bytes(&((self.keys.len() / 12) as u64).to_le_bytes())?;
        run.replacement(self.key_base, &self.old_keys, &self.keys)?;
        let mut directory = Vec::with_capacity(self.starts.len() + self.starts.len() / 64 + 16);
        crate::domain::content::sampled_lengths(
            &mut directory,
            self.starts
                .as_chunks::<8>()
                .0
                .windows(2)
                .map(|w| u64::from_le_bytes(w[1]) - u64::from_le_bytes(w[0])),
        )?;
        run.bytes(&directory)?;
        run.bytes(&self.total.to_le_bytes())?;
        let mut last = self.values;
        for (offset, edit) in self.membership {
            run.borrow(last..offset)?;
            if edit.add {
                run.bytes(&edit.id.to_le_bytes())?;
                last = offset
            } else {
                last = offset + 8
            }
        }
        run.borrow(last..self.old.end)?;
        run.flush()
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

    struct Prior(Vec<u8>);
    impl Previous for Prior {
        fn extent(&self, kind: usize) -> Range<u64> {
            assert_eq!(kind, CONTENT);
            0..self.0.len() as u64
        }
        fn copy_bytes(&self, range: Range<u64>, out: &mut dyn io::Write) -> io::Result<()> {
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
            let prior = Prior(encode(&files, sampled));
            let mut sink = MemorySink::default();
            Delta::plan(&prior, &units, &before)
                .unwrap()
                .unwrap()
                .write(&prior, &mut sink)
                .unwrap();
            assert!(sink.0 == expected, "legacy={}", !sampled);
        }
    }
}
