//! A packed index still sees one logical address space. A retained span names
//! immutable physical bytes directly, even after hundreds of publications.
use super::{Envelope, FileKind, SegmentWriter};
use crate::application::index::{build::MemoryBytes, layout::ExtentSink};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Write},
    ops::Range,
    path::Path,
};

pub(super) const LEGACY: u64 = 1 << 63;
const MAX_PIN_FILES: usize = 32;
const MAX_RETAINED_SPANS: usize = 16384;
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid snapshot extent map")
}

pub(super) fn name(key: u64) -> String {
    if key & LEGACY != 0 {
        format!("segment-{:016x}", key & !LEGACY)
    } else {
        format!("part-{key:016x}")
    }
}
#[derive(Clone, Debug)]
struct Piece {
    end: u64,
    key: u64,
    offset: u64,
}
fn logical_end(spans: &[Piece]) -> u64 {
    spans.last().map_or(0, |piece| piece.end)
}

#[derive(Clone, Default)]
pub(super) struct Map {
    spans: Vec<Piece>,
}
impl Map {
    pub fn legacy(id: u64, length: u64) -> io::Result<Self> {
        if id == 0 || id >= LEGACY {
            return Err(invalid());
        }
        let mut m = Self::default();
        m.push(id | LEGACY, 0, length)?;
        Ok(m)
    }
    fn len(&self) -> u64 {
        logical_end(&self.spans)
    }

    fn push(&mut self, key: u64, offset: u64, len: u64) -> io::Result<()> {
        if len == 0 {
            return Ok(());
        }
        let start = self.len();
        let end = start.checked_add(len).ok_or_else(invalid)?;
        offset.checked_add(len).ok_or_else(invalid)?;
        if let Some((last, earlier)) = self.spans.split_last_mut()
            && last.key == key
            && last.offset.checked_add(start - logical_end(earlier)) == Some(offset)
        {
            last.end = end;
        } else {
            self.spans.push(Piece { end, key, offset });
        }
        Ok(())
    }
    fn slices(
        &self,
        range: Range<u64>,
        mut use_bytes: impl FnMut(u64, Range<u64>) -> io::Result<()>,
    ) -> io::Result<()> {
        if range.start > range.end || range.end > self.len() {
            return Err(invalid());
        }
        let mut at = range.start;
        let first = self.spans.partition_point(|s| s.end <= at);
        let mut start = logical_end(&self.spans[..first]);
        for s in &self.spans[first..] {
            if at == range.end {
                break;
            }
            let until = range.end.min(s.end);
            let physical = s.offset + (at - start);
            use_bytes(s.key, physical..physical + (until - at))?;
            at = until;
            start = s.end;
        }
        Ok(())
    }
    pub fn append_rank(&mut self, generation: u64, range: Range<u64>) -> io::Result<()> {
        let key = rank_segment_key(generation)?;
        let length = self.checked_append(range)?;
        self.push(key, 0, length)
    }
    fn checked_append(&self, range: Range<u64>) -> io::Result<u64> {
        if self.len() != range.start || range.is_empty() {
            return Err(invalid());
        }
        Ok(range.end - range.start)
    }

    #[cfg(test)]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.encode_into(&mut bytes);
        bytes
    }
    pub(super) fn encode_into(&self, bytes: &mut Vec<u8>) {
        bytes.reserve(self.spans.len() * 24);
        for p in &self.spans {
            for v in [p.end, p.key, p.offset] {
                bytes.extend(v.to_le_bytes());
            }
        }
    }
    pub fn decode(bytes: &[u8], id: u64, len: u64) -> io::Result<Self> {
        let (rows, tail) = bytes.as_chunks::<24>();
        if !tail.is_empty() {
            return Err(invalid());
        }
        let mut m = Self::default();
        for r in rows {
            let word = |i| u64::from_le_bytes(r[i..i + 8].try_into().unwrap());
            let (end, key, offset) = (word(0), word(8), word(16));
            if end <= m.len() || !(1..=id).contains(&(key & !LEGACY)) {
                return Err(invalid());
            }
            m.push(key, offset, end - m.len())?;
        }
        if m.len() != len {
            return Err(invalid());
        }
        Ok(m)
    }
}

