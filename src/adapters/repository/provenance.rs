//! The commit tree supplies candidate identities. Working bytes are captured separately.
use super::{
    config, digest, git_dirs,
    index::take,
    invalid,
    sparse::{ObjectKind, Objects},
};
use std::{
    collections::HashMap,
    io::{self, Write},
    path::Path,
};

pub use crate::domain::BlobId;

impl BlobId {
    fn from_raw(raw: &[u8]) -> Self {
        let mut bytes = [0; 32];
        bytes[..raw.len()].copy_from_slice(raw);
        Self {
            bytes,
            len: raw.len() as u8,
        }
    }

    fn from_hex(raw: &[u8]) -> io::Result<Self> {
        if !matches!(raw.len(), 40 | 64) {
            return Err(invalid("invalid Git object ID length"));
        }
        let mut id = Self {
            bytes: [0; 32],
            len: (raw.len() / 2) as u8,
        };
        for (out, pair) in id.bytes.iter_mut().zip(raw.chunks_exact(2)) {
            let ascii = std::str::from_utf8(pair).map_err(invalid)?;
            *out = u8::from_str_radix(ascii, 16).map_err(invalid)?;
        }
        Ok(id)
    }


}

pub fn uses_sha256(root: &Path) -> io::Result<bool> {
    let (git, common) = git_dirs(root)?;
    Ok(config::read(&common, &git)?.sha256)
}
pub fn raw_blob_id(bytes: &[u8], sha256: bool) -> BlobId {
    object_id("blob", bytes, sha256)
}

fn object_id(kind: &str, bytes: &[u8], sha256: bool) -> BlobId {
    let mut header = [0; 32];
    let mut out = &mut header[..];
    write!(&mut out, "{kind} {}\0", bytes.len()).expect("Git blob header fits");
    let used = 32 - out.len();
    digest::digest(&header[..used], bytes, sha256)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Entry {
    Directory(BlobId),
    Regular(BlobId),
}

pub struct CommitTree {
    objects: Objects,
    root: BlobId,
    directories: HashMap<BlobId, HashMap<Box<[u8]>, Entry>>,
}

fn commit_root(objects: &Objects, commit: BlobId) -> io::Result<BlobId> {
    let bytes = objects.read(commit.as_bytes(), 0, ObjectKind::Commit)?;
    if object_id("commit", &bytes, commit.len == 32) != commit {
        return Err(invalid("Git commit content identity mismatch"));
    }
    let tree = bytes
        .split(|&b| b == b'\n')
        .next()
        .and_then(|line| line.strip_prefix(b"tree "))
        .ok_or_else(|| invalid("missing commit tree"))?;
    let root = BlobId::from_hex(tree)?;
    if root.len != commit.len {
        return Err(invalid("invalid commit tree ID length"));
    }
    Ok(root)
}

impl CommitTree {
    pub fn open(root: &Path, commit_hex: &str) -> io::Result<Self> {
        let (_, common) = git_dirs(root)?;
        let commit = BlobId::from_hex(commit_hex.as_bytes())?;
        let objects = Objects::new(&common, commit.len as usize)?;
        let root = commit_root(&objects, commit)?;
        Ok(Self {
            objects,
            root,
            directories: HashMap::new(),
        })
    }

    fn load(&mut self, tree: BlobId) -> io::Result<()> {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.directories.entry(tree) {
            let bytes = self.objects.read(tree.as_bytes(), 0, ObjectKind::Tree)?;
            if object_id("tree", &bytes, tree.len == 32) != tree {
                return Err(invalid("Git tree content identity mismatch"));
            }
            slot.insert(directory(&bytes, tree.len as usize)?);
        }
        Ok(())
    }

    /// Regular blob identities for paths whose committed bytes changed. A missing
    /// map entry means both commits have the same identity (including absence).
    pub fn changed_paths(
        &mut self,
        previous_commit: &str,
    ) -> io::Result<HashMap<String, Option<BlobId>>> {
        let previous = BlobId::from_hex(previous_commit.as_bytes())?;
        if previous.len != self.root.len {
            return Err(invalid("Git object format changed"));
        }
        let old_root = commit_root(&self.objects, previous)?;
        let mut result = HashMap::new();
        let mut pending = vec![(String::new(), Some(old_root), Some(self.root))];
        while let Some((prefix, old, new)) = pending.pop() {
            if old == new {
                continue;
            }
            for id in [old, new].into_iter().flatten() {
                self.load(id)?;
            }
            let old = old.and_then(|id| self.directories.get(&id));
            let new = new.and_then(|id| self.directories.get(&id));
            let mut compare = |name: &[u8], old: Option<Entry>, new: Option<Entry>| {
                if old == new {
                    return;
                }
                let Ok(name) = std::str::from_utf8(name) else {
                    return;
                };
                let path = if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}/{name}")
                };
                let blob = |entry| match entry {
                    Some(Entry::Regular(id)) => Some(id),
                    _ => None,
                };
                let dir = |entry| match entry {
                    Some(Entry::Directory(id)) => Some(id),
                    _ => None,
                };
                if blob(old) != blob(new) {
                    result.insert(path.clone(), blob(new));
                }
                if dir(old) != dir(new) {
                    pending.push((path, dir(old), dir(new)));
                }
            };
            if let Some(before) = old {
                for (name, entry) in before {
                    compare(name, Some(*entry), new.and_then(|d| d.get(name)).copied());
                }
            }
            if let Some(after) = new {
                for (name, entry) in after {
                    if old.is_none_or(|d| !d.contains_key(name)) {
                        compare(name, None, Some(*entry));
                    }
                }
            }
        }
        Ok(result)
    }

    pub fn blob(&mut self, path: &str) -> io::Result<Option<BlobId>> {
        let mut tree = self.root;
        let mut segments = path.as_bytes().split(|&b| b == b'/').peekable();
        while let Some(segment) = segments.next() {
            self.load(tree)?;
            match self.directories[&tree].get(segment) {
                Some(Entry::Directory(next)) if segments.peek().is_some() => tree = *next,
                Some(Entry::Regular(id)) if segments.peek().is_none() => return Ok(Some(*id)),
                _ => return Ok(None),
            }
        }
        Ok(None)
    }
}

fn directory(mut bytes: &[u8], hash: usize) -> io::Result<HashMap<Box<[u8]>, Entry>> {
    let mut entries = HashMap::new();
    while !bytes.is_empty() {
        let space = bytes
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| invalid("missing Git tree mode"))?;
        let mode = take(&mut bytes, space + 1)?;
        let nul = bytes
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| invalid("missing Git tree filename"))?;
        let name = take(&mut bytes, nul)?;
        if matches!(name, b"" | b"." | b"..") || name.contains(&b'/') {
            return Err(invalid("unsafe Git tree entry"));
        }
        take(&mut bytes, 1)?;
        let id = BlobId::from_raw(take(&mut bytes, hash)?);
        let entry = match mode {
            b"40000 " => Entry::Directory(id),
            b"100644 " | b"100755 " => Entry::Regular(id),
            b"120000 " | b"160000 " => continue,
            _ => return Err(invalid("invalid Git tree mode")),
        };
        if entries.insert(name.into(), entry).is_some() {
            return Err(invalid("duplicate Git tree filename"));
        }
    }
    Ok(entries)
}
