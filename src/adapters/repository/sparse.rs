use super::{
    hex,
    index::{Tracked, number, take},
    inflate::zlib,
    invalid, optional_bytes,
};
use std::{
    collections::HashMap,
    fs, io,
    path::{Component, Path, PathBuf},
};
#[derive(Clone, Copy)]
pub(super) enum ObjectKind {
    Commit = 1,
    Tree = 2,
}
impl ObjectKind {
    fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Tree => "tree",
        }
    }
}
struct PackIndex<'a> {
    count: usize,
    keys: &'a [u8],
    offsets: &'a [u8],
    large: &'a [u8],
}
impl PackIndex<'_> {
    fn offset(&self, i: usize) -> io::Result<usize> {
        let n = number(&self.offsets[i * 4..i * 4 + 4]);
        if n & 0x8000_0000 == 0 {
            return Ok(n);
        }
        let p = (n & 0x7fff_ffff) * 8;
        Ok(number(
            self.large
                .get(p..p + 8)
                .ok_or_else(|| invalid("large offset out of bounds"))?,
        ))
    }
}

struct Pack {
    idx: std::cell::OnceCell<Vec<u8>>,
    path: PathBuf,
    by_offset: std::cell::OnceCell<Vec<usize>>,
}
pub(super) struct Objects {
    pack_files: std::cell::RefCell<Vec<(PathBuf, fs::File)>>,
    directories: Vec<PathBuf>,
    packs: Vec<Pack>,
    hash: usize,
}
fn alternate_directories(root: &Path) -> io::Result<Vec<PathBuf>> {
            let mut extra = vec![];
            if let Some(bytes) = optional_bytes(&root.join("info/alternates"))? {
                for path in bytes.split(|&b| b == b'\n').filter(|p| !p.is_empty()) {
                    use std::os::unix::ffi::OsStrExt;
                    extra.push(
                        root.join(std::ffi::OsStr::from_bytes(path))
                            .canonicalize()?,
                    );
                }
            }
    Ok(extra)
}
fn loose_payload(data: &[u8], expected: ObjectKind) -> io::Result<Vec<u8>> {
    let mut decoded = zlib(data, None)?;
    let end = decoded
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| invalid("malformed loose tree header"))?;
    let length = decoded.len() - end - 1;
    if decoded[..end] != *format!("{} {}", expected.name(), length).as_bytes() {
        return Err(invalid("invalid loose tree size or type"));
    }
    decoded.drain(..=end);
    Ok(decoded)
}
impl Objects {
    pub(super) fn new(common: &Path, hash: usize) -> io::Result<Self> {
        let mut dirs = vec![common.join("objects").canonicalize()?];
        let mut i = 0;
        let mut packs = vec![];
        while i < dirs.len() {
            let root = &dirs[i];
            let extra = alternate_directories(root)?;
            Pack::discover(root, &mut packs)?;
            for p in extra {
                if !dirs.contains(&p) {
                    dirs.push(p)
                }
            }
            i += 1;
        }
        Ok(Self {
            pack_files: Default::default(),
            directories: dirs,
            packs,
            hash,
        })
    }
    pub(super) fn read(&self, id: &[u8], depth: u8, expected: ObjectKind) -> io::Result<Vec<u8>> {
        if depth > 64 {
            return Err(invalid("Git tree delta depth limit"));
        }
        if let Some(data) = self.loose(id)? {
            return loose_payload(&data, expected);
        }
        if let Some((pack, offset)) = self.packed_location(id)? {
            return self.unpack(pack, offset, depth, expected);
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "missing sparse-index tree object",
        ))
    }

    fn loose(&self, id: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let name = hex(id);
        for dir in &self.directories {
            let data = optional_bytes(&dir.join(&name[..2]).join(&name[2..]))?;
            if data.is_some() {
                return Ok(data);
            }
        }
        Ok(None)
    }

    fn packed_location(&self, id: &[u8]) -> io::Result<Option<(&Pack, usize)>> {
        for pack in &self.packs {
            let offset = pack.offset(id)?;
            if offset.is_some() {
                return Ok(offset.map(|offset| (pack, offset)));
            }
        }
        Ok(None)
    }
    fn packed_bytes(&self, pack: &Pack, offset: usize) -> io::Result<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        // One cache per open commit tree. Cap descriptors across repositories with many packs.
        // Release the borrow before a delta recursively requests its base.

        let mut cache = self.pack_files.borrow_mut();
        let index = if let Some(i) = cache.iter().position(|(p, _)| p == &pack.path) {
            i
        } else {
            let file = fs::File::open(&pack.path)?;
            if cache.len() == 8 {
                cache.remove(0);
            }
            cache.push((pack.path.clone(), file));
            cache.len() - 1
        };
        let f = &cache[index].1;
        // Read only this object's compressed range. Pack indexes record every object offset.
        let end = match pack.next_offset(offset, self.hash)? {
            Some(next) => next,
            None => (f.metadata()?.len() as usize)
                .checked_sub(self.hash)
                .ok_or_else(|| invalid("short Git pack"))?,
        };
        let n = end
            .checked_sub(offset)
            .ok_or_else(|| invalid("invalid packed tree offset"))?;
        if n > 256 << 20 {
            return Err(invalid("Git tree size limit"));
        }
        let mut data = vec![0; n];
        f.read_exact_at(&mut data, offset as u64)?;
        Ok(data)
    }

    fn unpack(
        &self,
        pack: &Pack,
        offset: usize,
        depth: u8,
        expected: ObjectKind,
    ) -> io::Result<Vec<u8>> {
        if depth > 64 {
            return Err(invalid("Git tree delta depth limit"));
        }
        let data = self.packed_bytes(pack, offset)?;
        let mut data = &data[..];
        let (kind, size) = packed_header(&mut data)?;
        let base = match kind {
            k if k == expected as u8 => return zlib(data, Some(size)),
            6 => {
                let dist = base_distance(&mut data)?;
                self.unpack(
                    pack,
                    offset
                        .checked_sub(dist)
                        .ok_or_else(|| invalid("bad delta base"))?,
                    depth + 1,
                    expected,
                )?
            }
            7 => self.read(take(&mut data, self.hash)?, depth + 1, expected)?,
            _ => return Err(invalid("sparse index did not reference a tree")),
        };
        delta(&base, &zlib(data, Some(size))?)
    }
}
impl Pack {
    fn discover(root: &Path, packs: &mut Vec<Self>) -> io::Result<()> {
        let files = match fs::read_dir(root.join("pack")) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for file in files {
            packs.extend(Self::from_index(file?.path()));
        }
        Ok(())
    }