fn rank_segment_key(generation: u64) -> io::Result<u64> {
    (1..LEGACY).contains(&generation).then_some(LEGACY | generation).ok_or_else(invalid)
}

struct SegmentDescriptors<'a> {
    cache: &'a Path,
    retained: BTreeMap<u64, Envelope>,
    selected: BTreeMap<u64, Envelope>,
}

impl SegmentDescriptors<'_> {
    fn descriptor(&mut self, key: u64) -> io::Result<&Envelope> {
        use std::collections::btree_map::Entry;
        match self.selected.entry(key) {
            Entry::Occupied(slot) => Ok(slot.into_mut()),
            Entry::Vacant(slot) => {
                let envelope = self.retained.remove(&key)
                    .map_or_else(|| open_segment(self.cache, key), Ok)?;
                Ok(slot.insert(envelope))
            }
        }
    }
}

fn open_segment(cache: &Path, key: u64) -> io::Result<Envelope> {
    Envelope::open(File::open(cache.join(name(key)))?, FileKind::Segment, key & !LEGACY)
}

pub(super) type Retired = std::collections::btree_map::IntoKeys<u64, Envelope>;

pub(super) struct Segments {
    memory: Option<MemoryBytes>,
    pub map: Map,
    files: BTreeMap<u64, Envelope>,
    cache: std::sync::OnceLock<super::read_cache::ReadCache>,
}
impl Segments {
    pub(super) fn memory(bytes: MemoryBytes) -> Self {
        Self { memory: Some(bytes), map: Map::default(), files: Default::default(), cache: Default::default() }
    }
    pub fn open(cache: &Path, map: Map) -> io::Result<Self> {
        Self::replace(cache, map, None).map(|(selected, _)| selected)
    }

    /// Segment keys name immutable files. Transfer the predecessor's validated
    /// descriptors under the writer lease, opening only newly referenced parts.
    pub fn replace(cache: &Path, map: Map, previous: Option<Self>) -> io::Result<(Self, Retired)> {
        let mut descriptors = SegmentDescriptors {
            cache,
            retained: previous.map(|prior| prior.files).unwrap_or_default(),
            selected: BTreeMap::new(),
        };
        let mut prev = 0;
        for s in &map.spans {
            let len = s.end - prev;
            prev = s.end;
            let envelope = descriptors.descriptor(s.key)?;
            if s.offset.checked_add(len).is_none_or(|n| n > envelope.payload_len())
            {
                return Err(invalid());
            }
        }
        Ok((Self {
            memory: None,
            map,
            files: descriptors.selected,
            cache: Default::default(),
        }, descriptors.retained.into_keys()))
    }
    pub fn keys(&self) -> impl Iterator<Item = u64> + '_ {
        self.files.keys().copied()
    }
    pub(super) fn memory_range(&self, range: Range<u64>) -> io::Result<Option<&[u8]>> {
        let Some(bytes) = &self.memory else { return Ok(None) };
        let (head, tail) = bytes.slices(range)?;
        Ok(tail.is_empty().then_some(head))
    }
    pub fn visit_cached(
        &self,
        r: Range<u64>,
        mut consume: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.memory.is_some() || r.end.saturating_sub(r.start) > 32768 {
            return self.visit(r, consume);
        }
        let cache = self.cache.get_or_init(Default::default);
        self.map.slices(r, |key, physical| {
            cache.visit(key, &self.files[&key], physical, &mut consume)
        })
    }
    pub fn visit(
        &self,
        r: Range<u64>,
        mut consume: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Some(bytes) = &self.memory {
            let (head, tail) = bytes.slices(r)?;
            consume(head)?;
            if !tail.is_empty() { consume(tail)?; }
            return Ok(());
        }
        self.map.slices(r, |key, physical| {
            self.files[&key].visit(physical, &mut consume)
        })
    }
}

