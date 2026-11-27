use super::crc::crc32c;
use std::{
    fs::File,
    io::{self, BufWriter, Read, Seek, Write},
    ops::Range,
    os::unix::fs::FileExt,
};

const HEADER: u64 = 64;
pub(super) const BLOCK: u64 = 4096;

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

/// Metadata for the envelope and its declared payload length.
pub struct EnvelopeHeader {
    pub kind: FileKind,
    pub generation: u64,
    pub root: Range<u64>,
    pub len: u64,
}

impl EnvelopeHeader {
    fn version(&self) -> (u8, u8) {
        match (self.kind, &self.root) {
            (FileKind::Manifest, Range { start: 0, end: 312 }) => (2, 0),
            (FileKind::Manifest, Range { start: 0, end: 264 }) => (1, 1),
            _ => (1, 0),
        }
    }

    fn valid(&self) -> bool {
        self.generation != 0 && valid_root(self.kind, &self.root, self.len)
    }

    fn encode(&self) -> [u8; 64] {
        let Self {
            kind,
            generation,
            root,
            len,
        } = self;
        let mut h = [0u8; 64];
        h[..8].copy_from_slice(kind.magic());
        (h[8], h[10]) = self.version();
        h[12..14].copy_from_slice(&64u16.to_le_bytes());
        h[16..24].copy_from_slice(&generation.to_le_bytes());
        h[24..32].copy_from_slice(&len.to_le_bytes());
        h[32..40].copy_from_slice(&root.start.to_le_bytes());
        h[40..48].copy_from_slice(&(root.end - root.start).to_le_bytes());
        h[48..52].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        let crc = crc32c(&h[..60]);
        h[60..].copy_from_slice(&crc.to_le_bytes());
        h
    }
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
        let header = EnvelopeHeader {
            kind: FileKind::Segment,
            generation: id,
            root: 0..0,
            len: self.length,
        };
        self.sink.write_all(&header.encode())?;
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
    header: EnvelopeHeader,
    mut input: impl Read,
) -> io::Result<()> {
    let len = header.len;
    physical_size(len)?;
    if !header.valid() {
        return Err(invalid());
    }
    if file.metadata()?.len() != 0 || file.stream_position()? != 0 {
        return Err(invalid());
    }
    let h = header.encode();
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

fn validate_fixed_header(kind: FileKind, bytes: &[u8; 64]) -> io::Result<()> {
    let magic = &bytes[..8] == kind.magic();
    let version = matches!(bytes[8..10], [1, 0] | [2, 0]);
    let geometry =
        bytes[11..16] == [0, 64, 0, 0, 0] && bytes[48..52] == (BLOCK as u32).to_le_bytes();
    let reserved = bytes[52..60] == [0; 8];
    if ![magic, version, geometry, reserved]
        .into_iter()
        .all(|valid| valid)
    {
        return Err(invalid());
    }
    if bytes[60..] != crc32c(&bytes[..60]).to_le_bytes() {
        return Err(invalid());
    }
    Ok(())
}

pub struct Envelope {
    file: File,
    len: u64,
    root: Range<u64>,
}

impl Envelope {
    pub fn open(file: File, kind: FileKind, expected_id: u64) -> io::Result<Self> {
        let (envelope, generation) = Self::read_header(file, kind)?;
        if generation != expected_id {
            return Err(invalid());
        }
        Ok(envelope)
    }

    pub(super) fn open_manifest(file: File) -> io::Result<(Self, u64)> {
        Self::read_header(file, FileKind::Manifest)
    }

    /// Read the selector directly into the buffer its table views will borrow.
    pub(super) fn manifest_payload(&self) -> io::Result<Vec<u8>> {
        if self.root.start != 0 || !matches!(self.root.end, 240 | 264 | 312) {
            return Err(invalid());
        }
        let len = usize::try_from(self.len).map_err(|_| invalid())?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len).map_err(io::Error::other)?;
        let mut hashes = [0; 64 * 4];
        while bytes.len() < len {
            let start = bytes.len();
            let n = (len - start).min(64 * BLOCK as usize);
            bytes.resize(start + n, 0);
            let checks = n.div_ceil(BLOCK as usize) * 4;
            self.read_blocks(
                start as u64 / BLOCK,
                &mut bytes[start..],
                &mut hashes[..checks],
            )?;
        }
        Ok(bytes)
    }

    pub(super) fn into_file(self) -> File {
        self.file
    }

    fn read_header(file: File, kind: FileKind) -> io::Result<(Self, u64)> {
        let mut h = [0u8; 64];
        file.read_exact_at(&mut h, 0)?;
        validate_fixed_header(kind, &h)?;
        let word = |i: usize| u64::from_le_bytes(h[i..][..8].try_into().unwrap());
        let len = word(24);
        let start = word(32);
        let root = start..start.checked_add(word(40)).ok_or_else(invalid)?;
        let generation = word(16);
        let header = EnvelopeHeader {
            kind,
            generation,
            root,
            len,
        };
        if !header.valid() || header.version() != (h[8], h[10]) {
            return Err(invalid());
        }
        if physical_size(len)? != file.metadata()?.len() {
            return Err(invalid());
        }
        Ok((
            Self {
                file,
                len,
                root: header.root,
            },
            generation,
        ))
    }

    pub fn payload_len(&self) -> u64 {
        self.len
    }
    pub fn root(&self) -> Range<u64> {
        self.root.clone()
    }

    // Callers supply complete checksum blocks, including a short final block.
    fn read_blocks(&self, first: u64, data: &mut [u8], hashes: &mut [u8]) -> io::Result<()> {
        debug_assert_eq!(data.len().div_ceil(BLOCK as usize) * 4, hashes.len());
        self.file.read_exact_at(data, HEADER + first * BLOCK)?;
        self.file
            .read_exact_at(hashes, HEADER + self.len + 4 * first)?;
        for (block, hash) in data.chunks(BLOCK as usize).zip(hashes.as_chunks::<4>().0) {
            if crc32c(block) != u32::from_le_bytes(*hash) {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub(super) fn read_block(&self, block: u64, bytes: &mut Vec<u8>) -> io::Result<()> {
        let begin = block
            .checked_mul(BLOCK)
            .filter(|&at| at < self.len)
            .ok_or_else(invalid)?;
        bytes.resize((self.len - begin).min(BLOCK) as usize, 0);
        self.read_blocks(block, bytes, &mut [0; 4])
    }

    /// Deliver checksum-verified bytes in batches. A later batch can fail
    /// after the consumer has received earlier bytes.
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
            self.read_blocks(first, data, hashes)?;
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
    fn manifest_payload_checks_every_batch_including_the_short_tail() {
        let path =
            std::env::temp_dir().join(format!("codeindex-manifest-batches-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(path).unwrap();
        let data = (0..64 * BLOCK as usize + 19)
            .map(|i| i as u8)
            .collect::<Vec<_>>();
        write_envelope(
            &mut file,
            EnvelopeHeader {
                kind: FileKind::Manifest,
                generation: 1,
                root: 0..312,
                len: data.len() as u64,
            },
            &data[..],
        )
        .unwrap();
        let (manifest, _) = Envelope::open_manifest(file.try_clone().unwrap()).unwrap();
        assert_eq!(manifest.manifest_payload().unwrap(), data);
        let tail = data.len() - 1;
        file.write_all_at(&[!data[tail]], HEADER + tail as u64)
            .unwrap();
        assert!(manifest.manifest_payload().is_err());
    }

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
        write_envelope(
            &mut file,
            EnvelopeHeader {
                kind: FileKind::Segment,
                generation: 9,
                root: 0..0,
                len: bytes.len() as u64,
            },
            &bytes[..],
        )
        .unwrap();
        let pin = Envelope::open(file.try_clone().unwrap(), FileKind::Segment, 9).unwrap();
        let mut out = Vec::new();
        pin.visit(4095..4097, |b| out.write_all(b)).unwrap();
        assert_eq!(out, &[42, 42]);
        let mut page = Vec::new();
        pin.read_block(0, &mut page).unwrap();
        assert_eq!(page, bytes[..4096]);
        pin.read_block(1, &mut page).unwrap();
        assert_eq!(page, bytes[4096..]);
        file.write_all_at(&[41], 64 + 4096).unwrap();
        assert!(pin.read_block(1, &mut page).is_err());
        pin.visit(0..2, |_| Ok(())).unwrap();
        assert!(pin.visit(4096..4097, |_| Ok(())).is_err());
    }
}

impl crate::application::index::layout::ExtentSink for SegmentWriter {
    fn offset(&self) -> u64 {
        self.offset()
    }
}
