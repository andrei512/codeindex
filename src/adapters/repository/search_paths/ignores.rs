//! Search ignore precedence, newest rule first within each priority.
use super::{SearchRules, git_dirs};
use crate::adapters::repository::{add_rules, ignore::Rule, invalid};
use std::{
    ffi::{CString, OsStr},
    fs, io,
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
};

/// Explicit files belong to this invocation, so their parsed rules and warnings
/// are shared by every operand. Patterns still match each written operand.
#[derive(Default)]
pub(super) struct ExplicitControls {
    rules: Vec<Rule>,
    warnings: String,
}

impl ExplicitControls {
    fn load(&mut self, cwd: &Path, file: &str) {
        match fs::read(cwd.join(file)) {
            Ok(bytes) => add_rules(&bytes, 0, false, &mut self.rules),
            Err(error) => self.warn(file, error),
        }
    }

    fn warn(&mut self, file: &str, error: io::Error) {
        use std::fmt::Write;
        let at = if error.kind() == io::ErrorKind::IsADirectory { "line 1: " } else { "" };
        writeln!(self.warnings, "{}: {file}: {at}{error}", crate::adapters::cli::name()).unwrap();
    }
}

pub(super) type Checkpoint = [usize; 4];

pub(super) struct IgnoreRules<'a> {
    ranks: [Vec<Rule>; 4],
    explicit: &'a [Rule],
    fold: bool,
}

impl IgnoreRules<'_> {
    fn load(&mut self, file: &Path, base: &Path, rank: usize) {
        // Inaccessible automatic ignore files provide no rules. The walker
        // reports directory enumeration and source file errors separately.
        if let Ok(bytes) = fs::read(file) {
            add_rules(&bytes, base.components().count(), self.fold, &mut self.ranks[rank]);
        }
    }

    pub fn checkpoint(&self) -> Checkpoint {
        self.ranks.each_ref().map(Vec::len)
    }
    pub fn truncate(&mut self, lengths: Checkpoint) {
        for (rules, n) in self.ranks.iter_mut().zip(lengths) {
            rules.truncate(n);
        }
    }

    pub fn directory<'a>(
        &'a self,
        dir: &'a Path,
        root: &'a Path,
        operand: &'a Path,
    ) -> Directory<'a> {
        Directory {
            rules: self,
            dir,
            root,
            operand,
            active: !self.explicit.is_empty() || self.ranks.iter().any(|r| !r.is_empty()),
            parents: None,
            explicit: None,
        }
    }
}

pub(super) struct Directory<'a> {
    rules: &'a IgnoreRules<'a>,
    dir: &'a Path,
    root: &'a Path,
    operand: &'a Path,
    active: bool,
    parents: Option<Vec<CString>>,
    explicit: Option<Vec<CString>>,
}

fn encode(name: &OsStr, fold: bool) -> io::Result<CString> {
    let bytes = name.as_bytes();
    let mut buffer = Vec::with_capacity(bytes.len() + 1);
    buffer.extend_from_slice(bytes);
    if fold { buffer.make_ascii_lowercase(); }
    CString::new(buffer).map_err(invalid)
}

fn components(path: &Path, fold: bool) -> io::Result<Vec<CString>> {
    path.components()
        .filter(|c| *c != Component::CurDir)
        .map(|c| encode(c.as_os_str(), fold))
        .collect()
}

fn cached_components(cache: &mut Option<Vec<CString>>, build: impl FnOnce() -> io::Result<Vec<CString>>) -> io::Result<&[CString]> {
    if cache.is_none() { *cache = Some(build()?); }
    Ok(cache.as_deref().unwrap())
}

