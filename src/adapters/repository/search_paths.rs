//! Query-local membership. Explicit search options can expose files absent
//! from the durable catalog; their source is read only for this invocation.
use super::git_dirs;
mod ignores;
use std::{
    borrow::Cow,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(crate) struct SearchRules<'a> {
    pub no_ignore: bool,
    pub no_global: bool,
    pub one_device: bool,
    pub no_dot: bool,
    pub no_vcs: bool,
    pub no_parent: bool,
    pub no_exclude: bool,
    pub no_files: bool,
    pub no_require_git: bool,
    pub fold: bool,
    pub follow: bool,
    pub files: Vec<&'a str>,
    explicit: std::sync::OnceLock<ignores::ExplicitControls>,
}
pub(crate) struct SearchPath {
    pub name: String,
    pub stamp: [u64; 7],
}

impl SearchPath {
    pub fn new(name: Cow<'_, str>, source: &fs::Metadata) -> Self {
        Self {
            name: name.into_owned(),
            stamp: super::capture::stamp(source),
        }
    }
}

impl AsRef<str> for SearchPath {
    fn as_ref(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SearchRoot<'a> {
    pub root: &'a Path,
    pub cwd: &'a Path,
    pub operand: &'a Path,
}

impl SearchRoot<'_> {
    fn display(self, path: &Path) -> PathBuf {
        if path == self.root {
            return self.operand.to_owned();
        }
        self.operand
            .join(path.strip_prefix(self.root).unwrap_or(path))
    }

    fn path_error(self, path: &Path, error: io::Error) -> io::Error {
        let display = self.display(path);
        io::Error::new(error.kind(), format!("{}: {error}", display.display()))
    }
}

/// Link resolution is shared by directory boundary checks and lazy file metadata.
struct ResolvedEntry {
    entry: fs::DirEntry,
    path: PathBuf,
    kind: fs::FileType,
    target: Option<fs::Metadata>,
}

impl ResolvedEntry {
    fn inspect(
        entry: fs::DirEntry,
        follow: bool,
        origin: SearchRoot<'_>,
    ) -> io::Result<Option<Self>> {
        let kind = entry.file_type()?;
        let path = entry.path();
        let target = match (kind.is_symlink(), follow) {
            (false, _) => None,
            (true, false) => return Ok(None),
            (true, true) => {
                Some(fs::metadata(&path).map_err(|error| origin.path_error(&path, error))?)
            }
        };
        let kind = target.as_ref().map_or(kind, fs::Metadata::file_type);
        Ok(Some(Self {
            entry,
            path,
            kind,
            target,
        }))
    }
}

enum Job {
    SortedEntry(fs::DirEntry, usize),
    Enter(PathBuf, usize, Option<(u64, u64)>),
    Leave(ignores::Checkpoint),
}

struct DirectoryBoundary {
    root: Option<(u64, u64)>,
    device: Option<u64>,
    follow: bool,
}

impl DirectoryBoundary {
    fn new(root: &Path, follow: bool, one_device: bool) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let root = (follow || one_device)
            .then(|| fs::metadata(root).map(|m| (m.dev(), m.ino())))
            .transpose()?;
        let device = root.filter(|_| one_device).map(|(dev, _)| dev);
        Ok(Self {
            root,
            device,
            follow,
        })
    }

    fn child(
        &self,
        child: ResolvedEntry,
        ancestors: &[(u64, u64)],
        origin: SearchRoot<'_>,
        level: usize,
    ) -> io::Result<Option<Job>> {
        use std::os::unix::fs::MetadataExt;
        if self.root.is_none() {
            return Ok(Some(Job::Enter(child.path, level, None)));
        }
        let meta = child.target.map_or_else(|| child.entry.metadata(), Ok)?;
        if self.device.is_some_and(|dev| meta.dev() != dev) {
            return Ok(None);
        }
        let id = (meta.dev(), meta.ino());
        if self.follow
            && let Some(index) = ancestors.iter().position(|&ancestor| ancestor == id)
        {
            let ancestor = child
                .path
                .ancestors()
                .nth(ancestors.len() - index)
                .expect("active ancestor");
            return Err(io::Error::other(format!(
                "File system loop found: {} points to an ancestor {}",
                origin.display(&child.path).display(),
                origin.display(ancestor).display()
            )));
        }
        Ok(Some(Job::Enter(child.path, level, Some(id))))
    }
}

/// Leaving a directory restores the ancestor identities and the directory-local ignore
/// precedence levels captured when that directory was entered.
struct Walk {
    boundary: DirectoryBoundary,
    jobs: Vec<Job>,
    ancestors: Vec<(u64, u64)>,
}

impl Walk {
    fn new(root: &Path, boundary: DirectoryBoundary) -> Self {
        Self {
            jobs: vec![Job::Enter(root.to_owned(), 0, boundary.root)],
            boundary,
            ancestors: Vec::new(),
        }
    }

    fn enter(&mut self, id: Option<(u64, u64)>, checkpoint: ignores::Checkpoint) {
        if self.boundary.follow {
            self.ancestors.extend(id);
        }
        self.jobs.push(Job::Leave(checkpoint));
    }

    fn leave(&mut self) {
        if self.boundary.follow {
            self.ancestors.pop();
        }
    }

