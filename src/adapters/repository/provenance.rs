//! The commit tree supplies candidate identities. Working bytes are captured separately.
use super::{
    config, digest, git_dirs,
    invalid,
    sparse::{ObjectKind, Objects, TreeEntry, TreeFraming},
};
use std::{
    collections::HashMap,
    io::{self, Write},
    path::Path,
};

pub use crate::domain::BlobId;

impl BlobId {
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

impl Entry {
    fn regular(self) -> Option<BlobId> {
        if let Self::Regular(id) = self { Some(id) } else { None }
    }
    fn directory(self) -> Option<BlobId> {
        if let Self::Directory(id) = self { Some(id) } else { None }
    }
}

#[derive(Default)]
struct TreeChanges {
    files: HashMap<String, Option<BlobId>>,
    pending: Vec<(String, Option<BlobId>, Option<BlobId>)>,
}

impl TreeChanges {
    fn compare(&mut self, prefix: &str, old: Option<&Directory>, new: Option<&Directory>) {
        let before = old.into_iter().flatten();
        for (name, &entry) in before {
            self.record(prefix, name, Some(entry), new.and_then(|d| d.get(name)).copied());
        }
        let added = new.into_iter().flatten().filter(|(name, _)| old.is_none_or(|d| !d.contains_key(*name)));
        for (name, &entry) in added {
            self.record(prefix, name, None, Some(entry));
        }
    }

    fn record(&mut self, prefix: &str, name: &[u8], old: Option<Entry>, new: Option<Entry>) {
        if old == new { return; }
        let Ok(name) = std::str::from_utf8(name) else { return };
        let path = if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") };
        let before = old.and_then(Entry::directory);
        let after = new.and_then(Entry::directory);
        let blob = new.and_then(Entry::regular);
        // Only a file/directory replacement needs the path in both collections.
        if before == after {
            self.files.insert(path, blob);
        } else {
            if old.and_then(Entry::regular) != blob {
                self.files.insert(path.clone(), blob);
            }
            self.pending.push((path, before, after));
        }
    }
}

type Directory = HashMap<Box<[u8]>, Entry>;

pub struct CommitTree {
    last_directory: Option<(String, Option<BlobId>)>,
    objects: Objects,
    root: BlobId,
    directories: HashMap<BlobId, Directory>,
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
            last_directory: None,
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
        let mut changes = TreeChanges::default();
        changes.pending.push((String::new(), Some(old_root), Some(self.root)));
        while let Some((prefix, old, new)) = changes.pending.pop() {
            if old == new {
                continue;
            }
            self.load_roots([old, new])?;
            let old = old.and_then(|id| self.directories.get(&id));
            let new = new.and_then(|id| self.directories.get(&id));
            changes.compare(&prefix, old, new);
        }
        Ok(changes.files)
    }

    fn load_roots(&mut self, roots: [Option<BlobId>; 2]) -> io::Result<()> {
        roots.into_iter().flatten().try_for_each(|id| self.load(id))
    }

    fn parent_tree(&mut self, parent: &str) -> io::Result<Option<BlobId>> {
        if let Some((_, id)) = self.last_directory.as_ref().filter(|(p, _)| p == parent) {
            return Ok(*id);
        }
        let mut tree = Some(self.root);
        if !parent.is_empty() {
            for component in parent.split('/') {
                let Some(id) = tree else { break };
                self.load(id)?;
                tree = self.directories[&id].get(component.as_bytes()).copied().and_then(Entry::directory);
            }
        }
        let (last, id) = self.last_directory.get_or_insert_with(Default::default);
        last.clear();
        last.push_str(parent);
        *id = tree;
        Ok(tree)
    }

    pub fn blob(&mut self, path: &str) -> io::Result<Option<BlobId>> {
        if path.starts_with('/') {
            return Ok(None);
        }
        let (parent, leaf) = path.rsplit_once('/').unwrap_or(("", path));
        let tree = self.parent_tree(parent)?;
        let Some(tree) = tree else { return Ok(None) };
        self.load(tree)?;
        Ok(self.directories[&tree].get(leaf.as_bytes()).copied().and_then(Entry::regular))
    }
}

fn directory(mut bytes: &[u8], hash: usize) -> io::Result<Directory> {
    let mut entries = HashMap::new();
    while !bytes.is_empty() {
        let TreeEntry { mode, name, oid } = TreeEntry::read(&mut bytes, hash, TreeFraming::Provenance)?;
        insert_entry(&mut entries, name, provenance_entry(mode, oid)?)?;
    }
    Ok(entries)
}


fn insert_entry(entries: &mut Directory, name: &[u8], entry: Option<Entry>) -> io::Result<()> {
    let Some(entry) = entry else { return Ok(()) };
    if entries.insert(name.into(), entry).is_some() {
        return Err(invalid("duplicate Git tree filename"));
    }
    Ok(())
}

fn provenance_entry(mode: &[u8], oid: &[u8]) -> io::Result<Option<Entry>> {
    let id = BlobId::from_raw(oid);
    match mode {
        b"40000 " => Ok(Some(Entry::Directory(id))),
        b"100644 " | b"100755 " => Ok(Some(Entry::Regular(id))),
        b"120000 " | b"160000 " => Ok(None),
        _ => Err(invalid("invalid Git tree mode")),
    }
}
