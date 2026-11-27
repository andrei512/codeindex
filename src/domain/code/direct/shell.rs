//! Static shell commands and parameter expansions. Never execute a word during lookup.
use super::shell_word::{Quoting, WordChars, separator, name_start as initial, name_byte as id};
use super::{Analysis, Call, Declaration};
mod command;
mod here;
use here::HereDelimiter;
mod resolve;
pub(in crate::domain::code) use resolve::{ShellResolver, SourceEdges};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::Path,
};
type Owner = Option<usize>;
#[derive(Default)]
pub(in crate::domain::code) struct Evidence {
    declarations: HashMap<(Owner, bool), HashMap<String, Vec<usize>>>,
    sources: Vec<(usize, Owner, String)>,
    uses: Vec<(usize, String, usize, bool)>,
    decl_scope: Vec<usize>,
    parent: Vec<usize>,
    transient: HashSet<usize>,
}
struct Parser<'a, F> {
    comment: F,
    text: &'a str,
    b: &'a [u8],
    out: Analysis,
    e: Evidence,
    scope: usize,
    command_depth: usize,
}
#[derive(Clone, Copy, Default, PartialEq)]
enum State {
    #[default]
    Command,
    Function,
    Timed,
    Args,
    Declare,
    Declaration(command::Declaration, bool),
    Export,
    Trap,
    Loop,
    Source,
    Case(command::CaseWord),
}
enum Substitution {
    Parameter,
    Command(usize),
    Arithmetic(usize),
}
enum UseKind {
    Variable,
    Call,
    FunctionReference,
}
#[derive(Default)]
struct WordContext {
    quote: Quoting,
    braces: usize,
}
struct ArithmeticState {
    depth: usize,
    assignment: std::ops::Range<usize>,
}
enum WordAction {
    Advance(usize),
    Expand,
    Commands,
    ProcessSubstitution(usize),
    End,
}
impl WordContext {
    fn action(
        &mut self,
        bytes: &[u8],
        commands_allowed: bool,
        groups: Option<&mut command::PatternGroups>,
    ) -> WordAction {
        let byte = bytes[0];
        if self.quote.toggle(byte) || self.quote == Quoting::Literal {
            return WordAction::Advance(1);
        }
        self.control_byte(bytes, commands_allowed, groups)
    }

    fn control_byte(
        &mut self,
        bytes: &[u8],
        commands_allowed: bool,
        groups: Option<&mut command::PatternGroups>,
    ) -> WordAction {
        match bytes[0] {
            b'\\' => WordAction::Advance(2),
            b'$' => {
                self.braces += usize::from(bytes.starts_with(b"${"));
                WordAction::Expand
            }
            b'}' if self.braces > 0 => {
                self.braces -= 1;
                WordAction::Advance(1)
            }
            b'`' if commands_allowed => WordAction::Commands,
            _ => self.word_boundary(bytes, groups),
        }
    }

    fn word_boundary(
        &self,
        bytes: &[u8],
        groups: Option<&mut command::PatternGroups>,
    ) -> WordAction {
        if self.quote != Quoting::Bare || self.braces != 0 {
            return WordAction::Advance(1);
        }
        if let Some(width) = groups.and_then(|pattern| pattern.operator(bytes)) {
            return WordAction::Advance(width);
        }
        if matches!(bytes[0], b'<' | b'>') {
            let next = after_continuations(bytes, 1);
            if bytes.get(next) == Some(&b'(') {
                return WordAction::ProcessSubstitution(next + 1);
            }
        }
        if separator(bytes[0]) {
            WordAction::End
        } else {
            WordAction::Advance(1)
        }
    }
}

/// Physical offset of the next logical byte in an unquoted operator.
fn after_continuations(bytes: &[u8], mut i: usize) -> usize {
    while bytes[i..].starts_with(b"\\\n") {
        i += 2;
    }
    i
}

