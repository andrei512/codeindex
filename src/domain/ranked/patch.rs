//! Sparse same-path replacements for grouped ranked postings. Addresses in
//! this plan are relative to the prior ranked product.
use super::storage::{FOOTER, get, invalid, put, row};
use super::{ChunkRange, RankedIndex, Term, cursor::Directory};
use std::{
    collections::{BTreeMap, HashMap},
    io::{self, Write},
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
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        self.borrow(at..at + prefix as u64);
        self.write(&new[prefix..new.len() - suffix]);
        self.borrow(at + (old.len() - suffix) as u64..at + old.len() as u64);
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
    new: Term,
}

fn contributions(index: RankedIndex) -> HashMap<Vec<u8>, Term> {
    let mut terms = index.terms;
    index
        .dictionary
        .into_iter()
        .map(|(k, i)| (k, std::mem::take(&mut terms[i])))
        .collect()
}
fn record_file(index: &RankedIndex) -> io::Result<Vec<u8>> {
    let f = &index.files[0];
    let mut out = Vec::new();
    put(&mut out, f.path.len())?;
    out.write_all(f.path.as_bytes())?;
    put(&mut out, f.spans.len())?;
    let mut prev = 0;
    for s in &f.spans {
        put(&mut out, s.start - prev)?;
        put(&mut out, s.len())?;
        prev = s.end
    }
    Ok(out)
}
fn record_rows(index: &RankedIndex, id: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut prev = 0;
    for r in &index.rows {
        for v in [
            id,
            r.chunk.first_line,
            r.chunk.last_line - r.chunk.first_line,
            r.chunk.bytes.start,
            r.chunk.bytes.len(),
            r.span_start,
            r.body_start - prev,
            r.parts_start - r.body_start,
            r.end - r.parts_start,
        ] {
            put(&mut out, v)?;
        }
        prev = r.end;
    }
    Ok(out)
}
struct Replacement {
    file: Vec<u8>,
    rows: Vec<u8>,
    chunk_count: usize,
    tokens: usize,
}
struct Group<'a> {
    file: usize,
    count: usize,
    raw: &'a [u8],
}
fn replace_term(
    key: &[u8],
    old: &[u8],
    changes: Vec<Change>,
    bases: &[usize],
) -> io::Result<Vec<u8>> {
    let mut data = old;
    let (mut df, mut total) = if old.is_empty() {
        (0, 0)
    } else {
        let n = get(&mut data)?;
        if n != key.len() || !data.starts_with(key) {
            return Err(invalid());
        }
        data = &data[n..];
        (get(&mut data)?, get(&mut data)?)
    };
    let mut groups = Vec::new();
    let mut consumed = 0usize;
    while !data.is_empty() {
        let original = data;
        let file = get(&mut data)?;
        let count = get(&mut data)?;
        if count == 0
            || file >= bases.len() - 1
            || groups.last().is_some_and(|g: &Group| g.file >= file)
        {
            return Err(invalid());
        }
        let mut last = 0usize;
        for i in 0..count {
            let delta = get(&mut data)?;
            last = last.checked_add(delta).ok_or_else(invalid)?;
            if last >= bases[file + 1] - bases[file] || i > 0 && delta == 0 {
                return Err(invalid());
            }
        }
        groups.push(Group {
            file,
            count,
            raw: &original[..original.len() - data.len()],
        });
        consumed = consumed.checked_add(count).ok_or_else(invalid)?;
    }
    if consumed != total {
        return Err(invalid());
    }
    let mut groups = groups.into_iter().peekable();
    let mut body = Vec::new();
    for change in changes {
        while let Some(g) = groups.next_if(|g| g.file < change.file) {
            body.extend_from_slice(g.raw)
        }
        if let Some(g) = groups.next_if(|g| g.file == change.file) {
            total = total.checked_sub(g.count).ok_or_else(invalid)?;
        } else if change.old_df != 0 {
            return Err(invalid());
        }
        df = df.checked_sub(change.old_df).ok_or_else(invalid)? + change.new.document_frequency;
        total += change.new.positions.len();
        if !change.new.positions.is_empty() {
            put(&mut body, change.file)?;
            put(&mut body, change.new.positions.len())?;
            let mut last = 0;
            for pos in change.new.positions {
                put(&mut body, pos - last)?;
                last = pos
            }
        }
    }
    for g in groups {
        body.extend_from_slice(g.raw)
    }
    if total == 0 {
        if df != 0 {
            return Err(invalid());
        }
        return Ok(Vec::new());
    }
    if df == 0 || df > total {
        return Err(invalid());
    }
    let mut out = Vec::new();
    put(&mut out, key.len())?;
    out.extend_from_slice(key);
    put(&mut out, df)?;
    put(&mut out, total)?;
    out.extend(body);
    Ok(out)
}
struct Record {
    old_slot: usize,
    existed: bool,
    old: Vec<u8>,
    new: Vec<u8>,
}

