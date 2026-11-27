//! Command-line overrides match paths relative to the invocation directory.
//! The same decision controls walking ignored files and selecting search hits.
use super::super::Glob;
use std::path::Path;

#[derive(Default)]
pub(super) struct Globs {
    rules: Vec<Rule>,
    pub positive: bool,
    needs_path: bool,
}

struct Rule {
    include: bool,
    path: bool,
    pattern: Glob,
}

impl Globs {
    pub fn add(&mut self, value: &str, fold: bool) -> Result<(), String> {
        let pattern = value.strip_prefix('!').unwrap_or(value);
        let include = pattern == value;
        let path = pattern.contains('/');
        self.rules.push(Rule {
            include,
            path,
            pattern: Glob::with_case(pattern.trim_start_matches('/'), fold)?,
        });
        self.positive |= include;
        self.needs_path |= path;
        Ok(())
    }

    pub fn for_operand(&self, cwd: &Path, operand: &Path) -> impl FnMut(&str) -> Option<bool> {
        let mut path = if self.needs_path { operand_prefix(cwd, operand) } else { String::new() };
        let offset = path.len();
        let mut scratch = crate::domain::pattern::Scratch::default();
        move |relative| {
            if self.rules.is_empty() {
                return None;
            }
            let name = relative.rsplit('/').next().unwrap_or(relative);
            let full = if offset == 0 {
                relative
            } else {
                path.truncate(offset);
                path.push_str(relative);
                path.as_str()
            };
            self.rules
                .iter()
                .rev()
                .find(|r| r.pattern.matches_with(if r.path { full } else { name }, &mut scratch))
                .map(|r| r.include)
        }
    }
}

fn operand_prefix(cwd: &Path, operand: &Path) -> String {
    let written = operand.strip_prefix(cwd).unwrap_or(operand).to_str().expect("UTF-8 operand");
    let prefix = written.trim_start_matches("./").trim_end_matches('/');
    if prefix.is_empty() || prefix == "." {
        return written.starts_with('/').then(|| "/".to_owned()).unwrap_or_default();
    }
    let mut path = String::with_capacity(prefix.len() + 1);
    path.push_str(prefix);
    path.push('/');
    path
}