// Track ordinary writes as one span until a borrow changes physical backing.
pub(super) struct Writer<'a> {
    file: SegmentWriter,
    id: u64,
    prior: Option<&'a Segments>,
    evacuate: Option<u64>,
    map: Map,
    flushed: u64,
}
impl<'a> Writer<'a> {
    pub fn new(file: File, id: u64, prior: Option<&'a Segments>) -> io::Result<Self> {
        if id >= LEGACY {
            return Err(invalid());
        }
        let prior = prior.filter(|p| p.map.spans.len() < MAX_RETAINED_SPANS);
        let evacuate = prior.and_then(|p| {
            // If pins are at their handle budget, retire the cheapest live
            // segment. Rewrite only its remaining spans, never the whole index.
            let mut live = BTreeMap::<u64, u64>::new();
            let mut start = 0;
            for span in &p.map.spans {
                *live.entry(span.key).or_default() += span.end - start;
                start = span.end;
            }
            live.into_iter()
                .filter(|(key, n)| {
                    let size = p.files[key].payload_len();
                    p.files.len() >= MAX_PIN_FILES || (size > 1024 * 1024 && *n < size / 8)
                })
                .min_by_key(|(_, n)| *n)
                .map(|(k, _)| k)
        });
        Ok(Self {
            file: SegmentWriter::new(file)?,
            id,
            prior,
            evacuate,
            map: Map::default(),
            flushed: 0,
        })
    }
    fn seal_writes(&mut self) -> io::Result<()> {
        let end = self.file.offset();
        self.map.push(self.id, self.flushed, end - self.flushed)?;
        self.flushed = end;
        Ok(())
    }
    pub fn finish(mut self) -> io::Result<(File, Map)> {
        self.seal_writes()?;
        Ok((self.file.finish(self.id)?, self.map))
    }
}
impl Write for Writer<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.file.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl ExtentSink for Writer<'_> {
    fn offset(&self) -> u64 {
        self.map.len() + (self.file.offset() - self.flushed)
    }
    fn retain(&mut self, r: Range<u64>) -> io::Result<bool> {
        let Some(prior) = self.prior else {
            return Ok(false);
        };
        if r.end.checked_sub(r.start).ok_or_else(invalid)? < 256 {
            return Ok(false);
        }
        self.seal_writes()?;
        prior.map.slices(r, |key, physical| {
            if self.evacuate == Some(key) || self.map.spans.len() >= MAX_RETAINED_SPANS {
                prior.files[&key].visit(physical, |b| self.file.write_all(b))
            } else {
                self.seal_writes()?;
                self.map
                    .push(key, physical.start, physical.end - physical.start)
            }
        })?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_transfers_retained_file_descriptors() {
        use std::os::fd::AsRawFd;
        let dir = std::env::temp_dir();
        let path = dir.join(format!("cx-held-segment-{}",std::process::id()));
        let file = File::options().read(true).write(true).create_new(true).open(&path).unwrap();
        // Keep the temporary inode alive by descriptor alone.
        std::fs::remove_file(path).unwrap();
        let mut writer = SegmentWriter::new(file).unwrap();
        writer.write_all(b"kept").unwrap();
        let file = writer.finish(1).unwrap();
        let fd = file.as_raw_fd();
        let envelope = Envelope::open(file,FileKind::Segment,1).unwrap();
        let mut map = Map::default();
        map.push(1,0,4).unwrap();
        let old = Segments {
            files:BTreeMap::from([(1,envelope)]), map:map.clone(), memory:None, cache:Default::default()
        };
        let (mut retained,retired) = Segments::replace(&dir,map,Some(old)).unwrap();
        assert_eq!(retired.len(),0);
        let file=retained.files.remove(&1).unwrap().into_file();
        assert_eq!(file.as_raw_fd(),fd);
    }

    #[test]
    fn slicing_crosses_physical_files_and_coalesces_neighbors(){
        let mut map=Map::default();
        map.push(1,100,12).unwrap();map.push(1,112,8).unwrap();
        map.push(2,4,5).unwrap();map.push(1,0,3).unwrap();
        let recovered=Map::decode(&map.encode(),2,28).unwrap();
        assert_eq!(recovered.spans.len(),3);
        let mut slices=Vec::new();
        recovered.slices(18..27,|k,r|{slices.push((k,r));Ok(())}).unwrap();
        assert_eq!(slices,[(1,118..120),(2,4..9),(1,0..2)]);
        assert!(Map::decode(&map.encode(),1,28).is_err());
        assert!(Map::decode(&map.encode(),2,27).is_err());
    }
}