pub(crate) fn patch(
    len: u64,
    mut read: impl FnMut(Range<u64>) -> io::Result<Vec<u8>>,
    edits: &[Edit<'_>],
) -> io::Result<Option<Patch>> {
    if read(0..8)? != b"CIRANK04" {
        return Ok(None);
    }
    let footer = len.checked_sub(FOOTER).ok_or_else(invalid)?;
    let tail = read(footer..len)?;
    let dir = Directory::decode(tail.as_slice().try_into().map_err(|_| invalid())?)?;
    if !dir.tree || dir.rows_end >= dir.offset || dir.offset > footer {
        return Err(invalid());
    }
    let raw_dir = read(dir.offset..footer)?;
    let (mut trees, [files, terms]) = super::directory::load(
        &raw_dir,
        dir.offset,
        [
            8 + super::directory::var_len(dir.files),
            dir.rows_end + super::directory::var_len(dir.terms),
        ],
        [dir.files, dir.terms],
    )?;
    let f = |i: usize| files[i];
    let t = |i: usize| terms[i];
    if f(dir.files) > dir.rows_end || t(dir.terms) != dir.offset {
        return Err(invalid());
    }
    let rows_at = f(dir.files);
    let raw_rows = read(rows_at..dir.rows_end)?;
    let mut row_data = raw_rows.as_slice();
    let nrows = get(&mut row_data)?;
    if nrows != dir.rows {
        return Err(invalid());
    }
    let mut bases = vec![0usize; dir.files + 1];
    let mut row_count = vec![0usize; dir.files];
    let mut row_ranges = vec![0u64; dir.files + 1];
    let before = |data: &[u8]| dir.rows_end - data.len() as u64;
    let mut last = 0;
    let mut next_file = 0;
    let mut by_file = edits
        .iter()
        .map(|e| (e.file, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for _ in 0..nrows {
        let at = before(row_data);
        let r = row(&mut row_data, last, dir.files)?;
        if r.file + 1 < next_file {
            return Err(invalid());
        }
        while next_file <= r.file {
            row_ranges[next_file] = at;
            next_file += 1
        }
        row_count[r.file] += 1;
        bases[r.file + 1] = r.end;
        if let Some(chunks) = by_file.get_mut(&r.file) {
            chunks.push(r.chunk);
        }
        last = r.end;
    }
    row_ranges[next_file..].fill(before(row_data));
    for i in 0..dir.files {
        if row_count[i] == 0 {
            bases[i + 1] = bases[i]
        }
    }
    if !row_data.is_empty() || dir.tokens != last {
        return Err(invalid());
    }
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
                file: record_file(&new)?,
                rows: record_rows(&new, edit.file)?,
                chunk_count: new.rows.len(),
                tokens: new.tokens,
            },
        );
        let mut previous = contributions(old);
        for (key, new) in contributions(new) {
            let old = previous.remove(&key).unwrap_or_default();
            if old.document_frequency != new.document_frequency || old.positions != new.positions {
                changes.entry(key).or_default().push(Change {
                    file: edit.file,
                    old_df: old.document_frequency,
                    new,
                });
            }
        }
        for (key, old) in previous {
            changes.entry(key).or_default().push(Change {
                file: edit.file,
                old_df: old.document_frequency,
                new: Term::default(),
            })
        }
    }
    // Cache dictionary probes across the sorted set of changed keys. Misses
    // never require decoding any unrelated term's positions.
    let mut key_cache = HashMap::new();
    let mut changed_terms = Vec::new();
    let mut lower = 0;
    for (key, owners) in changes {
        let mut lo = lower;
        let mut hi = dir.terms;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if let std::collections::hash_map::Entry::Vacant(e) = key_cache.entry(mid) {
                let range = t(mid)..t(mid + 1);
                let prefix = read(range.start..range.end.min(range.start + 10))?;
                let mut d = prefix.as_slice();
                let size = get(&mut d)?;
                if size == 0 || size > 32768 {
                    return Err(invalid());
                }
                let start = range.start + (prefix.len() - d.len()) as u64;
                let end = start
                    .checked_add(size as u64)
                    .filter(|&end| end <= range.end)
                    .ok_or_else(invalid)?;
                e.insert(read(start..end)?);
            }
            if key_cache[&mid] < key {
                lo = mid + 1
            } else {
                hi = mid
            }
        }
        let exists = lo < dir.terms && key_cache.get(&lo) == Some(&key);
        lower = lo;
        let old = if exists {
            read(t(lo)..t(lo + 1))?
        } else {
            vec![]
        };
        let new = replace_term(&key, &old, owners, &bases)?;
        changed_terms.push(Record {
            old_slot: lo,
            existed: exists,
            old,
            new,
        });
    }
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
    let mut last = 0;
    for (&file, new) in &replacements {
        patch.borrow(row_ranges[last]..row_ranges[file]);
        let r = row_ranges[file]..row_ranges[file + 1];
        patch.replace(
            r.start,
            &raw_rows[(r.start - rows_at) as usize..(r.end - rows_at) as usize],
            &new.rows,
        );
        last = file + 1;
    }
    patch.borrow(row_ranges[last]..dir.rows_end);
    let rows_end = patch.offset;
    let nterms = dir.terms
        + changed_terms
            .iter()
            .filter(|c| !c.existed && !c.new.is_empty())
            .count()
        - changed_terms
            .iter()
            .filter(|c| c.existed && c.new.is_empty())
            .count();
    patch.number(nterms)?;
    let mut slot = 0;
    let mut new_slot = 0;
    for edit in changed_terms {
        patch.borrow(t(slot)..t(edit.old_slot));
        new_slot += edit.old_slot - slot;
        slot = edit.old_slot;
        let op = match (edit.existed, edit.new.is_empty()) {
            (true, false) => super::directory::Edit::Set(edit.new.len() as u64),
            (true, true) => super::directory::Edit::Remove,
            (false, false) => super::directory::Edit::Insert(edit.new.len() as u64),
            (false, true) => continue,
        };
        trees[1].edit(new_slot, op)?;
        if !edit.new.is_empty() {
            patch.replace(t(edit.old_slot), &edit.old, &edit.new);
            new_slot += 1;
        }
        if edit.existed {
            slot += 1;
        }
    }
    patch.borrow(t(slot)..t(dir.terms));
    let at = patch.offset;
    super::directory::append(&trees, &mut patch);
    patch.write(b"RANKDIR4");
    for value in [
        dir.files as u64,
        nterms as u64,
        at,
        rows_end,
        tokens as u64,
        visible_rows as u64,
    ] {
        patch.write(&value.to_le_bytes())
    }
    Ok(Some(patch))
}
