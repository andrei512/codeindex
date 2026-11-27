use super::*;
use std::io::{self, Read, Write};

pub(super) fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid ranked product")
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Sequential,
    Offsets,
    Grouped,
    Tree,
}

impl Format {
    pub(super) fn read(magic: &[u8; 8]) -> io::Result<Self> {
        match magic {
            b"CIRANK01" => Ok(Self::Sequential),
            b"CIRANK02" => Ok(Self::Offsets),
            b"CIRANK03" => Ok(Self::Grouped),
            b"CIRANK04" => Ok(Self::Tree),
            _ => Err(invalid()),
        }
    }
    pub(super) fn grouped(self) -> bool {
        matches!(self, Self::Grouped | Self::Tree)
    }
}

pub(super) const FOOTER: u64 = 56;
pub(super) fn put(out: &mut impl Write, mut n: usize) -> io::Result<()> {
    let mut buffer = [0; 10];
    let mut i = 0;
    while n >= 128 {
        buffer[i] = n as u8 | 128;
        n >>= 7;
        i += 1
    }
    buffer[i] = n as u8;
    out.write_all(&buffer[..=i])
}
pub(super) fn get(input: &mut impl Read) -> io::Result<usize> {
    let mut n = 0;
    for shift in (0..usize::BITS).step_by(7) {
        let mut b = [0];
        input.read_exact(&mut b)?;
        n |= varint_chunk(b[0], shift)?;
        if b[0] < 128 {
            return Ok(n);
        }
    }
    Err(invalid())
}
fn varint_chunk(byte: u8, shift: u32) -> io::Result<usize> {
    let value = usize::from(byte & 127);
    if value > usize::MAX >> shift {
        return Err(invalid());
    }
    Ok(value << shift)
}

pub(super) fn bytes(input: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let n = get(input)?;
    if n > limit {
        return Err(invalid());
    }
    let mut v = Vec::new();
    input.take(n as u64).read_to_end(&mut v)?;
    if v.len() != n {
        return Err(invalid());
    }
    Ok(v)
}
fn sized(out: &mut impl Write, b: &[u8]) -> io::Result<()> {
    put(out, b.len())?;
    out.write_all(b)
}
struct Count<W> {
    out: W,
    pos: u64,
}
impl<W: Write> Write for Count<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = self.out.write(b)?;
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}
impl<W: Write> Count<W> {
    fn write_file_records<'a>(&mut self, bytes: impl Iterator<Item = &'a [u8]>, starts: &mut Vec<u64>) -> io::Result<()> {
        for record in bytes {
            starts.push(self.pos);
            self.write_all(record)?;
        }
        starts.push(self.pos);
        Ok(())
    }
}
pub(super) fn file(input: &mut impl Read) -> io::Result<File> {
    let path = String::from_utf8(bytes(input, usize::MAX)?).map_err(|_| invalid())?;
    let mut spans = vec![];
    let mut prev = 0usize;
    for _ in 0..get(input)? {
        let start = prev.checked_add(get(input)?).ok_or_else(invalid)?;
        let len = get(input)?;
        if len == 0 {
            return Err(invalid());
        }
        prev = start.checked_add(len).ok_or_else(invalid)?;
        spans.push(start..prev)
    }
    Ok(File { path, spans })
}
pub(super) fn row(input: &mut impl Read, prev: usize, file_count: usize) -> io::Result<Row> {
    let file = get(input)?;
    let first_line = get(input)?;
    let last_line = first_line.checked_add(get(input)?).ok_or_else(invalid)?;
    let start = get(input)?;
    let end = start.checked_add(get(input)?).ok_or_else(invalid)?;
    let span_start = get(input)?;
    let body_start = prev.checked_add(get(input)?).ok_or_else(invalid)?;
    let parts_start = body_start.checked_add(get(input)?).ok_or_else(invalid)?;
    let row_end = parts_start.checked_add(get(input)?).ok_or_else(invalid)?;
    if first_line == 0
        || file >= file_count
        || start == end
        || span_start.checked_add(parts_start - body_start).is_none()
    {
        return Err(invalid());
    }
    Ok(Row {
        file,
        chunk: ChunkRange {
            first_line,
            last_line,
            bytes: start..end,
        },
        span_start,
        body_start,
        parts_start,
        end: row_end,
    })
}
pub(super) fn read_positions(
    input: &mut impl Read,
    bounds: std::ops::Range<usize>,
    n: usize,
    positions: &mut Vec<usize>,
) -> io::Result<()> {
    visit_positions(input, bounds, n, |pos| positions.push(pos))
}

pub(super) fn visit_positions(
    input: &mut impl Read,
    bounds: std::ops::Range<usize>,
    n: usize,
    mut observe: impl FnMut(usize),
) -> io::Result<()> {
    let mut last = bounds.start;
    for first in (0..n).map(|i| i == 0) {
        let delta = get(input)?;
        last = last.checked_add(delta).ok_or_else(invalid)?;
        let in_bounds = last < bounds.end;
        let ordered = first || delta != 0;
        if !in_bounds || !ordered {
            return Err(invalid());
        }
        observe(last);
    }
    Ok(())
}

