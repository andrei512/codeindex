//! Rebase retained file and row coordinates into a new, path-ordered generation.
//! Only changed documents run through the tokenizer.
use super::{RankedIndex, Row, storage::invalid};
use std::{io, ops::Range};

struct Translation {
    old: Range<usize>,
    new_start: usize,
}

impl RankedIndex {
    pub(crate) fn refresh(
        mut prior: Self,
        reuse: &[Option<usize>],
        mut add_changed: impl FnMut(&mut Self, usize),
    ) -> io::Result<Self> {
        let mut output = Self::default();
        let mut files = prior.files.into_iter().enumerate().peekable();
        let mut rows = prior.rows.into_iter().peekable();
        let mut previous_end = 0;
        let mut translations: Vec<Translation> = Vec::new();
        let mut removed_row_ends = Vec::new();
        for (new_id, &old_id) in reuse.iter().enumerate() {
            let Some(old_id) = old_id else {
                add_changed(&mut output, new_id);
                continue;
            };
            while files.next_if(|(id, _)| *id < old_id).is_some() {}
            let (_, file) = files.next_if(|(id, _)| *id == old_id).ok_or_else(invalid)?;
            output.files.push(file);
            while let Some(row) = rows.next_if(|r| r.file < old_id) {
                removed_row_ends.push(row.end);
                previous_end = row.end;
            }
            let old_start = previous_end;
            let new_start = output.tokens;
            while let Some(mut row) = rows.next_if(|r| r.file == old_id) {
                previous_end = row.end;
                row.file = new_id;
                row.body_start = new_start + (row.body_start - old_start);
                row.parts_start = new_start + (row.parts_start - old_start);
                row.end = new_start + (row.end - old_start);
                output.tokens = row.end;
                output.rows.push(row);
            }
            if previous_end > old_start {
                if let Some(last) = translations.last_mut()
                    && last.old.end == old_start
                    && last.new_start + last.old.len() == new_start
                {
                    last.old.end = previous_end;
                } else {
                    translations.push(Translation {
                        old: old_start..previous_end,
                        new_start,
                    });
                }
            }
        }
        removed_row_ends.extend(rows.map(|r: Row| r.end));
        for (key, id) in prior.dictionary {
            let mut term = std::mem::take(&mut prior.terms[id]);
            let mut i = 0;
            let mut previous_removed_row = None;
            term.positions.retain_mut(|pos| {
                if translations.get(i).is_some_and(|t| *pos >= t.old.end) {
                    i += translations[i..].partition_point(|t| t.old.end <= *pos);
                }
                if let Some(t) = translations.get(i)
                    && t.old.start <= *pos
                {
                    *pos = t.new_start + (*pos - t.old.start);
                    true
                } else {
                    let row = removed_row_ends.partition_point(|&end| end <= *pos);
                    if previous_removed_row.replace(row) != Some(row) {
                        term.document_frequency -= 1;
                    }
                    false
                }
            });
            if term.positions.is_empty() {
                continue;
            }
            if let Some(&slot) = output.dictionary.get(&key) {
                let changed = &mut output.terms[slot];
                term.document_frequency += changed.document_frequency;
                term.positions.append(&mut changed.positions);
                // Both lists are already sorted; the stable adaptive sort merges their runs.
                term.positions.sort();
                *changed = term;
            } else {
                output.dictionary.insert(key, output.terms.len());
                output.terms.push(term);
            }
        }
        Ok(output)
    }
}