impl<'a, F: FnMut(usize)> Parser<'a, F> {
    fn prefix(&self, i: usize, s: &[u8]) -> bool {
        self.b.get(i..).is_some_and(|v| v.starts_with(s))
    }
    fn blanks(&self, mut i: usize) -> usize {
        loop {
            match &self.b[i..] {
                [b' ' | b'\t' | b'\r', ..] => i += 1,
                [b'\\', b'\n', ..] => i += 2,
                _ => return i,
            }
        }
    }
    fn ident_end(&self, i: usize) -> usize {
        i + self.b.get(i..).and_then(super::shell_word::name_len).unwrap_or(0)
    }
    fn declare(
        &mut self,
        name: Range<usize>,
        span: Range<usize>,
        owner: Owner,
        function: bool,
    ) -> usize {
        let key = &self.text[name];
        let local = self.out.declarations.len();
        self.e.decl_scope.push(self.scope);
        self.out.declarations.push(Declaration {
            span,
            kind: if function {
                "function_definition"
            } else {
                "shell_assignment"
            },
        });
        let names = self.e.declarations.entry((owner, function)).or_default();
        if let Some(ids) = names.get_mut(key) {
            ids.push(local);
        } else {
            names.insert(key.to_owned(), vec![local]);
        }
        local
    }
    fn usage(&mut self, span: Range<usize>, owner: Owner, kind: UseKind) {
        let key = self.text[span.clone()].to_owned();
        let local = self.out.calls.len();
        self.out.calls.push(Call {
            span,
            owner,
            is_call: matches!(kind, UseKind::Call),
        });
        self.e
            .uses
            .push((local, key, self.scope, !matches!(kind, UseKind::Variable)));
    }
    fn param(&mut self, i: usize, owner: Owner) -> usize {
        let (braced, prefix) = match &self.b[i + 1..] {
            [b'{', b'#' | b'!', ..] => (true, 3),
            [b'{', ..] => (true, 2),
            _ => (false, 1),
        };
        let start = i + prefix;
        let end = self.ident_end(start);
        if end == start {
            return i + 1;
        }
        if braced && matches!(&self.b[end..], [b':', b'=', ..] | [b'=', ..]) {
            self.declare(start..end, start..end, owner, false);
        } else {
            self.usage(start..end, owner, UseKind::Variable);
        }
        end
    }
    fn arithmetic_name(&mut self, span: Range<usize>, owner: Owner, assignment: &mut Range<usize>) {
        let eq = self.blanks(span.end);
        if self.b.get(eq) == Some(&b'=') && !self.prefix(eq, b"==") {
            // Chained assignments share the next delimiter and trailing whitespace.
            if eq >= assignment.end {
                let rest = &self.b[eq + 1..];
                let len = rest
                    .iter()
                    .position(|c| matches!(c, b';' | b')' | b','))
                    .unwrap_or(rest.len());
                *assignment = eq + 1 + rest[..len].trim_ascii_end().len()..eq + 1 + len;
            }
            self.declare(
                span.clone(),
                span.start..assignment.start.max(eq + 1),
                owner,
                false,
            );
        } else {
            self.usage(span, owner, UseKind::Variable)
        }
    }
    fn arithmetic(&mut self, mut i: usize, owner: Owner) -> usize {
        let mut state = ArithmeticState {
            depth: 1,
            assignment: 0..0,
        };
        while let Some(&byte) = self.b.get(i) {
            if byte == b')' && self.arithmetic_close(i, state.depth) {
                return i + 2;
            }
            i = self.arithmetic_token((i, byte), owner, &mut state);
        }
        i
    }

    fn arithmetic_close(&self, at: usize, depth: usize) -> bool {
        depth == 1 && self.b.get(at + 1) == Some(&b')')
    }

    fn arithmetic_token(
        &mut self,
        (i, byte): (usize, u8),
        owner: Owner,
        state: &mut ArithmeticState,
    ) -> usize {
        match byte {
            b'(' => {
                state.depth += 1;
                i + 1
            }
            b')' => {
                state.depth = (state.depth - 1).max(1);
                i + 1
            }
            b'`' | b'$' => self.arithmetic_expansion(i, owner, &mut state.depth),
            c if initial(c) => {
                let end = self.ident_end(i);
                self.arithmetic_name(i..end, owner, &mut state.assignment);
                end
            }
            _ => i + 1,
        }
    }
    fn arithmetic_expansion(&mut self, at: usize, owner: Owner, depth: &mut usize) -> usize {
        if self.b[at] == b'`' {
            return self.backtick(at, owner);
        }
        match self.substitution(at) {
            Substitution::Parameter => self.param(at, owner),
            Substitution::Command(body) => self.nested_commands(body, owner, b')'),
            Substitution::Arithmetic(body) => {
                *depth += 2;
                body
            }
        }
    }

