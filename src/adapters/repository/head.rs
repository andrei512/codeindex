//! Read checkout provenance directly from the files-backed Git ref store.
use super::{git_dirs, invalid, optional_bytes};
use std::{
    io,
    path::{Component, Path},
};
fn oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn packed_ref(common: &Path, name: &str) -> io::Result<Option<String>> {
    let Some(bytes) = optional_bytes(&common.join("packed-refs"))? else {
        return Ok(None);
    };
    let matched = bytes.split(|&b| b == b'\n').find_map(|line| {
        let separator = line.iter().position(|&b| b == b' ')?;
        let (id, reference) = (&line[..separator], &line[separator + 1..]);
        (reference == name.as_bytes()).then_some(id)
    });
    let Some(id) = matched else {
        return Ok(None);
    };
    let id = std::str::from_utf8(id).map_err(invalid)?;
    if !oid(id) {
        return Err(invalid("invalid packed Git object ID"));
    }
    Ok(Some(id.to_ascii_lowercase()))
}

fn ref_name(s: &str) -> io::Result<&str> {
    s.strip_prefix("ref: ")
        .filter(|name| {
            name.starts_with("refs/")
                && Path::new(name)
                    .components()
                    .all(|p| matches!(p, Component::Normal(_)))
        })
        .ok_or_else(|| invalid("invalid Git HEAD reference"))
}

enum Reference<'a> {
    Commit(&'a str),
    Symbolic(&'a str),
}

impl<'a> Reference<'a> {
    fn parse(value: &'a str) -> io::Result<Self> {
        let value = value.trim_end();
        if oid(value) {
            return Ok(Self::Commit(value));
        }
        Ok(Self::Symbolic(ref_name(value)?))
    }
}

struct RefStore {
    git: std::path::PathBuf,
    common: std::path::PathBuf,
}

impl RefStore {
    fn resolve(&self, value: String, remaining: usize) -> io::Result<Option<String>> {
        if remaining == 0 {
            return Err(invalid("cyclic Git HEAD reference"));
        }
        match Reference::parse(&value)? {
            Reference::Commit(id) => Ok(Some(id.to_ascii_lowercase())),
            Reference::Symbolic(name) => self.resolve_symbolic(name, remaining),
        }
    }

    fn resolve_symbolic(&self, name: &str, remaining: usize) -> io::Result<Option<String>> {
        let Some(bytes) = self.loose(name)? else {
            return packed_ref(&self.common, name);
        };
        self.resolve(String::from_utf8(bytes).map_err(invalid)?, remaining - 1)
    }

    fn loose(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        let worktree = ["refs/worktree/", "refs/bisect/", "refs/rewritten/"]
            .iter().any(|prefix| name.starts_with(prefix));
        let dir = if worktree { &self.git } else { &self.common };
        optional_bytes(&dir.join(name))
    }
}

pub fn head_commit(root: &Path) -> io::Result<Option<String>> {
    let (git, common) = git_dirs(root)?;
    let Some(head) = optional_bytes(&git.join("HEAD"))? else {
        return Ok(None);
    };
    RefStore { git, common }.resolve(String::from_utf8(head).map_err(invalid)?, 16)
}
