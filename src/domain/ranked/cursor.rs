//! Sorted term records have compact delta postings and a fixed-width offset directory.
//! A miss probes keys only. A hit reads its lists, the chunk directory and retained file spans.
use super::{
    storage::{self, FOOTER, Format, invalid},
    *,
};
use std::{collections::HashMap, io};
struct Layout {
    terms_at: u64,
    bases_at: u64,
    end: u64,
    trees: Option<[Range<u64>; 2]>,
}

#[derive(Clone, Copy)]
enum RecordTable {
    File,
    Term,
}

fn offsets_end(start: u64, count: usize) -> io::Result<u64> {
    (count as u64)
        .checked_add(1)
        .and_then(|v| v.checked_mul(8))
        .and_then(|v| v.checked_add(start))
        .ok_or_else(invalid)
}

pub(super) struct Directory {
    pub(super) format: Format,
    pub files: usize,
    pub terms: usize,
    pub offset: u64,
    pub(super) rows_end: u64,
    pub tokens: usize,
    pub rows: usize,
}
/// Count, byte boundaries, file order and token totals are common to ranked queries and edits.
pub(super) struct RowBlock<'a> {
    directory: &'a Directory,
    data: &'a [u8],
}
impl RowBlock<'_> {
    pub(super) fn visit(self, mut accept: impl FnMut(u64, usize, Row)) -> io::Result<()> {
        let dir = self.directory;
        let mut data = self.data;
        let (mut tokens, mut file) = (0, 0);
        for _ in 0..dir.rows {
            let offset = dir.rows_end - data.len() as u64;
            let row = storage::row(&mut data, tokens, dir.files)?;
            if row.file < file {
                return Err(invalid());
            }
            file = row.file;
            let start = tokens;
            tokens = row.end;
            accept(offset, start, row);
        }
        if tokens != dir.tokens || !data.is_empty() {
            return Err(invalid());
        }
        Ok(())
    }
}
impl Directory {
    pub(super) fn row_block<'a>(&'a self, mut bytes: &'a [u8]) -> io::Result<RowBlock<'a>> {
        if bytes.len() as u64 > self.rows_end
            || storage::get(&mut bytes)? != self.rows
            || self.rows > bytes.len() / 9
        {
            return Err(invalid());
        }
        Ok(RowBlock {
            directory: self,
            data: bytes,
        })
    }