    fn substitution(&self, dollar: usize) -> Substitution {
        let start = after_continuations(self.b, dollar + 1);
        if self.b.get(start) != Some(&b'(') {
            return Substitution::Parameter;
        }
        let operand = after_continuations(self.b, start + 1);
        if self.b.get(operand) == Some(&b'(') {
            Substitution::Arithmetic(operand + 1)
        } else {
            Substitution::Command(start + 1)
        }
    }

    fn expansion(&mut self, at: usize, owner: Owner) -> usize {
        match self.substitution(at) {
            Substitution::Arithmetic(body) => self.arithmetic(body, owner),
            Substitution::Command(body) => self.nested_commands(body, owner, b')'),
            Substitution::Parameter => self.param(at, owner),
        }
    }

    fn word(&mut self, mut i: usize, owner: Owner, pattern: bool) -> usize {
        let mut word = WordContext::default();
        let mut groups = pattern.then(command::PatternGroups::default);
        while i < self.b.len() {
            i = match word.action(&self.b[i..], self.command_depth < 80, groups.as_mut()) {
                WordAction::Advance(n) => (i + n).min(self.b.len()),
                WordAction::Expand => self.expansion(i, owner),
                WordAction::Commands => self.backtick(i, owner),
                WordAction::ProcessSubstitution(width) => {
                    self.nested_commands(i + width, owner, b')')
                }
                WordAction::End => break,
            };
        }
        i
    }
    // Retain absolute offsets for declarations and uses in nested input.
    fn within(&mut self, end: usize, read: impl FnOnce(&mut Self)) {
        let text = self.text;
        self.text = &text[..end];
        self.b = self.text.as_bytes();
        read(self);
        self.text = text;
        self.b = text.as_bytes();
    }

    fn backtick(&mut self, start: usize, owner: Owner) -> usize {
        let mut close = start + 1;
        while close < self.b.len() {
            match self.b[close] {
                b'`' => break,
                b'\\' => close += 2,
                _ => close += 1,
            }
        }
        let end = close.min(self.b.len());
        self.within(end, |parser| {
            parser.nested_commands(start + 1, owner, b'`');
        });
        (end + 1).min(self.b.len())
    }

    fn enter(&mut self) {
        let scope = self.e.parent.len();
        self.e.parent.push(self.scope);
        self.scope = scope;
    }
    fn nested_commands(&mut self, i: usize, owner: Owner, terminator: u8) -> usize {
        let saved = self.scope;
        self.enter();
        self.command_depth += 1;
        let pos = self.command_list(i, owner, Some(terminator));
        self.command_depth -= 1;
        self.scope = saved;
        pos
    }
    fn function_body(&self, end: usize, keyword: bool) -> Option<(usize, u8)> {
        let mut opener = self.blanks(end);
        if self.prefix(opener, b"()") {
            opener = self.blanks(opener + 2);
            while self.b.get(opener) == Some(&b'\n') {
                opener = self.blanks(opener + 1);
            }
        } else if !keyword {
            return None;
        }
        let terminator = match self.b.get(opener)? {
            b'{' => b'}',
            b'(' => b')',
            _ => return None,
        };
        Some((opener + 1, terminator))
    }
}
fn source_path(raw: &str) -> Option<String> {
    let mut word = source_word(raw);
    let path: String = word.by_ref().map(source_character).collect::<Option<_>>()?;
    (word.finished() && !path.is_empty() && !Path::new(&path).is_absolute()).then_some(path)
}

fn source_word(raw: &str) -> WordChars<'_> {
    // These two written expansions identify the directory of the current script.
    let quoted = raw.strip_prefix('"');
    let spelling = quoted.unwrap_or(raw);
    let prefix = ["$(dirname \"$0\")/", "${BASH_SOURCE%/*}/"]
        .iter()
        .find_map(|prefix| spelling.strip_prefix(prefix));
    match (quoted, prefix) {
        (Some(_), Some(tail)) => WordChars::double_quoted_tail(tail),
        (None, Some(tail)) => WordChars::new(tail),
        (_, None) => WordChars::new(raw),
    }
}