    fn from_index(path: PathBuf) -> Option<Self> {
        (path.extension()? == "idx").then(|| Self {
            idx: Default::default(),
            path: path.with_extension("pack"),
            by_offset: Default::default(),
        })
    }

    fn entries(&self, hash: usize) -> io::Result<PackIndex<'_>> {
        // Loose objects are common immediately after a commit. Open a pack index
        // only when a requested object was absent from the loose object store.
        if self.idx.get().is_none() {
            let data = fs::read(self.path.with_extension("idx"))?;
            let _ = self.idx.set(data);
        }
        let mut data = self.idx.get().unwrap().as_slice();
        if take(&mut data, 8)? != b"\xfftOc\0\0\0\x02" {
            return Err(invalid("unsupported Git pack-index version"));
        }
        let n = number(&take(&mut data, 1024)?[1020..]);
        let keys = take(&mut data, n * hash)?;
        take(&mut data, n * 4)?;
        let offsets = take(&mut data, n * 4)?;
        Ok(PackIndex {
            count: n,
            keys,
            offsets,
            large: data,
        })
    }
    fn offset(&self, id: &[u8]) -> io::Result<Option<usize>> {
        let index = self.entries(id.len())?;
        let (mut lo, mut hi) = (0, index.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let key = &index.keys[mid * id.len()..(mid + 1) * id.len()];
            match key.cmp(id) {
                std::cmp::Ordering::Equal => {
                    return index.offset(mid).map(Some);
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                _ => hi = mid,
            }
        }
        Ok(None)
    }
    fn next_offset(&self, current: usize, hash: usize) -> io::Result<Option<usize>> {
        if self.by_offset.get().is_none() {
            let index = self.entries(hash)?;
            let mut positions = (0..index.count)
                .map(|i| index.offset(i))
                .collect::<io::Result<Vec<_>>>()?;
            positions.sort_unstable();
            let _ = self.by_offset.set(positions);
        }
        let positions = self.by_offset.get().unwrap();
        let i = positions
            .binary_search(&current)
            .map_err(|_| invalid("missing Git pack delta base offset"))?;
        Ok(positions.get(i + 1).copied())
    }
}
fn packed_header(data: &mut &[u8]) -> io::Result<(u8, usize)> {
    let first = take(data, 1)?[0];
    let kind = (first >> 4) & 7;
    let mut size = usize::from(first & 15);
    let mut shift = 4;
    let mut byte = first;
    while byte & 128 != 0 {
        byte = take(data, 1)?[0];
        size |= usize::from(byte & 127)
            .checked_shl(shift)
            .ok_or_else(|| invalid("tree size overflow"))?;
        shift += 7
    }
    Ok((kind, size))
}
fn base_distance(data: &mut &[u8]) -> io::Result<usize> {
    let mut b = take(data, 1)?[0];
    let mut dist = usize::from(b & 127);
    while b & 128 != 0 {
        b = take(data, 1)?[0];
        dist = dist
            .checked_add(1)
            .and_then(|v| v.checked_mul(128))
            .and_then(|v| v.checked_add(usize::from(b & 127)))
            .ok_or_else(|| invalid("delta offset overflow"))?
    }
    if dist == 0 {
        return Err(invalid("zero delta distance"));
    }
    Ok(dist)
}
fn varint(data: &mut &[u8]) -> io::Result<usize> {
    let mut n = 0;
    for shift in (0..usize::BITS).step_by(7) {
        let b = take(data, 1)?[0];
        n |= usize::from(b & 127) << shift;
        if b & 128 == 0 {
            return Ok(n);
        }
    }
    Err(invalid("Git delta integer overflow"))
}
fn copied_bytes<'a>(base: &'a [u8], op: u8, commands: &mut &[u8]) -> io::Result<&'a [u8]> {
    let mut number = |shift: u32, n: u32| -> io::Result<usize> {
        let mut v = 0;
        for i in 0..n {
            if op & (1 << (shift + i)) != 0 {
                v |= usize::from(take(commands, 1)?[0]) << (8 * i)
            }
        }
        Ok(v)
    };
    let start = number(0, 4)?;
    let mut n = number(4, 3)?;
    if n == 0 {
        n = 65536
    }
    base.get(start..start + n)
        .ok_or_else(|| invalid("Git delta copy out of bounds"))
}
fn delta_instruction<'a>(base: &'a [u8], commands: &mut &'a [u8]) -> io::Result<&'a [u8]> {
    let op = take(commands, 1)?[0];
    match op {
        0 => Err(invalid("zero delta instruction")),
        1..=127 => take(commands, usize::from(op)),
        _ => copied_bytes(base, op, commands),
    }
}
fn delta<'a>(base: &'a [u8], mut commands: &'a [u8]) -> io::Result<Vec<u8>> {
    if varint(&mut commands)? != base.len() {
        return Err(invalid("Git delta base mismatch"));
    }
    let len = varint(&mut commands)?;
    if len > 256 << 20 {
        return Err(invalid("Git tree size limit"));
    }
    let mut result = Vec::with_capacity(len.min(8192));
    while !commands.is_empty() {
        let part = delta_instruction(base, &mut commands)?;
        if result.len() + part.len() > len {
            return Err(invalid("Git delta result overflow"));
        }
        result.extend_from_slice(part);
    }
    if result.len() != len {
        return Err(invalid("Git delta result truncated"));
    }
    Ok(result)
}
pub(super) struct TreeEntry<'a> {
    pub mode: &'a [u8],
    pub name: &'a [u8],
    pub oid: &'a [u8],
}

