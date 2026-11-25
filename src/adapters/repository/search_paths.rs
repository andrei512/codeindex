//! Query-local membership. Explicit search options can expose files absent
//! from the durable catalog; their source is read only for this invocation.
use super::{add_rules, capture, git_dirs, ignore::Rule, invalid};
use std::{
    ffi::CString,
    fs, io,
    os::unix::ffi::OsStrExt,
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
}
pub(crate) struct SearchPath {
    pub name: String,
    pub stamp: [u64; 7],
}

impl SearchRules<'_> {
    pub fn paths(
        &self,
        root: &Path,
        cwd: &Path,
        hidden: bool,
        depth: Option<usize>,
        override_glob: impl Fn(&str) -> Option<bool>,
    ) -> io::Result<Vec<SearchPath>> {
        use std::os::unix::fs::MetadataExt;
        let device = self
            .one_device
            .then(|| fs::metadata(root).map(|m| m.dev()))
            .transpose()?;
        let mut stack: Vec<(u8, Vec<Rule>)> = Vec::new();
        let mut found = Vec::new();
        let git = root.ancestors().find(|dir| dir.join(".git").exists());
        let load = |path: &Path,
                    base: &Path,
                    priority,
                    stack: &mut Vec<(u8, Vec<Rule>)>|
         -> io::Result<()> {
            match fs::read(path) {
                Ok(bytes) => {
                    let mut rules = Vec::new();
                    add_rules(&bytes, base.components().count(), self.fold, &mut rules);
                    stack.push((priority, rules));
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) => {}
                Err(e) => return Err(e),
            }
            Ok(())
        };
        if !self.no_ignore
            && !self.no_global
            && let Some(repo) = git
            && let Ok((git_dir, common)) = git_dirs(repo)
            && let Some(excludes) = super::config::read(&common, &git_dir)?.excludes
        {
            load(&excludes, repo, 1, &mut stack)?;
        }
        if !self.no_files {
            for file in &self.files {
                load(&cwd.join(file), cwd, 0, &mut stack)?;
            }
        }
        if !self.no_ignore
            && !self.no_exclude
            && let Some(repo) = git
        {
            let common = git_dirs(repo).ok().map(|(_, p)| p).or_else(|| {
                let marker = fs::read_to_string(repo.join(".git")).ok()?;
                let actual = cwd.join(marker.trim().strip_prefix("gitdir: ")?);
                let shared = fs::read_to_string(actual.join("commondir")).ok()?;
                Some(actual.join(shared.trim()))
            });
            if let Some(common) = common {
                load(&common.join("info/exclude"), repo, 1, &mut stack)?;
            }
        }
        let controls = |dir: &Path, stack: &mut Vec<(u8, Vec<Rule>)>| -> io::Result<()> {
            if self.no_ignore {
                return Ok(());
            }
            if !self.no_vcs
                && (self.no_require_git || git.is_some_and(|repo| dir.starts_with(repo)))
            {
                load(&dir.join(".gitignore"), dir, 2, stack)?;
            }
            if !self.no_dot {
                load(&dir.join(".ignore"), dir, 3, stack)?;
                load(&dir.join(".rgignore"), dir, 4, stack)?;
            }
            Ok(())
        };
        if !self.no_parent {
            for dir in root
                .ancestors()
                .skip(1)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                controls(dir, &mut stack)?;
            }
        }
        enum Job {
            Enter(PathBuf, usize),
            Leave(usize),
        }
        let mut jobs = vec![Job::Enter(root.to_owned(), 0)];
        let mut ancestors = Vec::new();
        while let Some(job) = jobs.pop() {
            let (dir, level) = match job {
                Job::Leave(n) => {
                    stack.truncate(n);
                    ancestors.pop();
                    continue;
                }
                Job::Enter(p, l) => (p, l),
            };
            let n = stack.len();
            controls(&dir, &mut stack)?;
            ancestors.push(if self.follow {
                dir.canonicalize()?
            } else {
                PathBuf::new()
            });
            jobs.push(Job::Leave(n));
            if depth.is_some_and(|max| level >= max) {
                continue;
            }
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_string_lossy();
                let ty = entry.file_type()?;
                let meta = if ty.is_symlink() {
                    if !self.follow {
                        continue;
                    }
                    fs::metadata(&path)?
                } else {
                    entry.metadata()?
                };
                if meta.is_dir() && device.is_some_and(|dev| meta.dev() != dev) {
                    continue;
                }
                let whitelisted = override_glob(&relative);
                let mut explicitly_included = whitelisted == Some(true);
                if whitelisted == Some(false) {
                    continue;
                }
                if whitelisted != Some(true) && !stack.is_empty() {
                    let names: Vec<_> = path
                        .components()
                        .map(|c| {
                            let bytes = c.as_os_str().as_bytes();
                            CString::new(if self.fold {
                                bytes.to_ascii_lowercase()
                            } else {
                                bytes.to_vec()
                            })
                            .map_err(invalid)
                        })
                        .collect::<io::Result<_>>()?;
                    let excluded = (0..=4).rev().find_map(|rank| {
                        stack
                            .iter()
                            .rev()
                            .filter(|(r, _)| *r == rank)
                            .flat_map(|(_, rules)| rules.iter().rev())
                            .find_map(|r| r.decision(&names, meta.is_dir()))
                    });
                    explicitly_included |= excluded == Some(false);
                    if excluded == Some(true) {
                        continue;
                    }
                }
                if !hidden
                    && !explicitly_included
                    && entry.file_name().as_bytes().first() == Some(&b'.')
                {
                    continue;
                }
                if meta.is_dir() {
                    if self.follow && ancestors.contains(&path.canonicalize()?) {
                        return Err(io::Error::other(format!(
                            "file system loop at {}",
                            path.display()
                        )));
                    }
                    jobs.push(Job::Enter(path, level + 1));
                } else if meta.is_file() {
                    found.push(SearchPath {
                        name: relative.into_owned(),
                        stamp: capture::stamp(&meta),
                    });
                }
            }
        }
        found.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }
}
