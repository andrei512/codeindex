//! Worktree discovery and immutable captures, without spawning Git.
mod capture;
mod config;
mod digest;
mod ignore;
mod index;
mod inflate;
mod key;
mod provenance;
mod sparse;
mod walk_pool;
pub use capture::{capture, capture_observed};
pub(crate) use capture::CaptureRoot;
pub use key::cache_key;
pub use provenance::{BlobId, CommitTree, raw_blob_id, uses_sha256};
use std::{
    env,
    ffi::CString,
    fs, io,
    sync::Arc,
    path::{Path, PathBuf},
};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(&mut s, "{b:02x}").unwrap()
    }
    s
}
fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
fn optional_bytes(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
pub fn locate(cwd: &Path) -> io::Result<PathBuf> {
    find_worktree(cwd)?.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "outside a Git worktree"))
}
pub(crate) fn find_worktree(cwd: &Path) -> io::Result<Option<PathBuf>> {
    let path = cwd.canonicalize()?;
    for root in path.ancestors() {
        match fs::symlink_metadata(root.join(".git")) {
            Ok(m) if m.is_dir() || m.is_file() => return Ok(Some(root.to_owned())),
            Ok(_) => return Err(invalid("invalid .git marker")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}
fn add_rules(bytes: &[u8], base: usize, fold: bool, rules: &mut Vec<ignore::Rule>) {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    rules.extend(
        bytes
            .split(|&b| b == b'\n')
            .filter_map(|s| ignore::Rule::parse(s, base, fold)),
    );
}
pub use crate::domain::ObservedPath;

struct Tree<'a> {
    root: &'a Path,
    cache: Option<PathBuf>,
    tracked: index::Tracked,
    fold: bool,
    plain_hidden: Option<bool>,
}
struct Rules {
    parent: Option<Arc<Rules>>,
    local: Vec<ignore::Rule>,
}

impl Rules {
    fn excludes(&self, names: &[CString], is_directory: bool) -> bool {
        let mut frame = Some(self);
        while let Some(rules) = frame {
            if let Some(excluded) = rules.local.iter().rev()
                .find_map(|r| r.decision(names, is_directory))
            {
                return excluded;
            }
            frame = rules.parent.as_deref();
        }
        false
    }
}

struct Directory {
    rel: String,
    ignored: bool,
    rules: Arc<Rules>,
    names: Vec<CString>,
}
struct Walk<'a> {
    tree: &'a Tree<'a>,
    rules: Arc<Rules>,
    names: Vec<CString>,
    found: Vec<ObservedPath>,
}
impl Walk<'_> {
    fn visit(&mut self, rel: &str, ignored: bool, mut ready: Option<&mut Vec<Directory>>) -> io::Result<()> {
        let dir = self.tree.root.join(rel);
        let mut old_rules = None;
        let mut control = None;
        let control_name = if self.tree.plain_hidden.is_some() { ".ignore" } else { ".gitignore" };
        if !ignored {
            let ignore = dir.join(control_name);
            match fs::symlink_metadata(&ignore) {
                Ok(m) if m.is_file() => {
                    let bytes =
                        capture::read_control(self.tree.root, &Path::new(rel).join(control_name), &m)?;
                    let mut nested = Vec::new();
                    add_rules(&bytes, self.names.len(), self.tree.fold, &mut nested);
                    if !nested.is_empty() {
                        old_rules = Some(self.rules.clone());
                        self.rules = Arc::new(Rules { parent: old_rules.clone(), local: nested });
                    }
                    let text = if bytes.contains(&0) {
                        None
                    } else {
                        String::from_utf8(bytes).ok()
                    };
                    control = Some((capture::stamp(&m), text));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if self.tree.plain_hidden == Some(false) && name.starts_with('.') || name == ".git" || self.tree.fold && name.eq_ignore_ascii_case(".git") {
                continue;
            }
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if self.tree.tracked.gitlinks.contains(&path) {
                continue;
            }
            let ty = entry.file_type()?;
            let tracked = self.tree.tracked.paths.contains(&path);
            if self.tree.cache.as_deref() == Some(Path::new(&path)) {
                let prefix = format!("{path}/");
                if tracked
                    || self.tree
                        .tracked
                        .paths
                        .range(prefix.clone()..)
                        .next()
                        .is_some_and(|p| p.starts_with(&prefix))
                {
                    return Err(invalid("tracked file collides with Codeindex cache"));
                }
                continue;
            }
            let is_control = name == control_name;
            // Tracked leaves remain eligible under every ignore rule. Only directories
            // need a component on the stack while visiting their children.
            let match_rules = ty.is_dir() || !tracked;
            let excluded = if match_rules {
                self.names.push(CString::new(if self.tree.fold { name.to_ascii_lowercase() } else { name }).map_err(invalid)?);
                ignored || self.rules.excludes(&self.names, ty.is_dir())
            } else {
                false
            };
            if ty.is_dir() {
                let prefix = format!("{path}/");
                let tracked_child = || {
                    self.tree.tracked
                        .paths
                        .range(prefix.clone()..)
                        .next()
                        .is_some_and(|p| p.starts_with(&prefix))
                };
                if (!excluded || tracked_child())
                    && (self.tree.plain_hidden.is_some() || !entry.path().join(".git").try_exists()? || tracked_child())
                {
                    if let Some(jobs) = ready.as_deref_mut() {
                        jobs.push(Directory { rel: path, ignored: excluded, rules: self.rules.clone(), names: self.names.clone() });
                    } else {
                        self.visit(&path, excluded, None)?;
                    }
                }
            } else if (tracked || !excluded) && (ty.is_file() || self.tree.plain_hidden.is_none() && ty.is_symlink()) {
                let metadata = entry.metadata()?;
                let stamp = capture::stamp(&metadata);
                let captured = if is_control && let Some((read_stamp, text)) = control.take() {
                    if stamp != read_stamp {
                        return Err(io::Error::other("ignore file changed during discovery"));
                    }
                    Some(text)
                } else {
                    None
                };
                self.found.push(ObservedPath {
                    path,
                    stamp,
                    captured,
                });
            }
            if match_rules { self.names.pop(); }
        }
        if let Some(saved) = old_rules { self.rules = saved; }
        Ok(())
    }
}
fn git_dirs(root: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let marker = root.join(".git");
    let git = if marker.is_dir() {
        marker
    } else {
        let file = fs::read_to_string(marker)?;
        let gitdir = file
            .trim_end_matches(['\n', '\r'])
            .strip_prefix("gitdir: ")
            .ok_or_else(|| invalid("invalid gitdir marker"))?;
        root.join(gitdir).canonicalize()?
    };
    let common = match optional_bytes(&git.join("commondir"))? {
        Some(s) => git.join(
            std::str::from_utf8(&s)
                .map_err(invalid)?
                .trim_end_matches(['\n', '\r']),
        ),
        None => git.clone(),
    };
    Ok((git, common))
}
mod head;
pub use head::head_commit;
pub fn observe(root: &Path) -> io::Result<Vec<ObservedPath>> {
    let (git,common)=git_dirs(root)?;
    let conf = config::read(&common, &git)?;
    let hash_len = if conf.sha256 { 32 } else { 20 };
    let mut tracked = index::read(&git.join("index"), hash_len)?;
    sparse::expand(&mut tracked, root, &common, hash_len)?;
    let cache = env::var_os("HOME")
        .and_then(|home| Path::new(&home).canonicalize().ok())
        .map(|p| p.join(".codeindex/repos"))
        .and_then(|p| p.strip_prefix(&root).ok().map(Path::to_owned));
    let mut rules = vec![];
    for file in conf
        .excludes
        .into_iter()
        .chain([common.join("info/exclude")])
    {
        if let Some(bytes) = optional_bytes(&file)? {
            add_rules(&bytes, 0, conf.ignore_case, &mut rules)
        }
    }
    let tree = Tree { root, cache, tracked, fold: conf.ignore_case, plain_hidden: None };
    walk(tree, rules, Vec::new())
}

pub(crate) fn plain_paths(root: &Path, hidden: bool) -> io::Result<Vec<ObservedPath>> {
    // Anchored parent patterns retain their original base when a caller names
    // a subdirectory explicitly or starts the command there.
    let mut names = Vec::new();
    let mut rules = Vec::new();
    let ancestors: Vec<_> = root.ancestors().collect();
    for &dir in ancestors.iter().rev() {
        if let Some(name) = dir.file_name() {
            names.push(CString::new(name.as_encoded_bytes()).map_err(invalid)?);
        }
        if dir == root { break; }
        let ignore = dir.join(".ignore");
        match fs::symlink_metadata(&ignore) {
            Ok(m) if m.is_file() => {
                let bytes = capture::read_control(dir, Path::new(".ignore"), &m)?;
                add_rules(&bytes, names.len(), false, &mut rules);
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let tree = Tree { root, cache: None, tracked: Default::default(), fold: false, plain_hidden: Some(hidden) };
    walk(tree, rules, names)
}

fn walk(tree: Tree<'_>, rules: Vec<ignore::Rule>, names: Vec<CString>) -> io::Result<Vec<ObservedPath>> {
    let mut walk = Walk {
        tree: &tree, rules: Arc::new(Rules { parent: None, local: rules }), names, found: Vec::new(),
    };
    let workers = if tree.tracked.paths.len() >= 256 {
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(6)
    } else { 1 };
    if workers == 1 {
        walk.visit("", false, None)?;
    } else {
        let mut ready = Vec::new();
        walk.visit("", false, Some(&mut ready))?;
        if !ready.is_empty() {
            walk.found.extend(walk_pool::collect(&tree, ready, workers)?);
        }
    }
    walk.found.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(walk.found)
}

pub fn inventory(cwd: &Path) -> io::Result<(PathBuf, Vec<String>)> {
    let root = locate(cwd)?;
    let observations = observe(&root)?;
    Ok((root, observations.into_iter().map(|f| f.path).collect()))
}

#[cfg(test)]
mod tests;
