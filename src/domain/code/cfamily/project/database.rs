//! Decode the compile database fields consumed by the indexer.
use super::{CProject, Command, Language, Path, PathBuf, Token, inside, lexer};

use std::{borrow::Cow, collections::HashMap};

pub(super) struct Directory {
    pub(super) path: PathBuf,
    spelled_root: Option<PathBuf>,
    aliased: bool,
}

impl Directory {
    pub(super) fn new(root: &Path, database_dir: &Path, spelling: &str) -> Self {
        let spelled = database_dir.join(spelling);
        let canonical = (!spelled.starts_with(root))
            .then(|| spelled.canonicalize().ok())
            .flatten();
        let path = canonical.unwrap_or_else(|| spelled.clone());
        let spelled_root = path
            .strip_prefix(root)
            .ok()
            .and_then(|suffix| spelled.ancestors().nth(suffix.components().count()))
            .map(Path::to_owned);
        Self {
            aliased: spelled != path,
            path,
            spelled_root,
        }
    }

    pub(super) fn operand(&self, root: &Path, value: &str) -> Option<PathBuf> {
        self.aliased.then(|| self.rebase(root, value)).flatten()
    }

    fn rebase(&self, root: &Path, value: &str) -> Option<PathBuf> {
        let relative = Path::new(value)
            .strip_prefix(self.spelled_root.as_ref()?)
            .ok()?;
        Some(root.join(relative))
    }
}

#[derive(Default)]
struct Fields<'a> {
    directory: Option<&'a Token<'a>>,
    file: Option<&'a Token<'a>>,
    command: Option<&'a Token<'a>>,
    arguments: Option<&'a [Token<'a>]>,
}

impl<'a> Fields<'a> {
    fn read(mut object: &'a [Token<'a>]) -> Self {
        let mut fields = Self::default();
        while let Some((key, value)) = next_field(object) {
            fields.record(json_text(key).as_deref(), &value[0], value);
            if fields.complete() {
                break;
            }
            object = after_value(value);
        }
        fields
    }

    // Even an invalid first value claims its field.
    fn record(&mut self, key: Option<&str>, value: &'a Token<'a>, tail: &'a [Token<'a>]) {
        let field = match key {
            Some("directory") => &mut self.directory,
            Some("file") => &mut self.file,
            Some("command") => &mut self.command,
            Some("arguments") => {
                self.arguments.get_or_insert(tail);
                return;
            }
            _ => return,
        };
        field.get_or_insert(value);
    }
    fn complete(&self) -> bool {
        self.file.is_some() && self.directory.is_some() && self.argument_array().is_some()
    }
    fn argument_array(&self) -> Option<&'a [Token<'a>]> {
        let (open, args) = self.arguments?.split_first()?;
        (open.text == "[").then_some(args)
    }

    fn resolve(
        &self,
        root: &Path,
        database_dir: &Path,
        directories: &mut HashMap<Cow<'a, str>, Directory>,
    ) -> Option<(String, Option<Command>)> {
        let file = json_text(self.file?)?;
        let spelling = json_text(self.directory?)?;
        let dir = directories
            .entry(spelling)
            .or_insert_with_key(|name| Directory::new(root, database_dir, name));
        let file = dir
            .rebase(root, &file)
            .unwrap_or_else(|| PathBuf::from(file.as_ref()));
        let file = inside(root, &dir.path, file.to_str()?)?;
        let command = self
            .argv()
            .and_then(|args| Command::parse(root, dir, &file, &args));
        Some((file, command))
    }

    fn argv(&self) -> Option<Vec<Cow<'a, str>>> {
        if let Some(args) = self.argument_array() {
            return args
                .iter()
                .take_while(|t| t.text != "]")
                .filter(|t| t.text != ",")
                .map(json_text)
                .collect();
        }
        self.command
            .and_then(json_text)
            .and_then(|s| split_command(&s))
    }
}
fn next_field<'a>(object: &'a [Token<'a>]) -> Option<(&'a Token<'a>, &'a [Token<'a>])> {
    let at = object.windows(2).position(|pair| pair[1].text == ":")?;
    let value = &object[at + 2..];
    (!value.is_empty()).then_some((&object[at], value))
}

fn after_value<'a>(value: &'a [Token<'a>]) -> &'a [Token<'a>] {
    let (head, tail) = value
        .split_first()
        .expect("next_field supplies a nonempty value");
    if head.text != "[" {
        return tail;
    }
    tail.iter()
        .position(|t| t.text == "]")
        .map_or(tail, |end| &tail[end + 1..])
}

fn json_text<'a>(t: &Token<'a>) -> Option<Cow<'a, str>> {
    let raw = t.text.strip_prefix('"')?.strip_suffix('"')?;
    let Some((first, mut rest)) = raw.split_once('\\') else {
        return Some(Cow::Borrowed(raw));
    };
    let mut value = String::with_capacity(raw.len());
    value.push_str(first);
    loop {
        let (decoded, tail) = escaped_scalar(rest)?;
        value.push(decoded);
        let Some((part, next)) = tail.split_once('\\') else {
            value.push_str(tail);
            return Some(Cow::Owned(value));
        };
        value.push_str(part);
        rest = next;
    }
}