    fn schedule_sorted(&mut self, mut siblings: Vec<(std::ffi::OsString, fs::DirEntry)>, level: usize, reverse: bool) {
        // Pop the next sibling only after visiting its predecessor's subtree.
        siblings.sort_unstable_by(|(a, _), (b, _)| if reverse { a.cmp(b) } else { b.cmp(a) });
        self.jobs.extend(siblings.into_iter().map(|(_, entry)| Job::SortedEntry(entry, level)));
    }

    fn descend(
        &mut self,
        entry: ResolvedEntry,
        origin: SearchRoot<'_>,
        level: usize,
    ) -> io::Result<()> {
        if let Some(job) = self.boundary.child(entry, &self.ancestors, origin, level)? {
            self.jobs.push(job);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Traversal {
    pub hidden: bool,
    pub depth: Option<usize>,
    pub reverse_path: Option<bool>,
}

/// Visits each readable sibling even when resolving another entry fails.
struct FileVisitor<'a, G, V> {
    origin: SearchRoot<'a>,
    follow: bool,
    traversal: Traversal,
    override_glob: G,
    visit: V,
    errors: Vec<io::Error>,
}

impl<G, V> FileVisitor<'_, G, V>
where
    G: FnMut(&str) -> Option<bool>,
    V: FnMut(Cow<'_, str>, fs::DirEntry, Option<fs::Metadata>, bool) -> io::Result<bool>,
{
    fn directory(
        &mut self,
        dir: &Path,
        ignores: &mut ignores::Directory<'_>,
        walk: &mut Walk,
        level: usize,
    ) -> bool {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.errors.push(self.origin.path_error(dir, error));
                return false;
            }
        };
        if let Some(reverse) = self.traversal.reverse_path {
            let siblings = entries.filter_map(|result| match result {
                Ok(entry) => Some((entry.file_name(), entry)),
                Err(error) => {
                    self.errors.push(self.origin.path_error(dir, error));
                    None
                }
            });
            walk.schedule_sorted(siblings.collect(), level, reverse);
            return false;
        }
        entries.into_iter().any(|entry| self.visit_entry(entry, ignores, walk, level))
    }

    fn visit_entry(
        &mut self,
        entry: io::Result<fs::DirEntry>,
        ignores: &mut ignores::Directory<'_>,
        walk: &mut Walk,
        level: usize,
    ) -> bool {
        match entry.and_then(|entry| self.entry(entry, ignores, walk, level)) {
            Ok(stop) => stop,
            Err(error) => {
                self.errors.push(error);
                false
            }
        }
    }

    fn entry(
        &mut self,
        entry: fs::DirEntry,
        ignores: &mut ignores::Directory<'_>,
        walk: &mut Walk,
        level: usize,
    ) -> io::Result<bool> {
        let Some(entry) = ResolvedEntry::inspect(entry, self.follow, self.origin)? else {
            return Ok(false);
        };
        let basename = entry
            .path
            .file_name()
            .expect("directory entry has a basename");
        let relative = entry
            .path
            .strip_prefix(self.origin.root)
            .unwrap()
            .to_string_lossy();
        let whitelisted = (self.override_glob)(&relative);
        if !ignores.includes(basename, entry.kind.is_dir(), self.traversal.hidden, whitelisted)? {
            return Ok(false);
        }
        if entry.kind.is_dir() {
            walk.descend(entry, self.origin, level + 1)?;
        } else if entry.kind.is_file() {
            return (self.visit)(
                relative,
                entry.entry,
                entry.target,
                whitelisted == Some(true),
            );
        }
        Ok(false)
    }
}

impl SearchRules<'_> {
    pub fn visit_files(
        &self,
        origin: SearchRoot<'_>,
        traversal: Traversal,
        override_glob: impl FnMut(&str) -> Option<bool>,
        visit: impl FnMut(Cow<'_, str>, fs::DirEntry, Option<fs::Metadata>, bool) -> io::Result<bool>,
    ) -> io::Result<Vec<io::Error>> {
        let SearchRoot { root, cwd, operand } = origin;
        let boundary = DirectoryBoundary::new(root, self.follow, self.one_device)?;
        let git = self.ignore_repository(root);
        let mut stack = self.initial_rules(root, cwd, git)?;
        let mut walk = Walk::new(root, boundary);
        let mut visitor = FileVisitor {
            origin,
            follow: self.follow,
            traversal,
            override_glob,
            visit,
            errors: Vec::new(),
        };
        while let Some(job) = walk.jobs.pop() {
            match job {
                Job::Enter(dir, level, id) => {
                    walk.enter(id, stack.checkpoint());
                    self.directory_controls(git, &dir, &mut stack);
                    if traversal.depth.is_none_or(|max| level < max)
                        && visitor.directory(
                            &dir,
                            &mut stack.directory(&dir, root, operand),
                            &mut walk,
                            level,
                        )
                    {
                        break;
                    }
                }
                Job::SortedEntry(entry, level) => {
                    let path = entry.path();
                    let dir = path.parent().expect("directory entry has a parent");
                    if visitor.visit_entry(Ok(entry), &mut stack.directory(dir, root, operand), &mut walk, level) {
                        break;
                    }
                }
                Job::Leave(checkpoint) => {
                    stack.truncate(checkpoint);
                    walk.leave();
                }
            }
        }
        Ok(visitor.errors)
    }
}