#[derive(Clone, Copy)]
pub(super) enum TreeFraming {
    Sparse,
    Provenance,
}

impl TreeFraming {
    fn mode(self) -> &'static str {
        match self {
            Self::Sparse => "bad sparse tree mode",
            Self::Provenance => "missing Git tree mode",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Sparse => "bad sparse tree name",
            Self::Provenance => "missing Git tree filename",
        }
    }

    fn validate_name(self, name: &[u8]) -> io::Result<()> {
        match self {
            Self::Provenance if matches!(name, b"" | b"." | b"..") || name.contains(&b'/') => {
                Err(invalid("unsafe Git tree entry"))
            }
            _ => Ok(()),
        }
    }
}

impl<'a> TreeEntry<'a> {
    pub(super) fn read(data: &mut &'a [u8], hash: usize, framing: TreeFraming) -> io::Result<Self> {
        let sep = data.iter().position(|&b| b == b' ').ok_or_else(|| invalid(framing.mode()))?;
        let mode = take(data, sep + 1)?;
        let end = data.iter().position(|&b| b == 0).ok_or_else(|| invalid(framing.name()))?;
        let name = take(data, end)?;
        framing.validate_name(name)?;
        take(data, 1)?;
        let oid = take(data, hash)?;
        Ok(Self { mode, name, oid })
    }
}

