//! Merge an edited file's postings with the validated, unmodified groups.
use super::{Change, get, invalid};
use crate::domain::ranked::bulk::varint;
use std::io;

struct Group<'a> {
    file: usize,
    count: usize,
    raw: &'a [u8],
}

struct Groups<'a> {
    data: &'a [u8],
    bases: &'a [usize],
    previous: Option<usize>,
    remaining: usize,
    head: Option<Group<'a>>,
}

impl<'a> Groups<'a> {
    fn copy_before(&mut self, limit: usize, out: &mut Vec<u8>) -> io::Result<()> {
        while let Some(group) = self.head.take_if(|g| g.file < limit) {
            out.extend_from_slice(group.raw);
            self.head = self.next()?;
        }
        Ok(())
    }

    fn replace(&mut self, change: Change, out: &mut Vec<u8>) -> io::Result<()> {
        self.copy_before(change.file, out)?;
        let removed = match self.head.take_if(|g| g.file == change.file) {
            Some(group) => {
                self.head = self.next()?;
                group.count
            }
            None => 0,
        };
        if removed != change.old_count {
            return Err(invalid());
        }
        write_group(out, change);
        Ok(())
    }

    fn next(&mut self) -> io::Result<Option<Group<'a>>> {
        let raw = self.data;
        if raw.is_empty() {
            return (self.remaining == 0).then_some(None).ok_or_else(invalid);
        }
        let file = get(&mut self.data)?;
        let count = get(&mut self.data)?;
        if count == 0 || self.previous.is_some_and(|p| p >= file) {
            return Err(invalid());
        }
        self.remaining = self.remaining.checked_sub(count).ok_or_else(invalid)?;
        let [start, end] = self.bases.get(file..).and_then(|tail| tail.first_chunk::<2>()).ok_or_else(invalid)?;
        let limit = end - start;
        super::super::storage::visit_positions(&mut self.data, 0..limit, count, |_| {})?;
        self.previous = Some(file);
        Ok(Some(Group {
            file,
            count,
            raw: &raw[..raw.len() - self.data.len()],
        }))
    }
}

fn write_group(out: &mut Vec<u8>, change: Change) {
    if !change.new.positions.is_empty() {
        varint(out, change.file);
        varint(out, change.new.positions.len());
        let mut last = 0;
        for pos in change.new.positions {
            varint(out, pos - last);
            last = pos;
        }
    }
}

fn updated_counts(
    mut df: usize,
    original: usize,
    changes: &[Change],
) -> io::Result<(usize, usize)> {
    let mut total = original;
    for change in changes {
        df = df
            .checked_sub(change.old_df)
            .and_then(|n| n.checked_add(change.new.document_frequency))
            .ok_or_else(invalid)?;
        total = total
            .checked_sub(change.old_count)
            .and_then(|n| n.checked_add(change.new.positions.len()))
            .ok_or_else(invalid)?;
    }
    if df > total || (df == 0) != (total == 0) {
        return Err(invalid());
    }
    Ok((df, total))
}

pub(super) fn replace_term(
    key: &[u8],
    old: &[u8],
    changes: Vec<Change>,
    bases: &[usize],
) -> io::Result<Vec<u8>> {
    let mut data = old;
    let (df, original) = if old.is_empty() {
        (0, 0)
    } else {
        let n = get(&mut data)?;
        if n != key.len() || !data.starts_with(key) {
            return Err(invalid());
        }
        data = &data[n..];
        (get(&mut data)?, get(&mut data)?)
    };
    let (df, total) = updated_counts(df, original, &changes)?;
    let mut out = Vec::new();
    if total > 0 {
        varint(&mut out, key.len());
        out.extend_from_slice(key);
        varint(&mut out, df);
        varint(&mut out, total);
    }
    let mut groups = Groups {
        data,
        bases,
        previous: None,
        remaining: original,
        head: None,
    };
    groups.head = groups.next()?;
    for change in changes {
        groups.replace(change, &mut out)?;
    }
    groups.copy_before(bases.len(), &mut out)?;
    Ok(out)
}
