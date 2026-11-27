//! Sparse same-path replacements for grouped ranked postings. Addresses in
//! this plan are relative to the prior ranked product.
mod postings;
use super::storage::{FOOTER, get, invalid, put};
use super::{ChunkRange, RankedIndex, Term, cursor::Directory};
use postings::replace_term;
use std::{
    collections::{BTreeMap, HashMap},
    io,
    ops::Range,
};

pub(crate) enum Piece {
    Borrow(Range<u64>),
    Bytes(Vec<u8>),
}
#[derive(Default)]
pub(crate) struct Patch {
    pub pieces: Vec<Piece>,
    offset: u64,
}
impl Patch {
    pub(super) fn borrow(&mut self, r: Range<u64>) {
        if r.is_empty() {
            return;
        }
        self.offset += r.end - r.start;
        if let Some(Piece::Borrow(prev)) = self.pieces.last_mut()
            && prev.end == r.start
        {
            prev.end = r.end
        } else {
            self.pieces.push(Piece::Borrow(r))
        }
    }
    pub(super) fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.offset += bytes.len() as u64;
        if let Some(Piece::Bytes(prev)) = self.pieces.last_mut() {
            prev.extend_from_slice(bytes)
        } else {
            self.pieces.push(Piece::Bytes(bytes.to_vec()))
        }
    }
    fn replace(&mut self, at: u64, old: &[u8], new: &[u8]) {
        let (prefix, suffix) = crate::domain::matching_edges(old, new);
        self.borrow(at..at + prefix as u64);
        self.write(&new[prefix..new.len() - suffix]);
        self.borrow(at + (old.len() - suffix) as u64..at + old.len() as u64);
    }
    fn replace_rows(
        &mut self,
        replacements: &BTreeMap<usize, Replacement>,
        row_ranges: &[u64],
        raw_rows: &[u8],
        rows_at: u64,
    ) {
        let mut last = 0;
        for (&file, new) in replacements {
            self.borrow(row_ranges[last]..row_ranges[file]);
            let r = row_ranges[file]..row_ranges[file + 1];
            self.replace(
                r.start,
                &raw_rows[(r.start - rows_at) as usize..(r.end - rows_at) as usize],
                &new.rows,
            );
            last = file + 1;
        }
        self.borrow(row_ranges[last]..*row_ranges.last().unwrap());
    }
    fn write_terms(
        &mut self,
        changed_terms: Vec<Record>,
        terms: &[u64],
        tree: &mut super::directory::Node,
    ) -> io::Result<usize> {
        let t = |slot: usize| terms[slot];
        let nterms = changed_terms.iter().fold(terms.len() - 1, |count, record| {
            record.action.count(count)
        });
        self.number(nterms)?;
        let mut slot = 0;
        let mut new_slot = 0;
        for edit in changed_terms {
            self.borrow(t(slot)..t(edit.old_slot));
            new_slot += edit.old_slot - slot;
            slot = edit.old_slot;
            let Some(operation) = edit.action.directory_edit() else { continue };
            tree.edit(new_slot, operation)?;
            new_slot += edit.write(self, t(edit.old_slot));
            slot += usize::from(edit.action.consumes());
        }
        self.borrow(t(slot)..t(terms.len() - 1));
        Ok(nterms)
    }

    fn number(&mut self, n: usize) -> io::Result<()> {
        let mut buf = Vec::new();
        put(&mut buf, n)?;
        self.write(&buf);
        Ok(())
    }
}
pub(crate) struct Edit<'a> {
    pub file: usize,
    pub path: &'a str,
    pub old: &'a str,
    pub new: &'a str,
    pub chunks: Vec<ChunkRange>,
}
struct Change {
    file: usize,
    old_df: usize,
    old_count: usize,
    new: Term,
}

/// Consume the existing term dictionaries. Unchanged positions drop with the
/// edited file, and only changed postings move into the patch plan.
fn record_changes(
    file: usize,
    mut old: RankedIndex,
    mut new: RankedIndex,
    changes: &mut BTreeMap<Vec<u8>, Vec<Change>>,
) {
    let empty = Term::default();
    for (key, id) in new.dictionary {
        let previous = old
            .dictionary
            .remove(&key)
            .map_or(&empty, |i| &old.terms[i]);
        let current = &mut new.terms[id];
        if previous.document_frequency == current.document_frequency
            && previous.positions == current.positions
        {
            continue;
        }
        changes.entry(key).or_default().push(Change {
            file,
            old_df: previous.document_frequency,
            old_count: previous.positions.len(),
            new: std::mem::take(current),
        });
    }
    for (key, id) in old.dictionary {
        changes.entry(key).or_default().push(Change {
            file,
            old_df: old.terms[id].document_frequency,
            old_count: old.terms[id].positions.len(),
            new: Term::default(),
        });
    }
}

