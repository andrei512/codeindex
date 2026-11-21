use super::*;
use std::io::{self, Read, Write};

pub(super) fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid ranked product")
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
        let v = (b[0] & 127) as usize;
        if v > usize::MAX >> shift {
            return Err(invalid());
        }
        n |= v << shift;
        if b[0] < 128 {
            return Ok(n);
        }
    }
    Err(invalid())
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
pub(super) fn term(input: &mut impl Read, rows: usize, tokens: usize) -> io::Result<Term> {
    let df = get(input)?;
    let count = get(input)?;
    if df == 0 || df > rows || df > count || count > tokens {
        return Err(invalid());
    }
    let mut previous = 0usize;
    let mut positions = vec![];
    for i in 0..count {
        let delta = get(input)?;
        previous = previous.checked_add(delta).ok_or_else(invalid)?;
        if previous >= tokens || i > 0 && delta == 0 {
            return Err(invalid());
        }
        positions.push(previous);
    }
    Ok(Term {
        positions,
        document_frequency: df,
    })
}
impl RankedIndex {
    pub fn write_to(&self, out: impl Write) -> io::Result<()> {
        // Collect record starts as the encoded bytes go to their final destination.
        let mut out = Count {
            out: io::BufWriter::with_capacity(64 * 1024, out),
            pos: 0,
        };
        out.write_all(b"CIRANK04")?;
        let bases = self.file_bases()?;
        let hints = grouped::file_hints(self.tokens, &bases);
        put(&mut out, self.files.len())?;
        let mut starts = Vec::with_capacity(self.files.len() + self.terms.len() + 2);
        let mut scratch = Vec::new();
        for f in &self.files {
            scratch.clear();
            starts.push(out.pos);
            sized(&mut scratch, f.path.as_bytes())?;
            put(&mut scratch, f.spans.len())?;
            let mut prev = 0;
            for s in &f.spans {
                put(&mut scratch, s.start - prev)?;
                put(&mut scratch, s.len())?;
                prev = s.end;
            }
            out.write_all(&scratch)?;
        }
        starts.push(out.pos);
        put(&mut out, self.rows.len())?;
        let mut prev = 0;
        for r in &self.rows {
            for n in [
                r.file,
                r.chunk.first_line,
                r.chunk.last_line - r.chunk.first_line,
                r.chunk.bytes.start,
                r.chunk.bytes.len(),
                r.span_start,
                r.body_start - prev,
                r.parts_start - r.body_start,
                r.end - r.parts_start,
            ] {
                put(&mut out, n)?
            }
            prev = r.end;
        }
        let rows_end = out.pos;
        put(&mut out, self.terms.len())?;
        let mut keys: Vec<_> = self.dictionary.iter().collect();
        keys.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        for (key, &id) in keys {
            starts.push(out.pos);
            sized(&mut out, key)?;
            let t = &self.terms[id];
            scratch.clear();
            grouped::write_term(&mut scratch, t, &bases, &hints)?;
            out.write_all(&scratch)?;
        }
        let directory = out.pos;
        starts.push(directory);
        let (files, terms) = starts.split_at(self.files.len() + 1);
        super::directory::write(
            &[
                super::directory::Node::from_offsets(files),
                super::directory::Node::from_offsets(terms),
            ],
            &mut out,
        )?;
        out.write_all(b"RANKDIR4")?;
        for n in [
            self.files.len() as u64,
            self.terms.len() as u64,
            directory,
            rows_end,
            self.tokens as u64,
            self.rows.len() as u64,
        ] {
            out.write_all(&n.to_le_bytes())?
        }
        out.flush()
    }
    pub fn read_from(mut input: impl Read) -> io::Result<Self> {
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        if &magic != b"CIRANK01"
            && &magic != b"CIRANK02"
            && &magic != b"CIRANK03"
            && &magic != b"CIRANK04"
        {
            return Err(invalid());
        }
        let mut index = Self::default();
        let nfiles = get(&mut input)?;
        for _ in 0..nfiles {
            index.files.push(file(&mut input)?)
        }
        let nrows = get(&mut input)?;
        for _ in 0..nrows {
            let r = row(&mut input, index.tokens, nfiles)?;
            let stop = r.span_start + (r.parts_start - r.body_start);
            let spans = index.files[r.file]
                .spans
                .get(r.span_start..stop)
                .ok_or_else(invalid)?;
            if spans.first().is_some_and(|s| s.start < r.chunk.bytes.start)
                || spans.last().is_some_and(|s| s.end > r.chunk.bytes.end)
            {
                return Err(invalid());
            }
            index.tokens = r.end;
            index.rows.push(r)
        }
        let bases = index.file_bases()?;
        let nterms = get(&mut input)?;
        let mut total = 0usize;
        for _ in 0..nterms {
            let key = bytes(&mut input, 32768)?;
            if key.is_empty() || index.dictionary.contains_key(&key) {
                return Err(invalid());
            }
            let t = if matches!(&magic, b"CIRANK03" | b"CIRANK04") {
                grouped::read_term(&mut input, nrows, &bases)?
            } else {
                term(&mut input, nrows, index.tokens)?
            };
            total = total.checked_add(t.positions.len()).ok_or_else(invalid)?;
            index.dictionary.insert(key, index.terms.len());
            index.terms.push(t)
        }
        if total != index.tokens {
            return Err(invalid());
        }
        if &magic == b"CIRANK04" {
            let mut buf = Vec::new();
            input.read_to_end(&mut buf)?;
            let boundary = buf.len().checked_sub(FOOTER as usize).ok_or_else(invalid)?;
            let dir = super::cursor::Directory::decode(buf[boundary..].try_into().unwrap())?;
            if !dir.tree
                || dir.files != nfiles
                || dir.terms != nterms
                || dir.tokens != index.tokens
                || dir.rows != nrows
            {
                return Err(invalid());
            }
            let (_, [files, terms]) = super::directory::load(
                &buf[..boundary],
                0,
                [
                    8 + super::directory::var_len(nfiles),
                    dir.rows_end
                        .checked_add(super::directory::var_len(nterms))
                        .ok_or_else(invalid)?,
                ],
                [nfiles, nterms],
            )?;
            if files[nfiles] > dir.rows_end || terms[nterms] != dir.offset {
                return Err(invalid());
            }
        } else if &magic != b"CIRANK01" {
            let mut last = 0;
            for _ in 0..nfiles + nterms + 2 {
                let mut b = [0; 8];
                input.read_exact(&mut b)?;
                let at = u64::from_le_bytes(b);
                if at < last {
                    return Err(invalid());
                }
                last = at
            }
            if &magic == b"CIRANK03" {
                for &base in &bases {
                    let mut b = [0; 8];
                    input.read_exact(&mut b)?;
                    if u64::from_le_bytes(b) != base as u64 {
                        return Err(invalid());
                    }
                }
            }
            let mut tail = [0; FOOTER as usize];
            input.read_exact(&mut tail)?;
            let dir = super::cursor::Directory::decode(&tail)?;
            if dir.grouped != (&magic == b"CIRANK03")
                || dir.files != nfiles
                || dir.terms != nterms
                || dir.rows != nrows
                || dir.tokens != index.tokens
                || dir.offset != last
            {
                return Err(invalid());
            }
        }
        Ok(index)
    }
}
