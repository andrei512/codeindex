//! Keep the old key and length rows until an individual membership changes.
//! Only the sampled absolute offsets can move after an unchanged dictionary row.
use super::{Columns, Edit, ExtentSink, MemberEdits, Previous, Run, bad, end, gram, read};
use crate::domain::content::SAMPLE;
use std::{collections::BTreeMap, io, ops::Range};

#[derive(Clone, Copy)]
enum Column {
    Keys,
    Lengths,
}

struct Row {
    slot: usize,
    key: u64,
    old: u64,
    new: u64,
}

struct PriorRows<'a> {
    keys: &'a [u8],
    counts: &'a [[u8; 8]],
    slot: usize,
    start: u64,
    total: u64,
}

impl PriorRows<'_> {
    fn seek(&mut self, target: u64, mut retain: impl FnMut(u64) -> io::Result<()>) -> io::Result<u64> {
        while let Some(&word) = self.counts.get(self.slot) {
            let key = gram(&self.keys[self.slot * 12..][..12])?;
            let length = u64::from_le_bytes(word);
            match key.cmp(&target) {
                std::cmp::Ordering::Equal => return Ok(length),
                std::cmp::Ordering::Greater => return Ok(0),
                std::cmp::Ordering::Less => {}
            }
            retain(length)?;
            self.consume(length)?;
        }
        Ok(0)
    }

    fn consume(&mut self, length: u64) -> io::Result<()> {
        self.start = self.start.checked_add(length)
            .filter(|&start| start <= self.total)
            .ok_or_else(bad)?;
        self.slot += 1;
        Ok(())
    }

    fn rest(self, mut retain: impl FnMut(u64) -> io::Result<()>) -> io::Result<()> {
        let mut end = self.start;
        for &word in &self.counts[self.slot..] {
            let length = u64::from_le_bytes(word);
            retain(length)?;
            end = end.checked_add(length).ok_or_else(bad)?;
        }
        if end != self.total {
            return Err(bad());
        }
        Ok(())
    }
}

impl Row {
    fn column_changes(&self, column: Column) -> bool {
        match column {
            Column::Keys => (self.old == 0) != (self.new == 0),
            Column::Lengths => self.old != self.new,
        }
    }

    fn write(&self, run: &mut Run<'_, impl ExtentSink>, column: Column) -> io::Result<()> {
        if self.new == 0 {
            return Ok(());
        }
        match column {
            Column::Keys => run.bytes(&crate::domain::content::encoded_key(self.key)),
            Column::Lengths => run.bytes(&self.new.to_le_bytes()),
        }
    }
}

pub(in crate::application::index::build) struct Patch {
    rows: Vec<Row>,
    members: MemberEdits,
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
        let mut columns = Columns {
            prior: p,
            at: old.start + 9,
            limit: old.end,
        };
        let key_range = columns.take(n, 12)?;
        let length_range = columns.counted(n, 8)?;
        let anchor_range = columns.counted(n.div_ceil(SAMPLE as u64), 8)?;
        let (values, old_total) = columns.values()?;
        let (keys, lengths, anchors) = (key_range.start, length_range.start, anchor_range.start);
        let old_keys = read(p, key_range)?;
        let old_lengths = read(p, length_range)?;
        let old_anchors = read(p, anchor_range)?;
        let counts = old_lengths.as_chunks::<8>().0;
        let mut patch = Self {
            rows: Vec::with_capacity(changes.len()),
            members: MemberEdits::default(),
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
        let mut prior = PriorRows { keys: &old_keys, counts, slot: 0, start: 0, total: old_total };
        for (target, edits) in changes {
            let old_len = prior.seek(target, |len| patch.advance(len))?;
            patch.edit_row(p, &mut prior, (target, old_len), edits)?;
        }
        prior.rest(|len| patch.advance(len))?;
        Ok(patch)
    }

    fn edit_row(&mut self, prior: &dyn Previous, cursor: &mut PriorRows<'_>, (key, old): (u64, u64), edits: Vec<Edit>) -> io::Result<()> {
        let base = self.values + cursor.start * 8;
        let stop = end(base, old, 8, self.old.end)?;
        let new = self.members.record(prior, base..stop, edits)?;
        self.rows.push(Row { slot: cursor.slot, key, old, new });
        if new != 0 {
            self.advance(new)?;
        }
        if old != 0 {
            cursor.consume(old)?;
        }
        Ok(())
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

    fn write_column(&self, run: &mut Run<'_, impl ExtentSink>, column: Column) -> io::Result<()> {
        let (base, width) = match column {
            Column::Keys => (self.keys, 12),
            Column::Lengths => (self.lengths, 8),
        };
        run.bytes(&(self.new_count as u64).to_le_bytes())?;
        let mut last = base;
        for row in self.rows.iter().filter(|row| row.column_changes(column)) {
            let at = base + row.slot as u64 * width;
            run.borrow(last..at)?;
            row.write(run, column)?;
            last = at + u64::from(row.old != 0) * width;
        }
        run.borrow(last..base + self.old_count as u64 * width)?;
        Ok(())
    }

    pub(super) fn write(self, p: &dyn Previous, w: &mut impl ExtentSink) -> io::Result<()> {
        let mut run = Run::new(Some(p), w);
        run.bytes(&crate::domain::content::PostingLayout::Sampled.tag())?;
        self.write_column(&mut run, Column::Keys)?;
        self.write_column(&mut run, Column::Lengths)?;
        run.bytes(&((self.new_anchors.len() / 8) as u64).to_le_bytes())?;
        run.replacement(self.anchors, &self.old_anchors, &self.new_anchors)?;
        run.bytes(&self.total.to_le_bytes())?;
        self.members.write(run, self.values..self.old.end)
    }
}
