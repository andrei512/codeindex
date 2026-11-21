//! Keep the old key and length rows until an individual membership changes.
//! Only the sampled absolute offsets can move after an unchanged dictionary row.
use super::{Edit, ExtentSink, Previous, Run, bad, end, gram, num, read};
use crate::domain::content::SAMPLE;
use std::{collections::BTreeMap, io, ops::Range};

struct Row {
    slot: usize,
    key: u64,
    old: u64,
    new: u64,
}

pub(in crate::application::index::build) struct Patch {
    rows: Vec<Row>,
    members: Vec<(u64, Edit)>,
    old: Range<u64>,
    keys: u64,
    lengths: u64,
    anchors: u64,
    values: u64,
    old_count: usize,
    new_count: usize,
    total: u64,
    old_anchors: Vec<u8>,
    new_anchors: Vec<u8>,
}

impl Patch {
    pub(super) fn plan(
        p: &dyn Previous,
        old: Range<u64>,
        n: u64,
        changes: BTreeMap<u64, Vec<Edit>>,
    ) -> io::Result<Self> {
        let keys = old.start + 9;
        let lens_header = end(keys, n, 12, old.end)?;
        let lengths = end(lens_header, 1, 8, old.end)?;
        let anchors_header = end(lengths, n, 8, old.end)?;
        let anchors = end(anchors_header, 1, 8, old.end)?;
        let samples = n.div_ceil(SAMPLE as u64);
        let ids_header = end(anchors, samples, 8, old.end)?;
        let values = end(ids_header, 1, 8, old.end)?;
        if num(&read(p, lens_header..lengths)?) != n
            || num(&read(p, anchors_header..anchors)?) != samples
        {
            return Err(bad());
        }
        let old_total = num(&read(p, ids_header..values)?);
        if end(values, old_total, 8, old.end)? != old.end {
            return Err(bad());
        }
        let old_keys = read(p, keys..lens_header)?;
        let old_lengths = read(p, lengths..anchors_header)?;
        let old_anchors = read(p, anchors..ids_header)?;
        let counts = old_lengths.as_chunks::<8>().0;
        let mut patch = Self {
            rows: Vec::with_capacity(changes.len()),
            members: vec![],
            old,
            keys,
            lengths,
            anchors,
            values,
            old_count: counts.len(),
            new_count: 0,
            total: 0,
            old_anchors,
            new_anchors: vec![],
        };
        let mut at = 0;
        let mut start = 0u64;
        for (target, edits) in changes {
            let mut old_len = 0;
            while let Some(&len) = counts.get(at) {
                let key = gram(&old_keys[at * 12..][..12])?;
                old_len = u64::from_le_bytes(len);
                if key == target {
                    break;
                }
                if key > target {
                    old_len = 0;
                    break;
                }
                patch.advance(old_len)?;
                start = start
                    .checked_add(old_len)
                    .filter(|&s| s <= old_total)
                    .ok_or_else(bad)?;
                at += 1;
                old_len = 0;
            }
            let base = values + start * 8;
            let stop = end(base, old_len, 8, patch.old.end)?;
            let local = if old_len <= 256 {
                Some(read(p, base..stop)?)
            } else {
                None
            };
            let mut new_len = old_len;
            for edit in edits {
                let read_id = |idx: u64| -> io::Result<u64> {
                    if let Some(data) = &local {
                        Ok(num(&data[idx as usize * 8..][..8]))
                    } else {
                        Ok(num(&read(p, base + idx * 8..base + idx * 8 + 8)?))
                    }
                };
                let (mut lo, mut hi) = (0, old_len);
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    if read_id(mid)? < edit.id {
                        lo = mid + 1
                    } else {
                        hi = mid
                    }
                }
                let present = lo < old_len && read_id(lo)? == edit.id;
                if present == edit.add {
                    return Err(bad());
                }
                new_len = if edit.add {
                    new_len.checked_add(1)
                } else {
                    new_len.checked_sub(1)
                }
                .ok_or_else(bad)?;
                patch.members.push((base + lo * 8, edit));
            }
            patch.rows.push(Row {
                slot: at,
                key: target,
                old: old_len,
                new: new_len,
            });
            if new_len != 0 {
                patch.advance(new_len)?;
            }
            if old_len != 0 {
                start = start
                    .checked_add(old_len)
                    .filter(|&s| s <= old_total)
                    .ok_or_else(bad)?;
                at += 1;
            }
        }
        for &word in &counts[at..] {
            let n = u64::from_le_bytes(word);
            patch.advance(n)?;
            start = start.checked_add(n).ok_or_else(bad)?;
        }
        if start != old_total {
            return Err(bad());
        }
        Ok(patch)
    }

    fn advance(&mut self, n: u64) -> io::Result<()> {
        if n == 0 {
            return Err(bad());
        }
        if self.new_count.is_multiple_of(SAMPLE) {
            self.new_anchors.extend(self.total.to_le_bytes());
        }
        self.new_count += 1;
        self.total = self.total.checked_add(n).ok_or_else(bad)?;
        Ok(())
    }

    pub(super) fn write(self, p: &dyn Previous, w: &mut impl ExtentSink) -> io::Result<()> {
        let mut run = Run::new(Some(p), w);
        run.bytes(&[2])?;
        for key_column in [true, false] {
            let (base, width) = if key_column {
                (self.keys, 12)
            } else {
                (self.lengths, 8)
            };
            run.bytes(&(self.new_count as u64).to_le_bytes())?;
            let mut last = base;
            for row in &self.rows {
                if (key_column && row.old != 0 && row.new != 0) || row.old == row.new {
                    continue;
                }
                let at = base + row.slot as u64 * width;
                run.borrow(last..at)?;
                if row.new != 0 {
                    if key_column {
                        let mut bytes = [0; 12];
                        for (b, shift) in bytes.chunks_exact_mut(4).zip([42, 21, 0]) {
                            b.copy_from_slice(
                                &(((row.key >> shift) & 0x1f_ffff) as u32).to_le_bytes(),
                            )
                        }
                        run.bytes(&bytes)?;
                    } else {
                        run.bytes(&row.new.to_le_bytes())?
                    }
                }
                last = at + u64::from(row.old != 0) * width;
            }
            run.borrow(last..base + self.old_count as u64 * width)?;
        }
        run.bytes(&((self.new_anchors.len() / 8) as u64).to_le_bytes())?;
        run.replacement(self.anchors, &self.old_anchors, &self.new_anchors)?;
        run.bytes(&self.total.to_le_bytes())?;
        let mut last = self.values;
        for (at, edit) in self.members {
            run.borrow(last..at)?;
            if edit.add {
                run.bytes(&edit.id.to_le_bytes())?;
                last = at
            } else {
                last = at + 8
            }
        }
        run.borrow(last..self.old.end)?;
        run.flush()
    }
}