pub(super) fn term_counts(
    input: &mut impl Read,
    rows: usize,
    tokens: usize,
) -> io::Result<(usize, usize)> {
    let df = get(input)?;
    let count = get(input)?;
    if !(1..=rows.min(count)).contains(&df) || count > tokens {
        return Err(invalid());
    }
    Ok((df, count))
}
pub(super) fn term(input: &mut impl Read, rows: usize, tokens: usize) -> io::Result<Term> {
    let (df, count) = term_counts(input, rows, tokens)?;
    let mut positions = Vec::new();
    read_positions(input, 0..tokens, count, &mut positions)?;
    Ok(Term {
        positions,
        document_frequency: df,
    })
}
/// Bulk builds already hold serialized groups. Other writers derive groups
/// from positions using one set of file-base hints for the entire dictionary.
enum TermSource<'a> {
    Packed(&'a BulkRank),
    Positions {
        bases: Vec<usize>,
        hints: Vec<usize>,
    },
}

impl<'a> TermSource<'a> {
    fn new(index: &RankedIndex, bulk: Option<&'a BulkRank>) -> io::Result<Self> {
        if let Some(bulk) = bulk {
            return Ok(Self::Packed(bulk));
        }
        let bases = index.file_bases()?;
        let hints = grouped::file_hints(index.tokens, &bases);
        Ok(Self::Positions { bases, hints })
    }

    fn write(
        &self,
        out: &mut impl Write,
        id: usize,
        term: &Term,
        scratch: &mut Vec<u8>,
    ) -> io::Result<()> {
        match self {
            Self::Packed(bulk) => {
                let packed = &bulk.terms[id];
                put(out, term.document_frequency)?;
                put(out, packed.count)?;
                out.write_all(&packed.groups)
            }
            Self::Positions { bases, hints } => {
                scratch.clear();
                grouped::write_term(scratch, term, bases, hints)?;
                out.write_all(scratch)
            }
        }
    }
}
impl RankedIndex {
    pub fn write_to(&self, out: impl Write) -> io::Result<()> {
        self.write_with(out, None)
    }
    pub(super) fn write_with(&self, out: impl Write, bulk: Option<&BulkRank>) -> io::Result<()> {
        // Collect record starts as the encoded bytes go to their final destination.
        let mut out = Count {
            out: io::BufWriter::with_capacity(64 * 1024, out),
            pos: 0,
        };
        out.write_all(b"CIRANK04")?;
        let terms = TermSource::new(self, bulk)?;
        put(&mut out, self.files.len())?;
        let mut starts = Vec::with_capacity(self.files.len() + self.terms.len() + 2);
        let mut scratch = Vec::new();
        self.write_files(&mut out, bulk, &mut scratch, &mut starts)?;
        put(&mut out, self.rows.len())?;
        self.write_rows(&mut out, |r| r.file)?;
        let rows_end = out.pos;
        put(&mut out, self.terms.len())?;
        let mut keys: Vec<_> = self.dictionary.iter().collect();
        keys.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        for (key, &id) in keys {
            starts.push(out.pos);
            sized(&mut out, key)?;
            terms.write(&mut out, id, &self.terms[id], &mut scratch)?;
        }
        let directory = out.pos;
        starts.push(directory);
        let (files, terms) = starts.split_at(self.files.len() + 1);
        super::cursor::Directory {
            format: Format::Tree,
            files: self.files.len(),
            terms: self.terms.len(),
            offset: directory,
            rows_end,
            tokens: self.tokens,
            rows: self.rows.len(),
        }
        .write_tree(&mut out, files, terms)?;
        out.flush()
    }
    pub(super) fn write_rows(
        &self,
        out: &mut impl Write,
        file: impl Fn(&Row) -> usize,
    ) -> io::Result<()> {
        let mut previous = 0;
        for row in &self.rows {
            for field in row.encoded_fields(file(row), previous) {
                put(out, field)?;
            }
            previous = row.end;
        }
        Ok(())
    }
    fn write_files<W: Write>(
        &self,
        out: &mut Count<W>,
        bulk: Option<&BulkRank>,
        scratch: &mut Vec<u8>,
        starts: &mut Vec<u64>,
    ) -> io::Result<()> {
        if let Some(bulk) = bulk {
            return out.write_file_records(bulk.file_ranges.windows(2).map(|range| {
                &bulk.file_bytes[range[0]..range[1]]
            }), starts);
        }
        for file in &self.files {
            scratch.clear();
            bulk::encode_file(file, scratch);
            starts.push(out.pos);
            out.write_all(scratch)?;
        }
        starts.push(out.pos);
        Ok(())
    }
    pub fn read_from(mut input: impl Read) -> io::Result<Self> {
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        let format = Format::read(&magic)?;
        let mut index = Self::default();
        let nfiles = get(&mut input)?;
        for _ in 0..nfiles {
            index.files.push(file(&mut input)?)
        }
        index.read_rows(&mut input)?;
        let bases = index.file_bases()?;
        index.read_dictionary(&mut input, format, &bases)?;
        index.check_directory(&mut input, format, &bases)?;
        Ok(index)
    }

