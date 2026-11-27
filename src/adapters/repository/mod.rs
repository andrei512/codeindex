//! Worktree discovery and immutable captures, without spawning Git.
mod capture;
mod config;
mod digest;
mod ignore;
mod index;
mod inflate;
mod key;
mod provenance;
pub(crate) mod search_paths;
mod sparse;
mod walk_pool;
pub(crate) use capture::CaptureRoot;
pub use capture::{capture, capture_observed};
pub use key::cache_key;
pub use provenance::{BlobId, CommitTree, raw_blob_id, uses_sha256};
use std::{
    env,
    ffi::CString,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
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
        if has_marker(root)? {
            return Ok(Some(root.to_owned()));
        }
    }
    Ok(None)
}
fn has_marker(root: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(root.join(".git")) {
        Ok(meta) if meta.is_dir() || meta.is_file() => Ok(true),
        Ok(_) => Err(invalid("invalid .git marker")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
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
    fn nested(self: &Arc<Self>, bytes: &[u8], base: usize, fold: bool) -> Option<Arc<Self>> {
        let mut local = Vec::new();
        add_rules(bytes, base, fold, &mut local);
        (!local.is_empty()).then(|| Arc::new(Self { parent: Some(Arc::clone(self)), local }))
    }

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
struct ReadControl {
    stamp: [u64; 7],
    bytes: Vec<u8>,
}

impl ReadControl {
    fn captured(self, stamp: [u64; 7]) -> io::Result<Option<String>> {
        if stamp != self.stamp {
            return Err(io::Error::other("ignore file changed during discovery"));
        }
        Ok(if self.bytes.contains(&0) { None } else { String::from_utf8(self.bytes).ok() })
    }
}

struct Child {
    name: String,
    path: String,
    ty: fs::FileType,
    tracked: bool,
}

/// The directory's ignore file is both a rule source and, when selected, an
/// indexed file. Transfer its existing capture to the matching directory entry.
struct IgnoreFile {
    name: &'static str,
    file: Option<ReadControl>,
}

impl Tree<'_> {
    fn ignore_file(&self, rel: &str, ignored: bool) -> io::Result<IgnoreFile> {
        let name = if self.plain_hidden.is_some() {
            ".ignore"
        } else {
            ".gitignore"
        };
        let file = if ignored {
            None
        } else {
            read_control(self.root, &Path::new(rel).join(name))?
        };
        Ok(IgnoreFile { name, file })
    }

    fn child(&self, entry: &fs::DirEntry, rel: &str) -> io::Result<Option<Child>> {
        let Ok(name) = entry.file_name().into_string() else {
            return Ok(None);
        };
        if self.skip_name(&name) {
            return Ok(None);
        }
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if self.tracked.gitlinks.contains(&path) {
            return Ok(None);
        }
        let ty = entry.file_type()?;
        let tracked = self.tracked.paths.contains(&path);
        if self.is_cache(&path, tracked)? {
            return Ok(None);
        }
        Ok(Some(Child {
            name,
            path,
            ty,
            tracked,
        }))
    }
    fn skip_name(&self, name: &str) -> bool {
        if self.plain_hidden == Some(false) && name.starts_with('.') { return true; }
        if self.fold { name.eq_ignore_ascii_case(".git") } else { name == ".git" }
    }

    fn is_cache(&self, path: &str, tracked: bool) -> io::Result<bool> {
        if self.cache.as_deref() != Some(Path::new(path)) {
            return Ok(false);
        }
        if tracked || self.tracked.has_descendant(path) {
            return Err(invalid("tracked file collides with Codeindex cache"));
        }
        Ok(true)
    }

    fn enter_directory(&self, entry: &fs::DirEntry, path: &str, excluded: bool) -> io::Result<bool> {
        let descendants = std::cell::OnceCell::new();
        let tracked_child = || *descendants.get_or_init(|| self.tracked.has_descendant(path));
        Ok((!excluded || tracked_child())
            && (self.plain_hidden.is_some() || !entry.path().join(".git").try_exists()? || tracked_child()))
    }
}

fn read_control(root: &Path, path: &Path) -> io::Result<Option<ReadControl>> {
    let metadata = match fs::symlink_metadata(root.join(path)) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let bytes = capture::read_control(root, path, &metadata)?;
    Ok(Some(ReadControl {
        stamp: capture::stamp(&metadata),
        bytes,
    }))
}

impl Walk<'_> {
    fn record_file(
        &mut self,
        entry: &fs::DirEntry,
        path: String,
        control: Option<ReadControl>,
    ) -> io::Result<()> {
        let metadata = entry.metadata()?;
        let stamp = capture::stamp(&metadata);
        let captured = control.map(|file| file.captured(stamp)).transpose()?;
        self.found.push(ObservedPath {
            path,
            stamp,
            captured,
        });
        Ok(())
    }
    fn descend(
        &mut self,
        path: String,
        excluded: bool,
        ready: Option<&mut Vec<Directory>>,
    ) -> io::Result<()> {
        if let Some(jobs) = ready {
            jobs.push(Directory {
                rel: path,
                ignored: excluded,
                rules: self.rules.clone(),
                names: self.names.clone(),
            });
            Ok(())
        } else {
            self.visit(&path, excluded, None)
        }
    }

    fn visit(
        &mut self,
        rel: &str,
        ignored: bool,
        ready: Option<&mut Vec<Directory>>,
    ) -> io::Result<()> {
        let control = self.tree.ignore_file(rel, ignored)?;
        let inherited = control
            .file
            .as_ref()
            .and_then(|file| {
                self.rules
                    .nested(&file.bytes, self.names.len(), self.tree.fold)
            })
            .map(|nested| std::mem::replace(&mut self.rules, nested));
        let result = self.visit_entries(rel, ignored, control, ready);
        if let Some(inherited) = inherited {
            self.rules = inherited;
        }
        result
    }
    fn push_name(&mut self, name: String) -> io::Result<()> {
        let name = if self.tree.fold {
            name.to_ascii_lowercase()
        } else {
            name
        };
        self.names.push(CString::new(name).map_err(invalid)?);
        Ok(())
    }

    fn visit_directory(
        &mut self,
        entry: &fs::DirEntry,
        child: Child,
        ignored: bool,
        ready: Option<&mut Vec<Directory>>,
    ) -> io::Result<()> {
        self.push_name(child.name)?;
        let excluded = ignored || self.rules.excludes(&self.names, true);
        let result = match self.tree.enter_directory(entry, &child.path, excluded) {
            Ok(true) => self.descend(child.path, excluded, ready),
            Ok(false) => Ok(()),
            Err(error) => Err(error),
        };
        self.names.pop();
        result
    }

    fn include_leaf(&mut self, name: String, tracked: bool, ignored: bool) -> io::Result<bool> {
        // Tracked leaves override inherited and local exclusions. An ignored
        // directory cannot unignore one of its untracked descendants.
        if tracked || ignored {
            return Ok(tracked);
        }
        if self.rules.local.is_empty() && self.rules.parent.is_none() {
            return Ok(true);
        }
        self.push_name(name)?;
        let included = !self.rules.excludes(&self.names, false);
        self.names.pop();
        Ok(included)
    }

    fn visit_entries(
        &mut self,
        rel: &str,
        ignored: bool,
        mut control: IgnoreFile,
        mut ready: Option<&mut Vec<Directory>>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(self.tree.root.join(rel))? {
            let entry = entry?;
            let Some(child) = self.tree.child(&entry, rel)? else {
                continue;
            };
            self.visit_child(&entry, child, ignored, (&mut control, ready.as_deref_mut()))?;
        }
        Ok(())
    }
    fn visit_child(
        &mut self,
        entry: &fs::DirEntry,
        child: Child,
        ignored: bool,
        (control, ready): (&mut IgnoreFile, Option<&mut Vec<Directory>>),
    ) -> io::Result<()> {
        if child.ty.is_dir() {
            self.visit_directory(entry, child, ignored, ready)
        } else {
            self.visit_leaf(entry, child, ignored, control)
        }
    }

    fn visit_leaf(
        &mut self,
        entry: &fs::DirEntry,
        child: Child,
        ignored: bool,
        control: &mut IgnoreFile,
    ) -> io::Result<()> {
        let is_control = child.name == control.name;
        let link_admitted = self.tree.plain_hidden.is_none() && child.ty.is_symlink();
        let file = child.ty.is_file() || link_admitted;
        if !file || !self.include_leaf(child.name, child.tracked, ignored)? {
            return Ok(());
        }
        let captured = is_control.then(|| control.file.take()).flatten();
        self.record_file(entry, child.path, captured)
    }
}
pub(crate) fn usable_marker(root:&Path)->bool {
    root.join(".git").is_dir() || git_dirs(root).is_ok()
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
        names.extend(encoded_basename(dir)?);
        if dir == root {
            break;
        }
        add_parent_ignore(dir, names.len(), &mut rules)?;
    }
    let tree = Tree {
        root,
        cache: None,
        tracked: Default::default(),
        fold: false,
        plain_hidden: Some(hidden),
    };
    walk(tree, rules, names)
}

fn encoded_basename(dir: &Path) -> io::Result<Option<CString>> {
    dir.file_name().map(|name| CString::new(name.as_encoded_bytes()).map_err(invalid)).transpose()
}

fn add_parent_ignore(dir: &Path, depth: usize, rules: &mut Vec<ignore::Rule>) -> io::Result<()> {
    if let Some(control) = read_control(dir, Path::new(".ignore"))? {
        add_rules(&control.bytes, depth, false, rules);
    }
    Ok(())
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
