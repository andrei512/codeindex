//! Resolve simple, explicit install/copy sources in shell installers without
//! executing a checkout. Only matching source operands contribute evidence.
use super::{Evidence, Role};
use crate::domain::pattern::Glob;
use std::{io, path::Path};

#[derive(Default)]
pub(super) struct Packaging {
    pub scripts: Vec<String>,
    sources: Option<Vec<Glob>>,
}
pub(super) fn installer(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(name, "install.sh" | "install.bash" | "setup.sh")
        || path.ends_with(".sh")
            && path
                .split('/')
                .any(|p| matches!(p, "functions" | "install" | "installer"))
}
fn source_pattern(word: &str) -> Option<Glob> {
    let unquoted = word.replace('"', "");
    // Keep literal components and globs; unexpanded variables can only match
    // within a component. An unknown root still needs a literal subdirectory.
    let mut iter = unquoted.chars().peekable();
    let mut pattern = String::new();
    while let Some(c) = iter.next() {
        if c != '$' {
            pattern.push(c);
            continue;
        }
        if iter.peek() == Some(&'{') {
            iter.next();
            let mut ended = false;
            for c in iter.by_ref() {
                if c == '}' {
                    ended = true;
                    break;
                }
                if !(c.is_ascii_alphanumeric() || c == '_') {
                    return None;
                }
            }
            if !ended {
                return None;
            }
        } else {
            if !iter
                .peek()
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_')
            {
                return None;
            }
            while iter
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                iter.next();
            }
        }
        pattern.push('*');
    }
    if let Some(rest) = pattern.strip_prefix("*/") {
        if !rest.contains('/') || rest.starts_with('*') {
            return None;
        }
        // The installer's root variable can denote either the repository or a subdirectory.
        pattern = format!("**/{rest}");
    }
    if !pattern.contains('/') || pattern.starts_with('/') || pattern.contains("..") {
        return None;
    }
    Glob::new(pattern.trim_start_matches("./")).ok()
}
impl Packaging {
    pub fn applies(
        &mut self,
        path: &str,
        base: Evidence,
        read: &mut impl FnMut(&Path) -> io::Result<Option<String>>,
    ) -> io::Result<bool> {
        if base.0 != Role::Unknown && !(base.0 == Role::Support && base.1 == "test filename") {
            return Ok(false);
        }
        if self.sources.is_none() {
            let mut patterns = Vec::new();
            for script in &self.scripts {
                let Some(text) = read(Path::new(script))? else {
                    continue;
                };
                for line in text.lines() {
                    let mut words = line.split_whitespace();
                    let cmd = words.next();
                    if !matches!(cmd, Some("install" | "cp")) {
                        continue;
                    }
                    let args: Vec<_> = words.take_while(|w| !w.starts_with('#')).collect();
                    let Some((dest, inputs)) = args.split_last() else {
                        continue;
                    };
                    if !dest.contains('$') && !dest.starts_with('/') {
                        continue;
                    }
                    for src in inputs {
                        if let Some(g) = source_pattern(src) {
                            patterns.push(g);
                        }
                    }
                }
            }
            self.sources = Some(patterns);
        }
        Ok(self
            .sources
            .as_ref()
            .unwrap()
            .iter()
            .any(|g| g.is_match(path)))
    }
}