fn source_character((ch, mode): (char, Quoting)) -> Option<char> {
    match (mode, ch) {
        (Quoting::Bare | Quoting::Double, '$' | '\x60')
        | (Quoting::Bare, '*' | '?' | '[') => None,
        _ => Some(ch),
    }
}

pub(super) fn analyze(text: &str) -> Analysis {
    analyze_with_comments(text, |_| {})
}

pub(in crate::domain::code) fn analyze_with_comments(
    text: &str,
    comment: impl FnMut(usize),
) -> Analysis {
    let mut p = Parser {
        comment,
        text,
        b: text.as_bytes(),
        out: Analysis::default(),
        e: Evidence {
            parent: vec![0],
            ..Default::default()
        },
        scope: 0,
        command_depth: 0,
    };
    p.command_list(0, None, None);
    p.out.direct_evidence = Some(Box::new(super::Evidence::Shell(p.e)));
    p.out
}
pub(super) fn path_for(source: &str, import: &str, paths: &HashMap<&str, usize>) -> Option<usize> {
    let p = crate::domain::code::relative_path(Path::new(source).parent()?, import)?;
    paths
        .get(p.to_str()?)
        .or_else(|| paths.get(import))
        .copied()
}
crate::domain::code::cache::record!(Evidence {
    declarations,
    sources,
    uses,
    decl_scope,
    parent,
    transient
});

#[cfg(test)]
mod quote_tests {
    use super::*;

    #[test]
    fn array_data_and_arithmetic_substitutions_keep_exact_call_sites() {
        let source = "hit() { :; }\nitems=(hit $(hit) hit); echo $(( $(hit) + 1 ))";
        let sites: Vec<_> = analyze(source)
            .calls
            .into_iter()
            .filter(|call| call.is_call && &source[call.span.clone()] == "hit")
            .map(|call| call.span.start)
            .collect();
        let expected: Vec<_> = source
            .match_indices("$(hit)")
            .map(|(at, _)| at + 2)
            .collect();
        assert_eq!(sites, expected);
    }

    #[test]
    fn redirections_preserve_command_and_argument_positions() {
        let source = "hit() { :; }\n2>log hit arg; : &>log hit; > >(hit) hit; time -p hit";
        let sites: Vec<_> = analyze(source)
            .calls
            .into_iter()
            .filter(|call| call.is_call && &source[call.span.clone()] == "hit")
            .map(|call| call.span.start)
            .collect();
        let expected = [
            source.find("hit arg").unwrap(),
            source.find(">(hit)").unwrap() + 2,
            source.find(") hit;").unwrap() + 2,
            source.rfind("hit").unwrap(),
        ];
        assert_eq!(sites, expected);
    }

    #[test]
    fn continued_here_delimiter_does_not_consume_the_following_command() {
        let text = "hit() { :; }\ncat << \\\n  END\nhit\nEND\nhit\n";
        let sites: Vec<_> = analyze(text)
            .calls
            .into_iter()
            .filter(|call| call.is_call && &text[call.span.clone()] == "hit")
            .map(|call| call.span.start)
            .collect();
        assert_eq!(sites, [text.rfind("hit").unwrap()]);
    }

    #[test]
    fn case_patterns_only_call_commands_in_substitutions_and_bodies() {
        let source = "case $(value) in @(hit|@(miss|pick))) hit ;; esac";
        let calls = analyze(source).calls;
        let sites: Vec<_> = calls
            .iter()
            .filter(|call| call.is_call)
            .map(|call| call.span.start)
            .collect();
        assert_eq!(
            sites,
            [source.find("value").unwrap(), source.rfind("hit").unwrap()]
        );
    }

    #[test]
    fn source_operands_preserve_quoted_spelling_and_reject_live_expansions() {
        for word in [r"./dep\ lib.sh", r#"./de"p lib".sh"#, "'./dep lib.sh'"] {
            assert_eq!(source_path(word).as_deref(), Some("./dep lib.sh"));
        }
        for word in ["$dir/file", r#""$dir/file""#, "scripts/*.sh", "'unfinished"] {
            assert!(source_path(word).is_none(), "{word}");
        }
        assert_eq!(
            source_path("'scripts/*.sh'").as_deref(),
            Some("scripts/*.sh")
        );
    }
}