impl Directory<'_> {
    pub fn includes(&mut self, name: &OsStr, dir: bool, hidden: bool, override_glob: Option<bool>) -> io::Result<bool> {
        if let Some(include) = override_glob { return Ok(include); }
        Ok(match self.decision(name, dir)? {
            Some(excluded) => !excluded,
            None => hidden || name.as_bytes().first() != Some(&b'.'),
        })
    }

    fn decision(&mut self, name: &OsStr, dir: bool) -> io::Result<Option<bool>> {
        if !self.active {
            return Ok(None);
        }
        let explicit = self.rules.explicit;
        let discovered = &self.rules.ranks;
        if discovered.iter().any(|rules| !rules.is_empty()) {
            let fold = self.rules.fold;
            let candidate = encode(name, fold)?;
            let parents = cached_components(&mut self.parents, || components(self.dir, fold))?;
            if let Some(decision) = discovered
                .iter()
                .rev()
                .flat_map(|rules| rules.iter().rev())
                .find_map(|r| r.decision_at(parents, &candidate, dir))
            {
                return Ok(Some(decision));
            }
        }
        if !explicit.is_empty() {
            // Explicit rules match the written operand and retain its original case.
            let candidate = encode(name, false)?;
            let parents = cached_components(&mut self.explicit, || {
                let path = self.operand.join(self.dir.strip_prefix(self.root).unwrap());
                components(&path, false)
            })?;
            return Ok(explicit
                .iter()
                .rev()
                .find_map(|r| r.decision_at(parents, &candidate, dir)));
        }
        Ok(None)
    }
}


fn linked_common_directory(repo: &Path, cwd: &Path) -> Option<std::path::PathBuf> {
    let marker = fs::read_to_string(repo.join(".git")).ok()?;
    let actual = cwd.join(marker.trim().strip_prefix("gitdir: ")?);
    let shared = fs::read_to_string(actual.join("commondir")).ok()?;
    Some(actual.join(shared.trim()))
}

impl SearchRules<'_> {
    fn uses_git_excludes(&self) -> bool {
        !self.no_global || !self.no_exclude
    }

    pub(super) fn ignore_repository<'p>(&self, root: &'p Path) -> Option<&'p Path> {
        if self.no_ignore || self.no_vcs {
            return None;
        }
        if self.no_require_git && !self.uses_git_excludes() {
            return None;
        }
        root.ancestors().find(|dir| dir.join(".git").exists())
    }

    fn explicit_controls(&self, cwd: &Path) -> &ExplicitControls {
        self.explicit.get_or_init(|| {
            let mut controls = ExplicitControls::default();
            if self.no_files { return controls; }
            for file in &self.files {
                controls.load(cwd, file);
            }
            controls
        })
    }

    pub(crate) fn ignore_file_warnings(&self, cwd: &Path) -> &str {
        &self.explicit_controls(cwd).warnings
    }

    pub(super) fn initial_rules(&self, root: &Path, cwd: &Path, git: Option<&Path>) -> io::Result<IgnoreRules<'_>> {
        let mut stack = IgnoreRules {
            ranks: Default::default(),
            explicit: &self.explicit_controls(cwd).rules,
            fold: self.fold,
        };
        if self.no_ignore {
            return Ok(stack);
        }
        if let Some(repo) = git {
            self.git_controls(repo, cwd, &mut stack)?;
        }
        if !self.no_parent {
            for dir in root
                .ancestors()
                .skip(1)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                self.directory_controls(git, dir, &mut stack);
            }
        }
        Ok(stack)
    }

    fn git_controls(&self, repo: &Path, cwd: &Path, stack: &mut IgnoreRules) -> io::Result<()> {
        if !self.uses_git_excludes() {
            return Ok(());
        }
        let layout = git_dirs(repo).ok();
        if !self.no_global
            && let Some((git_dir, common)) = &layout
            && let Some(excludes) =
                crate::adapters::repository::config::read(common, git_dir)?.excludes
        {
            stack.load(&excludes, repo, 0);
        }
        if !self.no_exclude {
            let common = layout
                .map(|(_, p)| p)
                .or_else(|| linked_common_directory(repo, cwd));
            if let Some(common) = common {
                stack.load(&common.join("info/exclude"), repo, 0);
            }
        }
        Ok(())
    }

    pub(super) fn directory_controls(&self, git: Option<&Path>, dir: &Path, stack: &mut IgnoreRules) {
        if self.no_ignore {
            return;
        }
        if !self.no_vcs
            && (self.no_require_git || git.is_some_and(|repo| dir.starts_with(repo)))
        {
            stack.load(&dir.join(".gitignore"), dir, 1);
        }
        if !self.no_dot {
            stack.load(&dir.join(".ignore"), dir, 2);
            stack.load(&dir.join(".rgignore"), dir, 3);
        }
    }
}