fn materialized(root: &Path, path: &str) -> io::Result<bool> {
    if path.is_empty()
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid("unsafe sparse index path"));
    }
    match fs::symlink_metadata(root.join(path)) {
        Ok(m) => Ok(m.is_dir()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub(super) fn expand(
    tracked: &mut Tracked,
    root: &Path,
    common: &Path,
    hash: usize,
) -> io::Result<()> {
    let mut queue = std::mem::take(&mut tracked.sparse);
    let mut cache: Option<TreeCache> = None;
    while let Some((path, id)) = queue.pop() {
        let path = path.trim_end_matches('/');
        if !materialized(root, path)? {
            continue;
        }
        // Avoid opening the object store until a sparse directory materializes.
        let cache = TreeCache::get_or_open(&mut cache, common, hash)?;
        cache.expand_tree(id, path, tracked, &mut queue)?;
    }
    Ok(())
}

struct TreeCache {
    objects: Objects,
    trees: HashMap<Vec<u8>, Vec<u8>>,
}
impl TreeCache {
    fn get_or_open<'a>(cache: &'a mut Option<Self>, common: &Path, hash: usize) -> io::Result<&'a mut Self> {
        match cache {
            Some(open) => Ok(open),
            slot @ None => Ok(slot.insert(Self {
                objects: Objects::new(common, hash)?,
                trees: HashMap::new(),
            })),
        }
    }

    fn expand_tree(
        &mut self,
        id: Vec<u8>,
        path: &str,
        tracked: &mut Tracked,
        queue: &mut Vec<(String, Vec<u8>)>,
    ) -> io::Result<()> {
        let hash = self.objects.hash;
        let mut data = self.tree(id)?;
        while !data.is_empty() {
            TreeEntry::read(&mut data, hash, TreeFraming::Sparse)?.track(path, tracked, queue)?;
        }
        Ok(())
    }

    fn tree(&mut self, id: Vec<u8>) -> io::Result<&[u8]> {
        match self.trees.entry(id) {
            std::collections::hash_map::Entry::Occupied(e) => Ok(e.into_mut()),
            std::collections::hash_map::Entry::Vacant(e) => {
                let bytes = self.objects.read(e.key(), 0, ObjectKind::Tree)?;
                Ok(e.insert(bytes))
            }
        }
    }
}

impl TreeEntry<'_> {
    fn track(
        self,
        path: &str,
        tracked: &mut Tracked,
        queue: &mut Vec<(String, Vec<u8>)>,
    ) -> io::Result<()> {
        let Self { mode, name, oid } = self;
        let Ok(name) = std::str::from_utf8(name) else {
            return Ok(());
        };
        if matches!(name, "" | "." | ".." | ".git") || name.contains('/') {
            return Err(invalid("unsafe sparse tree entry"));
        }
        let file = format!("{path}/{name}");
        match mode {
            b"40000 " => queue.push((file, oid.to_owned())),
            b"160000 " => {
                tracked.gitlinks.insert(file);
            }
            b"100644 " | b"100755 " | b"120000 " => {
                tracked.paths.insert(file);
            }
            _ => return Err(invalid("unknown sparse tree entry mode")),
        }
        Ok(())
    }
}
