//! Sorted term records have compact delta postings and a fixed-width offset directory.
//! A miss probes keys only. A hit reads its lists, the chunk directory and retained file spans.
use super::{
    storage::{self, FOOTER, invalid},
    *,
};
use std::{collections::HashSet, io};
pub(super) struct Directory {
    pub tree: bool,
    pub files: usize,
    pub terms: usize,
    pub offset: u64,
    pub(super) rows_end: u64,
    pub(super) grouped: bool,
    pub tokens: usize,
    pub rows: usize,
}
impl Directory {
    pub(super) fn decode(tail: &[u8; FOOTER as usize]) -> io::Result<Self> {
        if &tail[..8] != b"RANKDIR2" && &tail[..8] != b"RANKDIR3" && &tail[..8] != b"RANKDIR4" {
            return Err(invalid());
        }
        let n = |p: usize| u64::from_le_bytes(tail[p..p + 8].try_into().unwrap());
        let count = |p| usize::try_from(n(p)).map_err(|_| invalid());
        Ok(Self {
            tree: &tail[..8] == b"RANKDIR4",
            grouped: &tail[..8] != b"RANKDIR2",
            files: count(8)?,
            terms: count(16)?,
            offset: n(24),
            rows_end: n(32),
            tokens: count(40)?,
            rows: count(48)?,
        })
    }
}
type ReadAt<'a> = dyn FnMut(u64, &mut [u8]) -> io::Result<()> + 'a;
struct Reader<'a> {
    read: &'a mut ReadAt<'a>,
    dir: Directory,
    terms_at: u64,
    bases_at: u64,
    bases: Option<Vec<usize>>,
    trees: Option<[Range<u64>; 2]>,
    nodes: directory::Cursor,
    cached_rows: Option<Vec<Row>>,
}
impl Reader<'_> {
    fn position(&mut self, base: u64, slot: usize) -> io::Result<u64> {
        if self.dir.tree {
            return Ok(self.range(base, slot, self.dir.offset)?.start);
        }
        let mut b = [0; 8];
        (self.read)(base + (slot as u64) * 8, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn range(&mut self, base: u64, slot: usize, limit: u64) -> io::Result<Range<u64>> {
        if let Some(trees) = &self.trees {
            let file = base == self.dir.offset;
            let (root, first, count) = if file {
                (
                    &trees[0],
                    8 + directory::var_len(self.dir.files),
                    self.dir.files,
                )
            } else {
                (
                    &trees[1],
                    self.dir
                        .rows_end
                        .checked_add(directory::var_len(self.dir.terms))
                        .ok_or_else(invalid)?,
                    self.dir.terms,
                )
            };
            let range = self
                .nodes
                .range(self.read, root.clone(), slot, count, first)?;
            if range.end > limit {
                return Err(invalid());
            }
            return Ok(range);
        }
        let mut b = [0; 16];
        (self.read)(base + (slot as u64) * 8, &mut b)?;
        let lo = u64::from_le_bytes(b[..8].try_into().unwrap());
        let hi = u64::from_le_bytes(b[8..].try_into().unwrap());
        if lo < 8 || lo > hi || hi > limit {
            return Err(invalid());
        }
        Ok(lo..hi)
    }
    fn fetch(&mut self, r: Range<u64>) -> io::Result<Vec<u8>> {
        let n = r.end.checked_sub(r.start).ok_or_else(invalid)?;
        let n = usize::try_from(n).map_err(|_| invalid())?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(n).map_err(io::Error::other)?;
        buf.resize(n, 0);
        (self.read)(r.start, &mut buf)?;
        Ok(buf)
    }
    fn load_rows(&mut self) -> io::Result<()> {
        if self.cached_rows.is_some() {
            return Ok(());
        }
        let rows_at = self.position(self.dir.offset, self.dir.files)?;
        let buf = self.fetch(rows_at..self.dir.rows_end)?;
        let mut data = &buf[..];
        if storage::get(&mut data)? != self.dir.rows || self.dir.rows > data.len() / 9 {
            return Err(invalid());
        }
        let mut rows = Vec::with_capacity(self.dir.rows);
        let mut bases = if self.dir.tree {
            vec![0; self.dir.files.checked_add(1).ok_or_else(invalid)?]
        } else {
            vec![]
        };
        let mut tokens = 0;
        for _ in 0..self.dir.rows {
            let row = storage::row(&mut data, tokens, self.dir.files)?;
            if rows.last().is_some_and(|p: &Row| row.file < p.file) {
                return Err(invalid());
            }
            tokens = row.end;
            if self.dir.tree {
                bases[row.file + 1] = tokens
            }
            rows.push(row)
        }
        if tokens != self.dir.tokens || !data.is_empty() {
            return Err(invalid());
        }
        if self.dir.tree {
            for i in 1..bases.len() {
                bases[i] = bases[i].max(bases[i - 1])
            }
            self.bases = Some(bases);
        }
        self.cached_rows = Some(rows);
        Ok(())
    }
    fn lookup(&mut self, key: &[u8]) -> io::Result<Option<Term>> {
        let (mut lo, mut hi) = (0, self.dir.terms);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let range = self.range(self.terms_at, mid, self.dir.offset)?;
            if range.start <= self.dir.rows_end {
                return Err(invalid());
            }
            let size = range.end - range.start;
            let mut prefix = [0; 10];
            let n = size.min(10) as usize;
            (self.read)(range.start, &mut prefix[..n])?;
            let mut header = &prefix[..n];
            let key_len = storage::get(&mut header)?;
            let start = range.start + (n - header.len()) as u64;
            if key_len == 0 || key_len > 32768 || key_len as u64 > range.end - start {
                return Err(invalid());
            }
            let end = start + key_len as u64;
            let found = self.fetch(start..end)?;
            match found.as_slice().cmp(key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let buf = self.fetch(end..range.end)?;
                    let mut data = &buf[..];
                    let term = if self.dir.grouped {
                        if self.dir.tree {
                            self.load_rows()?;
                        }
                        if self.bases.is_none() {
                            let bases = self.fetch(
                                self.bases_at..self.bases_at + 8 * (self.dir.files as u64 + 1),
                            )?;
                            let bases = bases
                                .as_chunks::<8>()
                                .0
                                .iter()
                                .map(|b| {
                                    usize::try_from(u64::from_le_bytes(*b)).map_err(|_| invalid())
                                })
                                .collect::<io::Result<Vec<_>>>()?;
                            if bases.first() != Some(&0)
                                || bases.last() != Some(&self.dir.tokens)
                                || !bases.is_sorted()
                            {
                                return Err(invalid());
                            }
                            self.bases = Some(bases);
                        }
                        grouped::read_term(&mut data, self.dir.rows, self.bases.as_ref().unwrap())?
                    } else {
                        storage::term(&mut data, self.dir.rows, self.dir.tokens)?
                    };
                    if !data.is_empty() {
                        return Err(invalid());
                    }
                    return Ok(Some(term));
                }
            }
        }
        Ok(None)
    }
}
impl RankedIndex {
    pub fn search_extent<'a>(
        length: u64,
        mut read: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
        file_count: usize,
        path: impl Fn(usize) -> &'a str,
        mut source: impl FnMut(usize, Range<u64>) -> io::Result<String>,
        query: &str,
    ) -> io::Result<(Vec<RankedHit>, bool)> {
        let mut magic = [0; 8];
        read(0, &mut magic)?;
        if &magic != b"CIRANK02" && &magic != b"CIRANK03" && &magic != b"CIRANK04" {
            return Err(invalid());
        }
        let mut tail = [0; FOOTER as usize];
        let footer_pos = length.checked_sub(FOOTER).ok_or_else(invalid)?;
        read(footer_pos, &mut tail)?;
        let dir = Directory::decode(&tail)?;
        let next = |start: u64, n: usize| {
            (n as u64)
                .checked_add(1)
                .and_then(|v| v.checked_mul(8))
                .and_then(|v| v.checked_add(start))
                .ok_or_else(invalid)
        };
        let (terms_at, bases_at, directory_end, trees) = if dir.tree {
            let mut h = [0; 16];
            read(dir.offset, &mut h)?;
            let number = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap());
            let first = dir.offset.checked_add(16).ok_or_else(invalid)?;
            let middle = first.checked_add(number(&h[..8])).ok_or_else(invalid)?;
            let end = middle.checked_add(number(&h[8..])).ok_or_else(invalid)?;
            (middle, 0, end, Some([first..middle, middle..end]))
        } else {
            let terms_at = next(dir.offset, dir.files)?;
            let bases_at = next(terms_at, dir.terms)?;
            (
                terms_at,
                bases_at,
                if dir.grouped {
                    next(bases_at, dir.files)?
                } else {
                    bases_at
                },
                None,
            )
        };
        if dir.tree != (&magic == b"CIRANK04")
            || dir.grouped != (&magic != b"CIRANK02")
            || dir.files != file_count
            || directory_end != footer_pos
            || dir.rows_end < 8
            || dir.rows_end >= dir.offset
        {
            return Err(invalid());
        }
        let mut cursor = Reader {
            read: &mut read,
            dir,
            terms_at,
            bases_at,
            bases: None,
            trees,
            nodes: Default::default(),
            cached_rows: None,
        };
        let mut index = RankedIndex::default();
        let mut misses = HashSet::new();
        let plan = Query::parse(query, |k| {
            if let Some(&id) = index.dictionary.get(k) {
                return Ok(Some(id));
            }
            if misses.contains(k) {
                return Ok(None);
            }
            if let Some(term) = cursor.lookup(k)? {
                let id = index.terms.len();
                index.terms.push(term);
                index.dictionary.insert(k.to_owned(), id);
                Ok(Some(id))
            } else {
                misses.insert(k.to_owned());
                Ok(None)
            }
        })?;
        if plan.phrases.is_empty() {
            return Ok((vec![], false));
        }
        let rows_at = cursor.position(cursor.dir.offset, file_count)?;
        cursor.load_rows()?;
        index.tokens = cursor.dir.tokens;
        index.rows = cursor.cached_rows.take().unwrap();
        let mut kept = None;
        index
            .evaluate(
                &plan,
                |a, b| path(a).cmp(path(b)),
                |row, hits, count| {
                    if kept.as_ref().is_none_or(|(id, _)| *id != row.file) {
                        let file = (|| -> io::Result<File> {
                            let r = cursor.range(cursor.dir.offset, row.file, rows_at)?;
                            let buf = cursor.fetch(r)?;
                            let mut data = &buf[..];
                            let f = storage::file(&mut data)?;
                            if f.path != path(row.file) || !data.is_empty() {
                                return Err(invalid());
                            }
                            Ok(f)
                        })()
                        .map_err(|e| e.to_string())?;
                        kept = Some((row.file, file));
                    }
                    let stop = row.span_start + (row.parts_start - row.body_start);
                    let spans = kept
                        .as_ref()
                        .unwrap()
                        .1
                        .spans
                        .get(row.span_start..stop)
                        .ok_or_else(|| invalid().to_string())?;
                    let bytes = &row.chunk.bytes;
                    let text = source(row.file, bytes.start as u64..bytes.end as u64)
                        .map_err(|e| e.to_string())?;
                    if text.len() != bytes.len() {
                        return Err(invalid().to_string());
                    }
                    render(row, spans, &text, bytes.start, hits, count)
                },
            )
            .map_err(io::Error::other)
    }
}