    pub(super) fn write_tree(
        &self,
        out: &mut impl io::Write,
        files: &[u64],
        terms: &[u64],
    ) -> io::Result<()> {
        directory::write(
            &[
                directory::Node::from_offsets(files),
                directory::Node::from_offsets(terms),
            ],
            out,
        )?;
        out.write_all(&self.tree_footer())
    }
    pub(super) fn tree_footer(&self) -> [u8; FOOTER as usize] {
        let mut bytes = [0; FOOTER as usize];
        bytes[..8].copy_from_slice(b"RANKDIR4");
        for (slot, value) in bytes[8..].chunks_exact_mut(8).zip([
            self.files as u64,
            self.terms as u64,
            self.offset,
            self.rows_end,
            self.tokens as u64,
            self.rows as u64,
        ]) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    fn first_file(&self) -> u64 {
        8 + directory::var_len(self.files)
    }
    fn first_term(&self) -> io::Result<u64> {
        self.rows_end
            .checked_add(directory::var_len(self.terms))
            .ok_or_else(invalid)
    }

    fn check_tree_ends(&self, files_end: u64, terms_end: u64) -> io::Result<()> {
        if files_end > self.rows_end || terms_end != self.offset {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn validate_trees(&self, data: &[u8]) -> io::Result<()> {
        let [files, terms] = directory::validate(
            data,
            [self.first_file(), self.first_term()?],
            [self.files, self.terms],
        )?;
        self.check_tree_ends(files, terms)
    }

    pub(super) fn load_trees(&self, data: &[u8], base: u64) -> io::Result<directory::Expanded> {
        let expanded = directory::load(
            data,
            base,
            [self.first_file(), self.first_term()?],
            [self.files, self.terms],
        )?;
        let [files, terms] = &expanded.1;
        self.check_tree_ends(files[self.files], terms[self.terms])?;
        Ok(expanded)
    }
    fn subtree(&self, trees: &[Range<u64>; 2], table: RecordTable) -> io::Result<directory::Subtree> {
        let (root, first, count) = match table {
            RecordTable::File => (&trees[0], self.first_file(), self.files),
            RecordTable::Term => (&trees[1], self.first_term()?, self.terms),
        };
        Ok(directory::Subtree { bytes: root.clone(), count, start: first })
    }

    fn layout(&self, read: &mut ReadAt<'_>) -> io::Result<Layout> {
        if self.format == Format::Tree {
            let mut header = [0; directory::HEADER];
            read(self.offset, &mut header)?;
            let ranges = directory::tree_ranges(&header, self.offset)?;
            return Ok(Layout {
                terms_at: ranges[1].start,
                bases_at: 0,
                end: ranges[1].end,
                trees: Some(ranges),
            });
        }
        let terms_at = offsets_end(self.offset, self.files)?;
        let bases_at = offsets_end(terms_at, self.terms)?;
        let end = if self.format.grouped() { offsets_end(bases_at, self.files)? } else { bases_at };
        Ok(Layout { terms_at, bases_at, end, trees: None })
    }
    pub(super) fn decode(tail: &[u8; FOOTER as usize]) -> io::Result<Self> {
        let format = match &tail[..8] {
            b"RANKDIR2" => Format::Offsets,
            b"RANKDIR3" => Format::Grouped,
            b"RANKDIR4" => Format::Tree,
            _ => return Err(invalid()),
        };
        let n = |p: usize| u64::from_le_bytes(tail[p..p + 8].try_into().unwrap());
        let count = |p| usize::try_from(n(p)).map_err(|_| invalid());
        Ok(Self {
            format,
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
    cached_rows: Option<(u64, Vec<Row>)>,
}
impl<'a> Reader<'a> {
    fn open(length: u64, read: &'a mut ReadAt<'a>, file_count: usize) -> io::Result<Self> {
        let mut magic = [0; 8];
        read(0, &mut magic)?;
        let format = Format::read(&magic)?;
        if format == Format::Sequential {
            return Err(invalid());
        }
        let mut tail = [0; FOOTER as usize];
        let footer_pos = length.checked_sub(FOOTER).ok_or_else(invalid)?;
        read(footer_pos, &mut tail)?;
        let dir = Directory::decode(&tail)?;
        let Layout {
            terms_at,
            bases_at,
            end,
            trees,
        } = dir.layout(read)?;
        if dir.format != format
            || dir.files != file_count
            || end != footer_pos
            || dir.rows_end < 8
            || dir.rows_end >= dir.offset
        {
            return Err(invalid());
        }
        Ok(Self {
            read,
            dir,
            terms_at,
            bases_at,
            bases: None,
            trees,
            nodes: Default::default(),
            cached_rows: None,
        })
    }

    fn offset_base(&self, table: RecordTable) -> u64 {
        match table {
            RecordTable::File => self.dir.offset,
            RecordTable::Term => self.terms_at,
        }
    }

    fn position(&mut self, table: RecordTable, slot: usize) -> io::Result<u64> {
        if self.dir.format == Format::Tree {
            return Ok(self.range(table, slot, self.dir.offset)?.start);
        }
        let mut b = [0; 8];
        let base = self.offset_base(table);
        (self.read)(base + (slot as u64) * 8, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    fn range(&mut self, table: RecordTable, slot: usize, limit: u64) -> io::Result<Range<u64>> {
        if let Some(trees) = &self.trees {
            let subtree = self.dir.subtree(trees, table)?;
            let range = self.nodes.range(self.read, subtree, slot)?;
            return (range.end <= limit).then_some(range).ok_or_else(invalid);
        }
        let mut b = [0; 16];
        let base = self.offset_base(table);
        (self.read)(base + (slot as u64) * 8, &mut b)?;
        let lo = u64::from_le_bytes(b[..8].try_into().unwrap());
        let hi = u64::from_le_bytes(b[8..].try_into().unwrap());
        let valid = (8..=hi).contains(&lo) && hi <= limit;
        valid.then_some(lo..hi).ok_or_else(invalid)
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
    fn file(&mut self, id: usize, rows_at: u64, expected: &str) -> io::Result<File> {
        let range = self.range(RecordTable::File, id, rows_at)?;
        let buf = self.fetch(range)?;
        let mut data = &buf[..];
        let file = storage::file(&mut data)?;
        if file.path != expected || !data.is_empty() { return Err(invalid()); }
        Ok(file)
    }

    fn load_rows(&mut self) -> io::Result<()> {
        if self.cached_rows.is_some() {
            return Ok(());
        }
        let rows_at = self.position(RecordTable::File, self.dir.files)?;
        let buf = self.fetch(rows_at..self.dir.rows_end)?;
        let block = self.dir.row_block(&buf)?;
        let mut rows = Vec::with_capacity(self.dir.rows);
        let mut bases = (self.dir.format == Format::Tree)
            .then(|| grouped::FileBases::new(self.dir.files))
            .transpose()?;
        block.visit(|_, start, row| {
            if let Some(bases) = &mut bases {
                bases.observe(row.file, start);
            }
            rows.push(row);
        })?;
        if let Some(bases) = bases {
            self.bases = Some(bases.finish(self.dir.tokens));
        }
        self.cached_rows = Some((rows_at, rows));
        Ok(())
    }
    fn load_bases(&mut self) -> io::Result<()> {
        if self.dir.format == Format::Tree {
            self.load_rows()?;
        }
        if self.bases.is_none() {
            let bases =
                self.fetch(self.bases_at..self.bases_at + 8 * (self.dir.files as u64 + 1))?;
            let bases = bases
                .as_chunks::<8>()
                .0
                .iter()
                .map(|b| usize::try_from(u64::from_le_bytes(*b)).map_err(|_| invalid()))
                .collect::<io::Result<Vec<_>>>()?;
            if bases.first() != Some(&0)
                || bases.last() != Some(&self.dir.tokens)
                || !bases.is_sorted()
            {
                return Err(invalid());
            }
            self.bases = Some(bases);
        }
        Ok(())
    }
    fn decode_term(&mut self, mut data: &[u8]) -> io::Result<Term> {
        if self.dir.format.grouped() {
            self.load_bases()?;
        }
        let term = storage::term_record(
            &mut data,
            self.dir.format,
            storage::TermBounds {
                rows: self.dir.rows,
                tokens: self.dir.tokens,
                bases: self.bases.as_deref().unwrap_or_default(),
            },
        )?;
        if !data.is_empty() {
            return Err(invalid());
        }
        Ok(term)
    }

    fn probe<'p>(&mut self, slot: usize, bytes: &'p mut [u8; term_key::PROBE]) -> io::Result<term_key::Key<'p>> {
        let range = self.range(RecordTable::Term, slot, self.dir.offset)?;
        if range.start <= self.dir.rows_end {
            return Err(invalid());
        }
        let size = (range.end - range.start).min(bytes.len() as u64) as usize;
        (self.read)(range.start, &mut bytes[..size])?;
        term_key::Key::parse(range, &bytes[..size])
    }

    fn lookup(&mut self, key: &[u8]) -> io::Result<Option<Term>> {
        let (mut lo, mut hi) = (0, self.dir.terms);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mut bytes = [0; term_key::PROBE];
            let probe = self.probe(mid, &mut bytes)?;
            match probe.compare(key, |r| self.fetch(r))? {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let bytes = probe.postings(&mut self.read)?;
                    let term = self.decode_term(&bytes)?;
                    return Ok(Some(term));
                }
            }
        }
        Ok(None)
    }
}
impl RankedIndex {
    /// Search a serialized ranked index using the files from its original catalog.
    pub fn search_extent<'a, Path, Source>(
        extent: impl crate::domain::content_lookup::ExtentRead,
        catalog: super::ExtentCatalog<Path, Source>,
        query: &str,
    ) -> io::Result<(Vec<RankedHit>, bool)>
    where
        Path: Fn(usize) -> &'a str,
        Source: FnMut(usize, Range<u64>) -> io::Result<String>,
    {
        Self::project_extent(extent, catalog, query, |hit| hit.full_hit())
    }

    pub(crate) fn project_extent<'a, T, Path, Source>(
        mut extent: impl crate::domain::content_lookup::ExtentRead,
        catalog: super::ExtentCatalog<Path, Source>,
        query: &str,
        project: impl Fn(MatchedChunk<'_>) -> T,
    ) -> io::Result<(Vec<T>, bool)>
    where
        Path: Fn(usize) -> &'a str,
        Source: FnMut(usize, Range<u64>) -> io::Result<String>,
    {
        Reader::open(
            extent.length(),
            &mut |at, bytes| extent.read(at, bytes),
            catalog.count,
        )?
        .project(catalog.path, catalog.source, query, project)
    }
}

impl Reader<'_> {
    fn project<'a, T>(
        mut self,
        path: impl Fn(usize) -> &'a str,
        mut source: impl FnMut(usize, Range<u64>) -> io::Result<String>,
        query: &str,
        project: impl Fn(MatchedChunk<'_>) -> T,
    ) -> io::Result<(Vec<T>, bool)> {
        let mut index = RankedIndex::default();
        // Query parsing can request the same folded key multiple times. Cache
        // both present and absent keys in one table for this extent.
        let mut resolved = HashMap::new();
        let plan = Query::parse(query, |key| {
            if let Some(&known) = resolved.get(key) {
                return Ok(known);
            }
            let id = self.lookup(key)?.map(|term| {
                let id = index.terms.len();
                index.terms.push(term);
                id
            });
            resolved.insert(key.to_vec(), id);
            Ok(id)
        })?;
        if plan.phrases.is_empty() {
            return Ok((vec![], false));
        }
        self.load_rows()?;
        let (rows_at, rows) = self.cached_rows.take().unwrap();
        index.tokens = self.dir.tokens;
        index.rows = rows;
        let mut kept = None;
        index
            .evaluate(
                &plan,
                |a, b| path(a).cmp(path(b)),
                |row, score, hits| {
                    if kept.as_ref().is_none_or(|(id, _)| *id != row.file) {
                        let file = self
                            .file(row.file, rows_at, path(row.file))
                            .map_err(|e| e.to_string())?;
                        kept = Some((row.file, file));
                    }
                    let spans = kept
                        .as_ref()
                        .unwrap()
                        .1
                        .spans
                        .get(row.span_range())
                        .ok_or_else(|| invalid().to_string())?;
                    let bytes = &row.chunk.bytes;
                    let text = source(row.file, bytes.start as u64..bytes.end as u64)
                        .map_err(|e| e.to_string())?;
                    if text.len() != bytes.len() {
                        return Err(invalid().to_string());
                    }
                    MatchedChunk::new(
                        row,
                        score,
                        result::ChunkText { text: &text, base: bytes.start },
                        result::ChunkEvidence { spans, hits },
                    )
                        .map(&project)
                },
            )
            .map_err(io::Error::other)
    }
}