fn record_file(index: &RankedIndex) -> Vec<u8> {
    let mut out = Vec::new();
    super::bulk::encode_file(&index.files[0], &mut out);
    out
}
fn record_rows(index: &RankedIndex, id: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    index.write_rows(&mut out, |_| id)?;
    Ok(out)
}
struct Replacement {
    file: Vec<u8>,
    rows: Vec<u8>,
    chunk_count: usize,
    tokens: usize,
}
type Changes = BTreeMap<Vec<u8>, Vec<Change>>;
impl Replacement {
    fn prepare(
        edits: &[Edit<'_>],
        mut by_file: BTreeMap<usize, Vec<ChunkRange>>,
    ) -> io::Result<(BTreeMap<usize, Self>, Changes)> {
        let mut replacements = BTreeMap::new();
        let mut changes = BTreeMap::<Vec<u8>, Vec<Change>>::new();
        for edit in edits {
            let old_chunks = by_file.remove(&edit.file).ok_or_else(invalid)?;
            let mut old = RankedIndex::default();
            old.add_file(edit.path, edit.old, &old_chunks);
            let mut new = RankedIndex::default();
            new.add_file(edit.path, edit.new, &edit.chunks);
            replacements.insert(
                edit.file,
                Replacement {
                    file: record_file(&new),
                    rows: record_rows(&new, edit.file)?,
                    chunk_count: new.rows.len(),
                    tokens: new.tokens,
                },
            );
            record_changes(edit.file, old, new, &mut changes);
        }
        Ok((replacements, changes))
    }
}
enum TermAction {
    Set(u64),
    Remove,
    Insert(u64),
    Empty,
}

impl TermAction {
    fn classify(exists: bool, new: &[u8]) -> Self {
        match (exists, new.is_empty()) {
            (true, false) => Self::Set(new.len() as u64),
            (true, true) => Self::Remove,
            (false, false) => Self::Insert(new.len() as u64),
            (false, true) => Self::Empty,
        }
    }

    fn count(&self, count: usize) -> usize {
        count + usize::from(self.writes()) - usize::from(self.consumes())
    }

    fn writes(&self) -> bool {
        matches!(self, Self::Set(_) | Self::Insert(_))
    }

    fn consumes(&self) -> bool {
        matches!(self, Self::Set(_) | Self::Remove)
    }

    fn directory_edit(&self) -> Option<super::directory::Edit> {
        use super::directory::Edit;
        match *self {
            Self::Set(len) => Some(Edit::Set(len)),
            Self::Remove => Some(Edit::Remove),
            Self::Insert(len) => Some(Edit::Insert(len)),
            Self::Empty => None,
        }
    }
}

struct Record {
    old_slot: usize,
    action: TermAction,
    old: Vec<u8>,
    new: Vec<u8>,
}

impl Record {
    fn write(&self, patch: &mut Patch, offset: u64) -> usize {
        if !self.action.writes() {
            return 0;
        }
        patch.replace(offset, &self.old, &self.new);
        1
    }
}

struct TermLookup<'a, R> {
    terms: &'a [u64],
    read: R,
    keys: HashMap<usize, Vec<u8>>,
    lower: usize,
}

impl<R: FnMut(Range<u64>) -> io::Result<Vec<u8>>> TermLookup<'_, R> {
    fn key(&mut self, slot: usize) -> io::Result<&[u8]> {
        use std::collections::hash_map::Entry;
        let key = match self.keys.entry(slot) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let range = self.terms[slot]..self.terms[slot + 1];
                let length = (range.end - range.start).min(super::term_key::PROBE as u64);
                let bytes = (self.read)(range.start..range.start + length)?;
                let probe = super::term_key::Key::parse(range, &bytes)?;
                entry.insert(probe.owned(&mut self.read)?)
            }
        };
        Ok(key)
    }

    fn record(&mut self, slot: usize, exists: bool) -> io::Result<Vec<u8>> {
        if !exists {
            return Ok(Vec::new());
        }
        (self.read)(self.terms[slot]..self.terms[slot + 1])
    }

    fn locate(&mut self, key: &[u8]) -> io::Result<(usize, bool)> {
        let slots = self.lower as u64..(self.terms.len() - 1) as u64;
        let lo = crate::domain::lower_bound(slots, |mid| Ok(self.key(mid as usize)? < key))? as usize;
        self.lower = lo;
        Ok((lo, self.keys.get(&lo).is_some_and(|known| known == key)))
    }
}

