//! Resolve simple, explicit install/copy sources in shell installers without
//! executing a checkout. Only matching source operands contribute evidence.
use super::{Evidence, Role};
use crate::domain::code::direct::shell_word::{Quoting, WordChars};
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
// Preserve the quotation on each character until expansion has been classified.
// An unexpanded variable can match only within a component.
struct Operand {
    pattern: String,
    variable: bool,
}

impl Operand {
    fn read(source: &str) -> Option<(Self, usize)> {
        let mut word = WordChars::new(source);
        let mut pattern = String::new();
        let mut variable = false;
        while let Some((ch, quote)) = word.next() {
            if ch == '$' && quote != Quoting::Literal {
                // Read the written name. Quote boundaries can separate two
                // adjacent segments even if both segments are double-quoted.
                let len = variable_length(&source[word.consumed()..])?;
                word.nth(len - 1)?;
                variable = true;
                pattern.push('*');
                continue;
            }
            push_pattern_char(&mut pattern, ch, quote)?;
        }
        (word.complete() && word.consumed() != 0)
            .then_some((Self { pattern, variable }, word.consumed()))
    }

    fn destination(&self) -> bool {
        self.variable || self.pattern.starts_with('/')
    }
}

fn push_pattern_char(pattern: &mut String, ch: char, quote: Quoting) -> Option<()> {
    if ch == '`' && quote != Quoting::Literal {
        return None;
    }
    if quote != Quoting::Bare && matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | ',' | '\\') {
        pattern.push('\\');
    }
    pattern.push(ch);
    Some(())
}

fn variable_length(source: &str) -> Option<usize> {
    let name = source.strip_prefix('{').unwrap_or(source);
    let len = crate::domain::code::direct::shell_word::name_len(name.as_bytes())?;
    if name.len() == source.len() {
        return Some(len);
    }
    (name.as_bytes().get(len) == Some(&b'}')).then_some(len + 2)
}

fn source_pattern(mut pattern: String) -> Option<Glob> {
    if let Some(rest) = pattern.strip_prefix("*/") {
        if !rest.contains('/') || rest.starts_with('*') {
            return None;
        }
        // The installer's root variable can denote either the repository or a subdirectory.
        pattern = format!("**/{rest}");
    }
    if !relative_source(&pattern) || pattern.contains("..") {
        return None;
    }
    Glob::new(pattern.trim_start_matches("./")).ok()
}
fn relative_source(pattern: &str) -> bool {
    pattern.contains('/') && !pattern.starts_with('/')
}

fn install_operands(line: &str) -> Option<Vec<Operand>> {
    let (command, mut rest) = line.trim_start().split_once(char::is_whitespace)?;
    if !matches!(command, "install" | "cp") {
        return None;
    }
    let mut operands = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() || rest.starts_with('#') {
            break;
        }
        let (word, consumed) = Operand::read(rest)?;
        rest = &rest[consumed..];
        operands.push(word);
    }
    operands.pop()?.destination().then_some(operands)
}

fn install_sources(text: &str, patterns: &mut Vec<Glob>) {
    patterns.extend(
        text.lines()
            .filter_map(install_operands)
            .flatten()
            .filter_map(|operand| source_pattern(operand.pattern)),
    );
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
                install_sources(&text, &mut patterns);
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
