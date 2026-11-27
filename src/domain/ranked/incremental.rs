//! Rebase retained file and row coordinates into a new, path-ordered generation.
//! Only changed documents run through the tokenizer.
use super::{RankedIndex, Row, storage::invalid};
use std::{io, ops::Range};

struct Translation {
    old: Range<usize>,
    new_start: usize,
}

fn translate_term(
    term: &mut super::Term,
    translations: &[Translation],
    removed_row_ends: &[usize],
) {
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
}

#[derive(Default)]
struct Rebase {
    previous_end: usize,
    translations: Vec<Translation>,
    removed_ends: Vec<usize>,
}

impl Rebase {
    fn consume_file(
        &mut self,
        mut rows: impl Iterator<Item = Row>,
        output: &mut RankedIndex,
        old_file: usize,
        new_file: usize,
    ) {
        // The caller bounds this iterator to the prefix through old_file.
        let first = rows.find(|row| {
            if row.file == old_file {
                return true;
            }
            self.removed_ends.push(row.end);
            self.previous_end = row.end;
            false
        });
        let Some(first) = first else { return };
        let old_start = self.previous_end;
        let new_start = output.tokens;
        for mut row in std::iter::once(first).chain(rows) {
            self.previous_end = row.end;
            row.file = new_file;
            row.body_start = new_start + (row.body_start - old_start);
            row.parts_start = new_start + (row.parts_start - old_start);
            row.end = new_start + (row.end - old_start);
            output.tokens = row.end;
            output.rows.push(row);
        }
        self.add_translation(old_start, new_start);
    }

    fn add_translation(&mut self, old_start: usize, new_start: usize) {
        if self.previous_end <= old_start {
            return;
        }
        let adjacent = |last: &&mut Translation| {
            last.old.end == old_start && last.new_start + last.old.len() == new_start
        };
        if let Some(last) = self.translations.last_mut().filter(adjacent) {
            last.old.end = self.previous_end;
        } else {
            self.translations.push(Translation { old: old_start..self.previous_end, new_start });
        }
    }

    fn merge_term(&self, output: &mut RankedIndex, key: Vec<u8>, mut term: super::Term) {
        use std::collections::hash_map::Entry;
        translate_term(&mut term, &self.translations, &self.removed_ends);
        if term.positions.is_empty() {
            return;
        }
        match output.dictionary.entry(key) {
            Entry::Occupied(entry) => {
                let changed = &mut output.terms[*entry.get()];
                term.document_frequency += changed.document_frequency;
                term.positions.append(&mut changed.positions);
                // The stable adaptive sort merges the already ordered runs.
                term.positions.sort();
                *changed = term;
            }
            Entry::Vacant(entry) => {
                entry.insert(output.terms.len());
                output.terms.push(term);
            }
        }
    }
}

impl RankedIndex {
    pub(crate) fn refresh(
        mut prior: Self,
        reuse: &[Option<usize>],
        mut add_changed: impl FnMut(&mut Self, usize),
    ) -> io::Result<Self> {
        let mut output = Self::default();
        let mut files = prior.files.into_iter().enumerate();
        let mut rows = prior.rows.into_iter().peekable();
        let mut rebase = Rebase::default();
        for (new_id, &old_id) in reuse.iter().enumerate() {
            let Some(old_id) = old_id else {
                add_changed(&mut output, new_id);
                continue;
            };
            let (_, file) = files.find(|(id, _)| *id >= old_id)
                .filter(|(id, _)| *id == old_id).ok_or_else(invalid)?;
            output.files.push(file);
            let owned_prefix = std::iter::from_fn(|| rows.next_if(|row| row.file <= old_id));
            rebase.consume_file(owned_prefix, &mut output, old_id, new_id);
        }
        rebase.removed_ends.extend(rows.map(|row| row.end));
        for (key, id) in prior.dictionary {
            let term = std::mem::take(&mut prior.terms[id]);
            rebase.merge_term(&mut output, key, term);
        }
        Ok(output)
    }
}
