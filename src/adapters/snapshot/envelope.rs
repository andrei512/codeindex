use super::crc::crc32c;
use std::{
    fs::File,
    io::{self, BufWriter, Read, Seek, Write},
    ops::Range,
    os::unix::fs::FileExt,
};

const HEADER: u64 = 64;
const BLOCK: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    Manifest,
    Segment,
}

impl FileKind {
    fn magic(self) -> &'static [u8; 8] {
        match self {
            Self::Manifest => b"CIXMAN01",
            Self::Segment => b"CIXSEG01",
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid snapshot envelope")
}

fn physical_size(len: u64) -> io::Result<u64> {
    HEADER
        .checked_add(len)
        .and_then(|n| n.checked_add(4 * len.div_ceil(BLOCK)))
        .ok_or_else(invalid)
}

fn valid_root(kind: FileKind, root: &Range<u64>, len: u64) -> bool {
    root.start <= root.end
        && root.end <= len
        && match kind {
            FileKind::Manifest => matches!(root.end - root.start, 240 | 264 | 312),
            FileKind::Segment => root == &(0..0),
        }
}

fn header(kind: FileKind, id: u64, root: &Range<u64>, len: u64) -> [u8; 64] {
    let mut h = [0u8; 64];
    h[..8].copy_from_slice(kind.magic());
    let major: u16 = if kind == FileKind::Manifest && root == &(0..312) {
        2
    } else {
        1
    };
    h[8..10].copy_from_slice(&major.to_le_bytes());
    h[10] = u8::from(kind == FileKind::Manifest && root == &(0..264));
    h[12..14].copy_from_slice(&64u16.to_le_bytes());
    h[16..24].copy_from_slice(&id.to_le_bytes());
    h[24..32].copy_from_slice(&len.to_le_bytes());
    h[32..40].copy_from_slice(&root.start.to_le_bytes());
    h[40..48].copy_from_slice(&(root.end - root.start).to_le_bytes());
    h[48..52].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    let crc = crc32c(&h[..60]);
    h[60..].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Writes extent payload bytes directly to their final offsets. Only one partial
/// checksum block and the four-byte trailer entry per finished block stay live.
pub struct SegmentWriter {
    sink: BufWriter<File>,
    length: u64,
    checksums: Vec<u8>,
    block: Vec<u8>,
}
impl SegmentWriter {
    pub fn new(file: File) -> io::Result<Self> {
        if file.metadata()?.len() != 0 {
            return Err(invalid());
        }
        let mut sink = BufWriter::new(file);
        sink.write_all(&[0; 64])?;
        Ok(Self {
            sink,
            length: 0,
            checksums: Vec::new(),
            block: Vec::with_capacity(BLOCK as usize),
        })
    }
    pub fn offset(&self) -> u64 {
        self.length
    }
    pub fn finish(mut self, id: u64) -> io::Result<File> {
        if id == 0 {
            return Err(invalid());
        }
        if !self.block.is_empty() {
            self.checksums.extend(crc32c(&self.block).to_le_bytes());
        }
        self.sink.write_all(&self.checksums)?;
        self.sink.rewind()?;
        self.sink
            .write_all(&header(FileKind::Segment, id, &(0..0), self.length))?;
        self.sink.into_inner().map_err(|e| e.into_error())
    }
}
impl Write for SegmentWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.length
            .checked_add(buf.len() as u64)
            .ok_or_else(invalid)?;
        self.sink.write_all(buf)?;
        let mut rest = buf;
        if !self.block.is_empty() {
            let n = rest.len().min(BLOCK as usize - self.block.len());
            self.block.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if self.block.len() == BLOCK as usize {
                self.checksums.extend(crc32c(&self.block).to_le_bytes());
                self.block.clear();
            }
        }
        let (blocks, tail) = rest.as_chunks::<4096>();
        for block in blocks {
            self.checksums.extend(crc32c(block).to_le_bytes());
        }
        self.block.extend_from_slice(tail);
        self.length += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

/// Write one private, previously empty file. The publisher orders file and
/// directory synchronization after all referenced extents have been sealed.
pub fn write_envelope(
    file: &mut File,
    kind: FileKind,
    id: u64,
    root: Range<u64>,
    len: u64,
    mut input: impl Read,
) -> io::Result<()> {
    physical_size(len)?;
    if id == 0
        || !valid_root(kind, &root, len)
        || file.metadata()?.len() != 0
        || file.stream_position()? != 0
    {
        return Err(invalid());
    }
    let h = header(kind, id, &root, len);
    let mut sink = BufWriter::new(file);
    sink.write_all(&h)?;
    let mut checksums = Vec::new();
    let checksum_bytes = usize::try_from(4 * len.div_ceil(BLOCK)).map_err(|_| invalid())?;
    checksums
        .try_reserve_exact(checksum_bytes)
        .map_err(io::Error::other)?;
    let mut remaining = len;
    let mut block = [0; BLOCK as usize];
    while remaining != 0 {
        let n = remaining.min(BLOCK) as usize;
        input.read_exact(&mut block[..n])?;
        sink.write_all(&block[..n])?;
        checksums.extend_from_slice(&crc32c(&block[..n]).to_le_bytes());
        remaining -= n as u64;
    }
    sink.write_all(&checksums)?;
    sink.flush()
}

pub struct Envelope {
    file: File,
    len: u64,
    root: Range<u64>,
}

impl Envelope {
    pub fn open(file: File, kind: FileKind, expected_id: u64) -> io::Result<Self> {
        let mut h = [0u8; 64];
        file.read_exact_at(&mut h, 0)?;
        if &h[..8] != kind.magic()
            || !matches!(h[8..10], [1, 0] | [2, 0])
            || h[11..16] != [0, 64, 0, 0, 0]
            || h[48..52] != (BLOCK as u32).to_le_bytes()
            || h[52..60] != [0; 8]
            || h[60..] != crc32c(&h[..60]).to_le_bytes()
        {
            return Err(invalid());
        }
        let word = |i: usize| u64::from_le_bytes(h[i..][..8].try_into().unwrap());
        let len = word(24);
        let start = word(32);
        let root = start..start.checked_add(word(40)).ok_or_else(invalid)?;
        if expected_id == 0
            || word(16) != expected_id
            || h[10] != u8::from(kind == FileKind::Manifest && root == (0..264))
            || (h[8] == 2) != (kind == FileKind::Manifest && root == (0..312))
            || !valid_root(kind, &root, len)
            || physical_size(len)? != file.metadata()?.len()
        {
            return Err(invalid());
        }
        Ok(Self { file, len, root })
    }

    pub fn payload_len(&self) -> u64 {
        self.len
    }
    pub fn root(&self) -> Range<u64> {
        self.root.clone()
    }

    /// Deliver one verified borrowed slice per touched block. An error can
    /// follow earlier successful calls to the consumer.
    pub fn visit(
        &self,
        range: Range<u64>,
        mut consume: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if range.start > range.end || range.end > self.len {
            return Err(invalid());
        }
        if range.is_empty() {
            return Ok(());
        }
        let mut first = range.start / BLOCK;
        let stop = range.end.div_ceil(BLOCK);
        let batch = (stop - first).min(64);
        let mut bytes = vec![0; (batch * BLOCK) as usize];
        let mut checks = vec![0; (batch * 4) as usize];
        while first < stop {
            let count = (stop - first).min(batch);
            let start = first * BLOCK;
            let len = (self.len - start).min(count * BLOCK) as usize;
            let data = &mut bytes[..len];
            let hashes = &mut checks[..count as usize * 4];
            self.file.read_exact_at(data, HEADER + start)?;
            self.file
                .read_exact_at(hashes, HEADER + self.len + 4 * first)?;
            for (block, hash) in data.chunks(BLOCK as usize).zip(hashes.as_chunks::<4>().0) {
                if crc32c(block) != u32::from_le_bytes(*hash) {
                    return Err(invalid());
                }
            }
            consume(
                &data[range.start.saturating_sub(start) as usize
                    ..(range.end - start).min(len as u64) as usize],
            )?;
            first += count;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    #[test]
    fn detects_damage_only_in_visited_blocks() {
        let path = std::env::temp_dir().join(format!("codeindex-envelope-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(path).unwrap();
        let bytes = vec![42; 4097];
        write_envelope(&mut file, FileKind::Segment, 9, 0..0, 4097, &bytes[..]).unwrap();
        let pin = Envelope::open(file.try_clone().unwrap(), FileKind::Segment, 9).unwrap();
        let mut out = Vec::new();
        pin.visit(4095..4097, |b| out.write_all(b)).unwrap();
        assert_eq!(out, &[42, 42]);
        file.write_all_at(&[41], 64 + 4096).unwrap();
        pin.visit(0..2, |_| Ok(())).unwrap();
        assert!(pin.visit(4096..4097, |_| Ok(())).is_err());
    }
}

impl crate::application::index::layout::ExtentSink for SegmentWriter {
    fn offset(&self) -> u64 {
        self.offset()
    }
}