    fn read_dictionary(
        &mut self,
        input: &mut impl Read,
        format: Format,
        bases: &[usize],
    ) -> io::Result<()> {
        let nrows = self.rows.len();
        let nterms = get(input)?;
        let mut total = 0usize;
        for _ in 0..nterms {
            let key = bytes(input, 32768)?;
            let slot = vacant_key(&mut self.dictionary, key)?;
            let t = term_record(input, format, TermBounds { rows: nrows, tokens: self.tokens, bases })?;
            total = total.checked_add(t.positions.len()).ok_or_else(invalid)?;
            slot.insert(self.terms.len());
            self.terms.push(t)
        }
        if total != self.tokens {
            return Err(invalid());
        }
        Ok(())
    }

    fn read_rows(&mut self, input: &mut impl Read) -> io::Result<()> {
        let nrows = get(input)?;
        for _ in 0..nrows {
            let r = row(input, self.tokens, self.files.len())?;
            let spans = self.files[r.file]
                .spans
                .get(r.span_range())
                .ok_or_else(invalid)?;
            if spans.first().is_some_and(|s| s.start < r.chunk.bytes.start)
                || spans.last().is_some_and(|s| s.end > r.chunk.bytes.end)
            {
                return Err(invalid());
            }
            self.tokens = r.end;
            self.rows.push(r)
        }
        Ok(())
    }
    fn check_directory(
        &self,
        mut input: impl Read,
        format: Format,
        bases: &[usize],
    ) -> io::Result<()> {
        match format {
            Format::Sequential => Ok(()),
            Format::Tree => self.check_tree(&mut input),
            format => self.check_flat(&mut input, format, bases),
        }
    }

    fn checked_footer(
        &self,
        bytes: &[u8; FOOTER as usize],
    ) -> io::Result<super::cursor::Directory> {
        let dir = super::cursor::Directory::decode(bytes)?;
        if (dir.files, dir.terms, dir.tokens, dir.rows)
            != (
                self.files.len(),
                self.terms.len(),
                self.tokens,
                self.rows.len(),
            )
        {
            return Err(invalid());
        }
        Ok(dir)
    }

    fn check_tree(&self, input: &mut impl Read) -> io::Result<()> {
        let mut buf = Vec::new();
        input.read_to_end(&mut buf)?;
        let boundary = buf.len().checked_sub(FOOTER as usize).ok_or_else(invalid)?;
        let dir = self.checked_footer(buf[boundary..].try_into().unwrap())?;
        if dir.format != Format::Tree {
            return Err(invalid());
        }
        dir.validate_trees(&buf[..boundary])
    }

    fn check_legacy_bases(input: &mut impl Read, bases: &[usize]) -> io::Result<()> {
        for &base in bases {
            let mut bytes = [0; 8];
            input.read_exact(&mut bytes)?;
            if u64::from_le_bytes(bytes) != base as u64 {
                return Err(invalid());
            }
        }
        Ok(())
    }

    fn check_flat(&self, input: &mut impl Read, format: Format, bases: &[usize]) -> io::Result<()> {
        let mut last = 0;
        for _ in 0..self.files.len() + self.terms.len() + 2 {
            let mut b = [0; 8];
            input.read_exact(&mut b)?;
            let at = u64::from_le_bytes(b);
            if at < last {
                return Err(invalid());
            }
            last = at
        }
        if format == Format::Grouped {
            Self::check_legacy_bases(input, bases)?;
        }
        let mut tail = [0; FOOTER as usize];
        input.read_exact(&mut tail)?;
        let dir = self.checked_footer(&tail)?;
        if dir.format.grouped() != (format == Format::Grouped)
            || dir.offset != last
        {
            return Err(invalid());
        }
        Ok(())
    }
}


fn vacant_key(
    dictionary: &mut std::collections::HashMap<Vec<u8>, usize>,
    key: Vec<u8>,
) -> io::Result<std::collections::hash_map::VacantEntry<'_, Vec<u8>, usize>> {
    use std::collections::hash_map::Entry;
    if key.is_empty() {
        return Err(invalid());
    }
    match dictionary.entry(key) {
        Entry::Vacant(entry) => Ok(entry),
        Entry::Occupied(_) => Err(invalid()),
    }
}

pub(super) struct TermBounds<'a> {
    pub rows: usize,
    pub tokens: usize,
    pub bases: &'a [usize],
}

pub(super) fn term_record(
    input: &mut impl Read,
    format: Format,
    bounds: TermBounds<'_>,
) -> io::Result<Term> {
    if format.grouped() {
        grouped::read_term(input, bounds.rows, bounds.bases)
    } else {
        term(input, bounds.rows, bounds.tokens)
    }
}
