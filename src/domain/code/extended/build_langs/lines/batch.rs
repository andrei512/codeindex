use super::super::{Facts, UseMode, simple, span};
use crate::domain::code::Analysis;

#[derive(Default)]
struct BatchQuotes {
    quoted: bool,
    escaped: bool,
}
impl BatchQuotes {
    /// A separator acts only when it is unquoted and was not escaped.
    fn active(&mut self, ch: char) -> bool {
        if self.escaped {
            self.escaped = false;
            return false;
        }
        match ch {
            '^' if !self.quoted => self.escaped = true,
            '"' => self.quoted = !self.quoted,
            _ => return !self.quoted,
        }
        false
    }
}

fn batch_word(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    if line.is_empty() {
        return None;
    }
    let mut quotes = BatchQuotes::default();
    for (i, ch) in line.char_indices() {
        if quotes.active(ch) && ch.is_ascii_whitespace() {
            return Some(line.split_at(i));
        }
    }
    (!quotes.quoted).then_some((line, ""))
}

/// Return the command after a complete, literal IF predicate. Command words
/// occurring inside operands or an echo command are not branch instructions.
fn batch_command(line: &str) -> Option<(&str, &str)> {
    let (op, rest) = batch_word(line)?;
    if !op.eq_ignore_ascii_case("if") {
        return Some((op, rest));
    }
    batch_word(batch_predicate_tail(rest)?)
}

fn batch_predicate_tail(rest: &str) -> Option<&str> {
    let (mut first, mut rest) = batch_word(rest)?;
    if first.eq_ignore_ascii_case("/i") {
        (first, rest) = batch_word(rest)?;
    }
    if first.eq_ignore_ascii_case("not") {
        (first, rest) = batch_word(rest)?;
    }
    if ["defined", "exist", "errorlevel", "cmdextversion"]
        .iter()
        .any(|kw| first.eq_ignore_ascii_case(kw))
    {
        return batch_word(rest).map(|(_, tail)| tail);
    }
    if let Some(right) = batch_inline_comparison(first) {
        return comparison_command(right, rest);
    }
    let (compare, operands) = batch_word(rest)?;
    let binary = ["==", "equ", "neq", "lss", "leq", "gtr", "geq"]
        .iter()
        .any(|kw| compare.eq_ignore_ascii_case(kw));
    if !binary {
        return None;
    }
    batch_word(operands).map(|(_, tail)| tail)
}

fn comparison_command<'a>(right: &str, remaining: &'a str) -> Option<&'a str> {
    if right.is_empty() {
        batch_word(remaining).map(|(_, tail)| tail)
    } else {
        Some(remaining)
    }
}

// An equality written without spaces may have its right operand in the next word.
fn batch_inline_comparison(word: &str) -> Option<&str> {
    let mut quoted = false;
    word.char_indices().find_map(|(i, c)| match c {
        '"' => {
            quoted = !quoted;
            None
        }
        '=' if i > 0 && !quoted => word[i..].strip_prefix("=="),
        _ => None,
    })
}

// cmd.exe separators are active only outside quoting, escaping and handle redirection.
fn batch_segments(line: &str) -> impl Iterator<Item = &str> {
    let mut quotes = BatchQuotes::default();
    let mut redirection = false;
    line.split(move |ch| {
        let active = quotes.active(ch);
        let separator = active && (ch == '|' || ch == '&' && !redirection);
        redirection = active && matches!(ch, '>' | '<');
        separator
    })
}

fn batch_label(name: &str) -> bool {
    simple(name) && !name.eq_ignore_ascii_case("eof")
}

fn batch_branch(command: &str) -> Option<(&str, UseMode)> {
    let (op, rest) = batch_command(command.trim_start().trim_start_matches('@'))?;
    let call = op.eq_ignore_ascii_case("call");
    if !call && !op.eq_ignore_ascii_case("goto") {
        return None;
    }
    let (arg, _) = batch_word(rest)?;
    let (name, mode) = if call {
        (arg.strip_prefix(':')?, UseMode::Call)
    } else {
        (arg.trim_start_matches(':'), UseMode::Reference)
    };
    batch_label(name).then_some((name, mode))
}

fn declare_label(facts: &mut Facts, source: &str, name: &str) -> Option<usize> {
    batch_label(name)
        .then(|| facts.declare(name.to_ascii_lowercase(), span(source, name), "batch_label"))
}

pub(in crate::domain::code) fn batch(source: &str) -> Analysis {
    let mut f = Facts::default();
    let mut owner = None;
    let mut continued = false;
    let commands = source.lines().filter(|line| {
        let skip = continued;
        continued = line.bytes().rev().take_while(|b| *b == b'^').count() % 2 == 1;
        !skip
    });
    for line in commands {
        observe_line(source, line, &mut f, &mut owner);
    }
    f.finish()
}

fn observe_line(source: &str, line: &str, facts: &mut Facts, owner: &mut Option<usize>) {
    let line = line.trim().trim_start_matches('@').trim_start();
    if let Some(name) = line.strip_prefix(':') {
        *owner = declare_label(facts, source, name).or(*owner);
        return;
    }
    for (name, mode) in batch_segments(line).filter_map(batch_branch) {
        facts.usage(name.to_ascii_lowercase(), span(source, name), *owner, mode);
    }
}
