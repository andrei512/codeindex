//! A term's positions within one file survive length changes in earlier files.
use super::{
    RankedIndex, Term,
    storage::{get, invalid, put},
};
use std::io::{self, Read, Write};

/// Row records are sorted by file. Fill boundaries for skipped files from
/// the preceding token end, including empty files after the final row.
pub(super) struct FileBases {
    values: Vec<usize>,
    count: usize,
}
impl FileBases {
    pub fn new(files: usize) -> io::Result<Self> {
        let count = files.checked_add(1).ok_or_else(invalid)?;
        Ok(Self {
            values: Vec::with_capacity(count),
            count,
        })
    }
    pub fn observe(&mut self, file: usize, previous_end: usize) {
        if self.values.len() <= file {
            self.values.resize(file + 1, previous_end);
        }
    }
    pub fn finish(mut self, tokens: usize) -> Vec<usize> {
        self.values.resize(self.count, tokens);
        self.values
    }
}

impl RankedIndex {
    pub(super) fn file_bases(&self) -> io::Result<Vec<usize>> {
        let mut bases = Vec::with_capacity(self.files.len() + 1);
        let mut rows = self.rows.iter().peekable();
        let mut pos = 0;
        for id in 0..self.files.len() {
            bases.push(pos);
            while let Some(r) = rows.next_if(|r| r.file == id) {
                pos = r.end;
            }
        }
        if rows.next().is_some() || pos != self.tokens {
            return Err(invalid());
        }
        bases.push(pos);
        Ok(bases)
    }
}
/// A sampled owner directory avoids searching the full file table for each
/// posting group during bulk encoding. One hint serves 256 token positions.
pub(super) fn file_hints(tokens: usize, bases: &[usize]) -> Vec<usize> {
    let mut hints = Vec::with_capacity(tokens.div_ceil(256));
    let mut file = 0;
    for position in (0..tokens).step_by(256) {
        while bases[file + 1] <= position {
            file += 1;
        }
        hints.push(file);
    }
    hints
}
pub(super) fn write_term(
    out: &mut impl Write,
    t: &Term,
    bases: &[usize],
    hints: &[usize],
) -> io::Result<()> {
    put(out, t.document_frequency)?;
    put(out, t.positions.len())?;
    let mut positions = t.positions.as_slice();
    while let Some(&first) = positions.first() {
        let (file, end) = posting_owner(first, hints, bases)?;
        let n = positions.iter().take_while(|&&p| p < end).count();
        put(out, file)?;
        put(out, n)?;
        let mut last = bases[file];
        for &p in &positions[..n] {
            put(out, p - last)?;
            last = p
        }
        positions = &positions[n..];
    }
    Ok(())
}
fn posting_owner(position: usize, hints: &[usize], bases: &[usize]) -> io::Result<(usize, usize)> {
    let mut file = *hints.get(position / 256).ok_or_else(invalid)?;
    while bases.get(file + 1).is_some_and(|&end| end <= position) {
        file += 1;
    }
    Ok((file, *bases.get(file + 1).ok_or_else(invalid)?))
}

pub(super) fn read_term(input: &mut impl Read, rows: usize, bases: &[usize]) -> io::Result<Term> {
    let (df, count) = super::storage::term_counts(input, rows, *bases.last().ok_or_else(invalid)?)?;
    let mut positions = Vec::new();
    let mut previous = None;
    while positions.len() < count {
        let file = get(input)?;
        let n = get(input)?;
        if n == 0
            || n > count - positions.len()
            || file >= bases.len() - 1
            || previous.is_some_and(|old| file <= old)
        {
            return Err(invalid());
        }
        super::storage::read_positions(input, bases[file]..bases[file + 1], n, &mut positions)?;
        previous = Some(file);
    }
    Ok(Term {
        positions,
        document_frequency: df,
    })
}
