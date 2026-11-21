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
struct Pack {
    idx: std::cell::OnceCell<Vec<u8>>,
    path: PathBuf,
    by_offset: std::cell::OnceCell<Vec<usize>>,
}
pub(super) struct Objects {
    directories: Vec<PathBuf>,
    packs: Vec<Pack>,
    hash: usize,
}
impl Objects {
    pub(super) fn new(common: &Path, hash: usize) -> io::Result<Self> {
        let mut dirs = vec![common.join("objects").canonicalize()?];
        let mut i = 0;
        let mut packs = vec![];
        while i < dirs.len() {
            let root = &dirs[i];
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
            match fs::read_dir(root.join("pack")) {
                Ok(files) => {
                    for e in files {
                        let p = e?.path();
                        if p.extension().is_some_and(|x| x == "idx") {
                            packs.push(Pack {
                                idx: Default::default(),
                                path: p.with_extension("pack"),
                                by_offset: Default::default(),
                            })
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            for p in extra {
                if !dirs.contains(&p) {
                    dirs.push(p)
                }
            }
            i += 1;
        }
        Ok(Self {
            directories: dirs,
            packs,
            hash,
        })
    }
    pub(super) fn read(&self, id: &[u8], depth: u8, expected: ObjectKind) -> io::Result<Vec<u8>> {
        if depth > 64 {
            return Err(invalid("Git tree delta depth limit"));
        }
        let name = hex(id);
        for dir in &self.directories {
            if let Some(data) = optional_bytes(&dir.join(&name[..2]).join(&name[2..]))? {
                let bytes = zlib(&data, None)?;
                let mut fields = bytes.splitn(2, |&b| b == 0);
                let head = fields.next().unwrap();
                let payload = fields
                    .next()
                    .ok_or_else(|| invalid("malformed loose tree header"))?;
                if head != format!("{} {}", expected.name(), payload.len()).as_bytes() {
                    return Err(invalid("invalid loose tree size or type"));
                }
                return Ok(payload.to_vec());
            }
        }
        for p in &self.packs {
            if let Some(offset) = p.offset(id)? {
                return self.unpack(p, offset, depth, expected);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "missing sparse-index tree object",
        ))
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
        use std::io::{Read, Seek, SeekFrom};
        let mut f = fs::File::open(&pack.path)?;
        f.seek(SeekFrom::Start(offset as u64))?;
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
        f.read_exact(&mut data)?;
        let mut data = &data[..];
        let first = take(&mut data, 1)?[0];
        let kind = (first >> 4) & 7;
        let mut size = usize::from(first & 15);
        let mut shift = 4;
        let mut byte = first;
        while byte & 128 != 0 {
            byte = take(&mut data, 1)?[0];
            size |= usize::from(byte & 127)
                .checked_shl(shift)
                .ok_or_else(|| invalid("tree size overflow"))?;
            shift += 7
        }
        let base = match kind {
            k if k == expected as u8 => return zlib(data, Some(size)),
            6 => {
                let mut b = take(&mut data, 1)?[0];
                let mut dist = usize::from(b & 127);
                while b & 128 != 0 {
                    b = take(&mut data, 1)?[0];
                    dist = dist
                        .checked_add(1)
                        .and_then(|v| v.checked_mul(128))
                        .and_then(|v| v.checked_add(usize::from(b & 127)))
                        .ok_or_else(|| invalid("delta offset overflow"))?
                }
                if dist == 0 {
                    return Err(invalid("zero delta distance"));
                }
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
    fn entries(&self, hash: usize) -> io::Result<(usize, &[u8], &[u8], &[u8])> {
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
        Ok((n, keys, offsets, data))
    }
    fn decode_offset(i: usize, offsets: &[u8], large: &[u8]) -> io::Result<usize> {
        let n = number(&offsets[i * 4..i * 4 + 4]);
        if n & 0x8000_0000 == 0 {
            return Ok(n);
        }
        let p = (n & 0x7fff_ffff) * 8;
        Ok(number(
            large
                .get(p..p + 8)
                .ok_or_else(|| invalid("large offset out of bounds"))?,
        ))
    }
    fn offset(&self, id: &[u8]) -> io::Result<Option<usize>> {
        let (n, keys, offsets, large) = self.entries(id.len())?;
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let key = &keys[mid * id.len()..(mid + 1) * id.len()];
            match key.cmp(id) {
                std::cmp::Ordering::Equal => {
                    return Self::decode_offset(mid, offsets, large).map(Some);
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                _ => hi = mid,
            }
        }
        Ok(None)
    }
    fn next_offset(&self, current: usize, hash: usize) -> io::Result<Option<usize>> {
        if self.by_offset.get().is_none() {
            let (n, _, offsets, large) = self.entries(hash)?;
            let mut positions = (0..n)
                .map(|i| Self::decode_offset(i, offsets, large))
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
fn delta(base: &[u8], mut commands: &[u8]) -> io::Result<Vec<u8>> {
    if varint(&mut commands)? != base.len() {
        return Err(invalid("Git delta base mismatch"));
    }
    let len = varint(&mut commands)?;
    if len > 256 << 20 {
        return Err(invalid("Git tree size limit"));
    }
    let mut result = Vec::with_capacity(len.min(8192));
    while !commands.is_empty() {
        let op = take(&mut commands, 1)?[0];
        let part = if op & 128 != 0 {
            let mut number = |shift: u32, n: u32| -> io::Result<usize> {
                let mut v = 0;
                for i in 0..n {
                    if op & (1 << (shift + i)) != 0 {
                        v |= usize::from(take(&mut commands, 1)?[0]) << (8 * i)
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
                .ok_or_else(|| invalid("Git delta copy out of bounds"))?
        } else if op > 0 {
            take(&mut commands, usize::from(op))?
        } else {
            return Err(invalid("zero delta instruction"));
        };
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
pub(super) fn expand(
    tracked: &mut Tracked,
    root: &Path,
    common: &Path,
    hash: usize,
) -> io::Result<()> {
    let mut queue = std::mem::take(&mut tracked.sparse);
    let mut objects = None;
    let mut trees = HashMap::new();
    while let Some((path, id)) = queue.pop() {
        let path = path.trim_end_matches('/');
        if path.is_empty()
            || Path::new(path)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(invalid("unsafe sparse index path"));
        }
        match fs::symlink_metadata(root.join(path)) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
        if objects.is_none() {
            objects = Some(Objects::new(common, hash)?)
        }
        if !trees.contains_key(&id) {
            trees.insert(
                id.clone(),
                objects.as_ref().unwrap().read(&id, 0, ObjectKind::Tree)?,
            );
        }
        let mut data = trees[&id].as_slice();
        while !data.is_empty() {
            let sep = data
                .iter()
                .position(|&b| b == b' ')
                .ok_or_else(|| invalid("bad sparse tree mode"))?;
            let mode = take(&mut data, sep + 1)?;
            let end = data
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| invalid("bad sparse tree name"))?;
            let name = take(&mut data, end)?;
            take(&mut data, 1)?;
            let oid = take(&mut data, hash)?;
            let Ok(name) = std::str::from_utf8(name) else {
                continue;
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
        }
    }
    Ok(())
}