fn changed_records(
    terms: &[u64],
    changes: BTreeMap<Vec<u8>, Vec<Change>>,
    bases: &[usize],
    read: impl FnMut(Range<u64>) -> io::Result<Vec<u8>>,
) -> io::Result<Vec<Record>> {
    // Sorted edits share dictionary probes. Misses omit unrelated postings.
    let mut lookup = TermLookup { terms, read, keys: HashMap::new(), lower: 0 };
    let mut changed_terms = Vec::new();
    for (key, owners) in changes {
        let (slot, exists) = lookup.locate(&key)?;
        let old = lookup.record(slot, exists)?;
        let new = replace_term(&key, &old, owners, bases)?;
        changed_terms.push(Record { old_slot: slot, action: TermAction::classify(exists, &new), old, new });
    }
    Ok(changed_terms)
}

struct PriorRows {
    bases: Vec<usize>,
    row_count: Vec<usize>,
    row_ranges: Vec<u64>,
    by_file: BTreeMap<usize, Vec<ChunkRange>>,
}
impl PriorRows {
    fn read(raw_rows: &[u8], dir: &Directory, edits: &[Edit<'_>]) -> io::Result<Self> {
        let block = dir.row_block(raw_rows)?;
        let mut bases = super::grouped::FileBases::new(dir.files)?;
        let mut row_count = vec![0usize; dir.files];
        let mut row_ranges = vec![0u64; dir.files + 1];
        let mut next_file = 0;
        let mut by_file = edits
            .iter()
            .map(|e| (e.file, Vec::new()))
            .collect::<BTreeMap<_, _>>();
        block.visit(|at, start, r| {
            while next_file <= r.file {
                row_ranges[next_file] = at;
                next_file += 1
            }
            row_count[r.file] += 1;
            bases.observe(r.file, start);
            if let Some(chunks) = by_file.get_mut(&r.file) {
                chunks.push(r.chunk);
            }
        })?;
        row_ranges[next_file..].fill(dir.rows_end);
        let bases = bases.finish(dir.tokens);
        Ok(Self {
            bases,
            row_count,
            row_ranges,
            by_file,
        })
    }
}

fn patch_footer(len: u64, read: &mut impl FnMut(Range<u64>) -> io::Result<Vec<u8>>) -> io::Result<(Directory, u64)> {
    let footer = len.checked_sub(FOOTER).ok_or_else(invalid)?;
    let tail = read(footer..len)?;
    let dir = Directory::decode(tail.as_slice().try_into().map_err(|_| invalid())?)?;
    if dir.format != super::storage::Format::Tree || !(dir.rows_end < dir.offset && dir.offset <= footer) {
        return Err(invalid());
    }
    Ok((dir, footer))
}

pub(crate) fn patch(
    len: u64,
    mut read: impl FnMut(Range<u64>) -> io::Result<Vec<u8>>,
    edits: &[Edit<'_>],
) -> io::Result<Option<Patch>> {
    if read(0..8)? != b"CIRANK04" {
        return Ok(None);
    }
    let (dir, footer) = patch_footer(len, &mut read)?;
    let raw_dir = read(dir.offset..footer)?;
    let (mut trees, [files, terms]) = dir.load_trees(&raw_dir, dir.offset)?;
    let f = |i: usize| files[i];
    let rows_at = f(dir.files);
    let raw_rows = read(rows_at..dir.rows_end)?;
    let PriorRows {
        bases,
        row_count,
        row_ranges,
        by_file,
    } = PriorRows::read(&raw_rows, &dir, edits)?;
    let (replacements, changes) = Replacement::prepare(edits, by_file)?;
    let changed_terms = changed_records(&terms, changes, &bases, &mut read)?;
    let mut patch = Patch::default();
    patch.borrow(0..f(0));
    let mut tokens = dir.tokens;
    let mut visible_rows = dir.rows;
    let mut last = 0;
    for (&file, r) in &replacements {
        patch.borrow(f(last)..f(file));
        let range = f(file)..f(file + 1);
        trees[0].edit(file, super::directory::Edit::Set(r.file.len() as u64))?;
        patch.replace(range.start, &read(range.clone())?, &r.file);
        tokens = tokens
            .checked_sub(bases[file + 1] - bases[file])
            .and_then(|n| n.checked_add(r.tokens))
            .ok_or_else(invalid)?;
        visible_rows = visible_rows
            .checked_sub(row_count[file])
            .and_then(|n| n.checked_add(r.chunk_count))
            .ok_or_else(invalid)?;
        last = file + 1;
    }
    patch.borrow(f(last)..f(dir.files));
    patch.number(visible_rows)?;
    patch.replace_rows(&replacements, &row_ranges, &raw_rows, rows_at);
    let rows_end = patch.offset;
    let nterms = patch.write_terms(changed_terms, &terms, &mut trees[1])?;
    let at = patch.offset;
    super::directory::append(&trees, &mut patch);
    patch.write(
        &super::cursor::Directory {
            format: dir.format,
            files: dir.files,
            terms: nterms,
            offset: at,
            rows_end,
            tokens,
            rows: visible_rows,
        }
        .tree_footer(),
    );
    Ok(Some(patch))
}