fn escaped_scalar(rest: &str) -> Option<(char, &str)> {
    let mut chars = rest.chars();
    let escape = chars.next()?;
    let tail = chars.as_str();
    const SHORT: &[u8] = b"\"/\\nrtbf";
    const DECODED: &[u8] = b"\"/\\\n\r\t\x08\x0c";
    if let Some(index) = SHORT.iter().position(|&ch| char::from(ch) == escape) {
        return Some((char::from(DECODED[index]), tail));
    }
    if escape != 'u' {
        return None;
    }
    let (digits, tail) = tail.split_at_checked(4)?;
    let code = u32::from_str_radix(digits, 16).ok()?;
    Some((char::from_u32(code)?, tail))
}

// Compilation databases may store a POSIX-quoted command in place of argv.
// Reject substitutions and shell operators. No shell is ever invoked.
fn split_command(mut command: &str) -> Option<Vec<Cow<'static, str>>> {
    let mut words = Vec::new();
    while !command.trim_start().is_empty() {
        let (word, tail) = command_word(command.trim_start())?;
        words.extend(word.map(Cow::Owned));
        command = tail;
    }
    Some(words)
}

fn command_word(command: &str) -> Option<(Option<String>, &str)> {
    use crate::domain::code::direct::shell_word::{Quoting, WordChars};
    let mut input = WordChars::new(command);
    let mut word = String::new();
    while let Some((ch, quote)) = input.next_until(|tail| tail.starts_with(char::is_whitespace)) {
        match (ch, quote) {
            ('$' | '`', Quoting::Double | Quoting::Bare)
            | (';' | '|' | '&' | '<' | '>', Quoting::Bare) => return None,
            _ => word.push(ch),
        }
    }
    if !input.complete() {
        return None;
    }
    let admitted = input.quoted() || !word.is_empty();
    Some((admitted.then_some(word), &command[input.consumed()..]))
}

impl CProject<'_> {
    pub(super) fn database(&mut self, root: &Path, name: &str, text: &str) {
        let tokens = lexer::tokens(text, Language::Js);
        let base = root.join(Path::new(name).parent().unwrap_or(Path::new("")));
        let mut directories = HashMap::new();
        let commands = tokens
            .split(|t| t.text == "{")
            .skip(1)
            .filter_map(|object| Fields::read(object).resolve(root, &base, &mut directories));
        for (file, command) in commands {
            let entries = self.commands.entry(file).or_default();
            let new = !entries.contains(&command);
            entries.extend(new.then_some(command));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_quotes_preserve_non_special_backslashes() {
        assert_eq!(
            split_command(r#"cc -I"inc\literal" "" 'a$b'"#).unwrap(),
            ["cc", r"-Iinc\literal", "", "a$b"]
        );
        assert_eq!(split_command("cc inc\\\n/dir").unwrap(), ["cc", "inc/dir"]);
        assert!(split_command(r#"cc "$HOME""#).is_none());
    }

    #[test]
    fn argument_array_borrows_literal_words_and_decodes_escaped_words_once() {
        let text = r#"{"arguments":["clang","-Iinc","-DFLAG=\u0031","main.c"]}"#;
        let tokens = lexer::tokens(text, Language::Js);
        let words = Fields::read(&tokens).argv().unwrap();
        assert!(
            matches!(&words[..],[Cow::Borrowed("clang"),Cow::Borrowed("-Iinc"),Cow::Owned(flag),Cow::Borrowed("main.c")] if flag=="-DFLAG=1")
        );
    }
    #[test]
    fn arguments_take_precedence_and_duplicate_fields_keep_the_first_value() {
        let text = r#"{"command":"cc fallback","file":"one.c","file":"two.c","directory":".","arguments":["cc","-DFLAG=1"],"arguments":["bad"]}"#;
        let tokens = lexer::tokens(text, Language::Js);
        let fields = Fields::read(&tokens);
        assert_eq!(fields.file.and_then(json_text).as_deref(), Some("one.c"));
        assert_eq!(fields.argv().unwrap(), ["cc", "-DFLAG=1"]);
        let text = r#"{"arguments":false,"arguments":["bad"],"command":"cc 'with space'"}"#;
        let tokens = lexer::tokens(text, Language::Js);
        assert_eq!(Fields::read(&tokens).argv().unwrap(), ["cc", "with space"]);
    }
}
