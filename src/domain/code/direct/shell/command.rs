//! A command list owns its control scopes and pending assignment prefixes.
use super::*;

#[derive(Default)]
struct Frame<'src> {
    pending_docs: Vec<HereDelimiter<'src>>,
    state: State,
    cases: usize,
    blocks: Vec<usize>,
    prefix_vars: Vec<usize>,
}

impl Frame<'_> {
    fn closes(&self, c: u8, next: Option<&u8>, terminator: Option<u8>) -> bool {
        if terminator != Some(c) {
            return false;
        }
        match c {
            b')' => self.cases == 0,
            b'}' => next.is_none_or(|&n| separator(n)),
            _ => true,
        }
    }
    fn separator(&mut self, bytes: &[u8]) -> usize {
        if self.cases != 0
            && let Some(arm_end) = case_terminator(bytes)
        {
            self.state = State::Case(CaseWord::Pattern);
            self.prefix_vars.clear();
            return arm_end;
        }
        match (self.state, bytes[0]) {
            (State::Case(CaseWord::Pattern), b')') => self.next_command(),
            (State::Case(_), _) => {}
            _ => self.next_command(),
        }
        1
    }

    fn assigned(&mut self, variable: usize) {
        match self.state {
            State::Command => self.prefix_vars.push(variable),
            State::Declaration(role, _) => self.state = State::Declaration(role, false),
            _ => {}
        }
    }

    fn next_command(&mut self) {
        self.prefix_vars.clear();
        self.state = State::Command;
    }
}

/// Count physical bytes so subsequent source positions include line continuations.
fn case_terminator(bytes: &[u8]) -> Option<usize> {
    if bytes.first() != Some(&b';') {
        return None;
    }
    let second = after_continuations(bytes, 1);
    match bytes.get(second)? {
        b'&' => Some(second + 1),
        b';' => {
            let third = after_continuations(bytes, second + 1);
            let suffix = bytes.get(third).filter(|&&byte| byte == b'&');
            Some(suffix.map_or(second + 1, |_| third + 1))
        }
        _ => None,
    }
}

/// Parentheses and alternatives within an extended glob belong to the pattern.
#[derive(Default)]
pub(super) struct PatternGroups(usize);

impl PatternGroups {
    pub(super) fn operator(&mut self, bytes: &[u8]) -> Option<usize> {
        match bytes {
            [b'@' | b'?' | b'+' | b'*' | b'!', b'(', ..] => {
                self.0 += 1;
                Some(2)
            }
            [b')', ..] if self.0 > 0 => {
                self.0 -= 1;
                Some(1)
            }
            [b'|', ..] if self.0 > 0 => Some(1),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum CaseWord {
    Subject,
    In,
    Pattern,
}

impl CaseWord {
    fn after(self, raw: &str) -> State {
        match (self, raw) {
            (Self::Subject, _) => State::Case(Self::In),
            (Self::In, "in") => State::Case(Self::Pattern),
            (Self::Pattern, "esac") => State::Command,
            _ => State::Case(self),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Declaration {
    Write,
    ShowVariable,
    ShowFunction,
}
impl Declaration {
    fn options(self, raw: &str) -> Option<Self> {
        let flags = raw.strip_prefix('-')?;
        if flags.is_empty() || !flags.bytes().all(|b| b.is_ascii_alphabetic()) {
            return None;
        }
        Some(if flags.contains(['f', 'F']) {
            Self::ShowFunction
        } else if self == Self::Write && flags.contains('p') {
            Self::ShowVariable
        } else {
            self
        })
    }
}

impl State {
    /// Keywords inside a case clause or timed pipeline precede ordinary command words.
    fn for_word(self, spelling: &str) -> Option<Self> {
        match self {
            Self::Declaration(role, true) if spelling == "--" => {
                Some(Self::Declaration(role, false))
            }
            Self::Case(phase) => Some(phase.after(spelling)),
            Self::Timed if matches!(spelling, "-p" | "--") => None,
            Self::Timed => Some(Self::Command),
            other => Some(other),
        }
    }

    fn builtin(spelling: &str) -> Option<Self> {
        Some(match spelling {
            "trap" => State::Trap,
            "export" => State::Export,
            "local" | "declare" | "typeset" => State::Declaration(Declaration::Write, true),
            "readonly" | "read" => State::Declare,
            "." | "source" => State::Source,
            "command" | "exec" | "builtin" | "env" | "sudo" => State::Args,
            _ => return None,
        })
    }

    fn accepts_assignment(self) -> bool {
        matches!(
            self,
            Self::Command
                | Self::Function
                | Self::Declare
                | Self::Declaration(Declaration::Write, _)
                | Self::Export
        )
    }
}

#[derive(Clone, Copy)]
struct Word {
    start: usize,
    name_end: usize,
    end: usize,
}

impl Word {
    fn is_identifier(self) -> bool {
        self.name_end == self.end
    }

    /// Exact span of a statically spelled name after removing an enclosing quote.
    fn command_span(self, bytes: &[u8], plain: bool) -> Option<Range<usize>> {
        if plain {
            return Some(self.start..self.end);
        }
        let [quote @ (b'\'' | b'"'), name @ .., end] = &bytes[self.start..self.end] else {
            return None;
        };
        let valid = name == b"."
            || name.first().is_some_and(|&c| initial(c))
                && name.iter().all(|&c| id(c) || c == b'-');
        (quote == end && valid).then_some(self.start + 1..self.end - 1)
    }

    fn plain_command(self, bytes: &[u8]) -> bool {
        self.name_end > self.start
            && bytes[self.name_end..self.end]
                .iter()
                .all(|&b| id(b) || b == b'-')
    }
}

impl<'a, F: FnMut(usize)> Parser<'a, F> {
    fn skip_command_trivia(&mut self, i: usize) -> usize {
        let start = self.blanks(i);
        if self.b.get(start) != Some(&b'#') {
            return start;
        }
        (self.comment)(start);
        self.text[start..]
            .find('\n')
            .map_or(self.b.len(), |n| start + n)
    }

    pub(super) fn command_list(
        &mut self,
        mut i: usize,
        owner: Owner,
        terminator: Option<u8>,
    ) -> usize {
        if self.command_depth > 80 {
            return self.b.len();
        }
        let mut frame = Frame::default();
        loop {
            i = self.skip_command_trivia(i);
            if let Some(end) = self.command_end(&frame, i, terminator) {
                return end;
            }
            i = self.command_token(&mut frame, i, owner);
        }
    }

    fn command_end(&self, frame: &Frame, at: usize, terminator: Option<u8>) -> Option<usize> {
        let Some(&byte) = self.b.get(at) else {
            return Some(at);
        };
        frame
            .closes(byte, self.b.get(at + 1), terminator)
            .then_some(at + 1)
    }

    fn command_token(&mut self, frame: &mut Frame<'a>, i: usize, owner: Owner) -> usize {
        match &self.b[i..] {
            [b'\n', ..] => {
                frame.separator(&self.b[i..]);
                self.here(i + 1, &mut frame.pending_docs, owner)
            }
            [b'(', ..] => self.parenthesized(frame, i, owner),
            [b'&', ..] => self.ampersand(frame, i, owner),
            [b'<' | b'>', ..] => self.redirection(frame, i, owner),
            [b';' | b'|' | b')', ..] => i + frame.separator(&self.b[i..]),
            [b'{', _, ..] => self.braced_command(frame, i, owner),
            _ => self.scan_command_word(frame, i, owner),
        }
    }

    fn braced_command(&mut self, frame: &mut Frame<'a>, i: usize, owner: Owner) -> usize {
        if frame.state == State::Command && separator(self.b[i + 1]) {
            frame.state = State::Args;
            return self.nested_commands(i + 1, owner, b'}');
        }
        self.scan_command_word(frame, i, owner)
    }

    fn ampersand(&mut self, frame: &mut Frame<'a>, i: usize, owner: Owner) -> usize {
        let next = after_continuations(self.b, i + 1);
        if self.b.get(next) == Some(&b'>') {
            return self.redirection(frame, next, owner);
        }
        i + frame.separator(&self.b[i..])
    }

    fn parenthesized(&mut self, frame: &mut Frame, i: usize, owner: Owner) -> usize {
        if frame.state == State::Case(CaseWord::Pattern) {
            return i + 1;
        }
        frame.state = State::Args;
        if self.prefix(i, b"((") {
            return self.arithmetic(i + 2, owner);
        }
        self.nested_commands(i + 1, owner, b')')
    }

    fn redirection(&mut self, frame: &mut Frame<'a>, i: usize, owner: Owner) -> usize {
        let next = after_continuations(self.b, i + 1);
        let end = match (self.b[i], self.b.get(next)) {
            (_, Some(b'(')) => return self.scan_command_word(frame, i, owner),
            (b'<', Some(b'<')) => {
                let third = after_continuations(self.b, next + 1);
                if self.b.get(third) != Some(&b'<') {
                    let (end, delimiter) = self.here_delimiter(third);
                    frame.pending_docs.extend(delimiter);
                    return end;
                }
                third + 1
            }
            (b'>', Some(b'>' | b'|' | b'&')) | (b'<', Some(b'>' | b'&')) => next + 1,
            _ => next,
        };
        let target = self.blanks(end);
        self.word(target, owner, false)
    }

    fn scan_command_word(&mut self, frame: &mut Frame, start: usize, owner: Owner) -> usize {
        // Only command words need an identifier prefix. word() records expansions
        // for arguments and redirect targets as well.
        let name_end = if frame.state != State::Args {
            self.ident_end(start)
        } else {
            start
        };
        let end = self.word(
            name_end,
            owner,
            frame.state == State::Case(CaseWord::Pattern),
        );
        if end == start {
            return start + 1;
        }
        // An unquoted number immediately before a redirect names its descriptor.
        if matches!(self.b.get(end), Some(b'<' | b'>'))
            && self.b[start].is_ascii_digit()
            && self.text[start..end]
                .split("\\\n")
                .all(|part| part.bytes().all(|b| b.is_ascii_digit()))
        {
            return end;
        }
        self.command_word(
            frame,
            Word {
                start,
                name_end,
                end,
            },
            owner,
        )
    }

    fn command_word(&mut self, frame: &mut Frame, word: Word, owner: Owner) -> usize {
        let spelling = &self.text[word.start..word.end];
        let Some(mode) = frame.state.for_word(spelling) else {
            return word.end;
        };
        frame.state = mode;
        if mode.accepts_assignment()
            && let Some(end) = self.assignment(frame, word, owner)
        {
            return end;
        }
        match mode {
            State::Command | State::Function => self.head_word(frame, word, owner),
            State::Args | State::Case(_) => word.end,
            _ => {
                frame.state = self.command_operand(mode, word, owner);
                word.end
            }
        }
    }

    fn head_word(&mut self, frame: &mut Frame, word: Word, owner: Owner) -> usize {
        let plain = word.plain_command(self.b);
        if plain
            && let Some((body, terminator)) =
                self.function_body(word.end, frame.state == State::Function)
        {
            let name = word.start..word.end;
            let id = self.declare(name.clone(), name, owner, true);
            frame.state = State::Args;
            return self.nested_commands(body, Some(id), terminator);
        }
        self.e.transient.extend(frame.prefix_vars.drain(..));
        frame.state = self.command_head(frame, word, owner, plain);
        word.end
    }

    fn command_operand(&mut self, mode: State, word: Word, owner: Owner) -> State {
        match mode {
            State::Declaration(role, options) => self.declaration_word(role, options, word, owner),
            State::Declare => self.declare_builtin(word, owner),
            State::Loop => {
                self.declare_operand(word, owner);
                State::Args
            }
            State::Export => {
                self.variable_reference(word, owner);
                State::Export
            }
            State::Trap => {
                self.function_reference(word, owner);
                State::Args
            }
            State::Source => {
                let raw = &self.text[word.start..word.end];
                self.e
                    .sources
                    .extend(source_path(raw).map(|path| (word.start, owner, path)));
                State::Args
            }
            _ => State::Args,
        }
    }

    fn declare_builtin(&mut self, word: Word, owner: Owner) -> State {
        if matches!(&self.b[word.start..word.end], [b'-', b'f' | b'F', ..]) {
            return State::Args;
        }
        self.declare_operand(word, owner);
        State::Declare
    }

    fn variable_reference(&mut self, word: Word, owner: Owner) {
        if word.is_identifier() {
            self.usage(word.start..word.end, owner, UseKind::Variable);
        }
    }

    fn function_reference(&mut self, word: Word, owner: Owner) {
        if let Some(name) = word.command_span(self.b, word.plain_command(self.b)) {
            self.usage(name, owner, UseKind::FunctionReference);
        }
    }

    fn declaration_word(
        &mut self,
        role: Declaration,
        options: bool,
        word: Word,
        owner: Owner,
    ) -> State {
        let span = word.start..word.end;
        if options && let Some(next) = role.options(&self.text[span.clone()]) {
            return State::Declaration(next, true);
        }
        match role {
            Declaration::Write => self.declare_operand(word, owner),
            Declaration::ShowVariable => self.variable_reference(word, owner),
            Declaration::ShowFunction => self.function_reference(word, owner),
        }
        State::Declaration(role, false)
    }

    fn declare_operand(&mut self, word: Word, owner: Owner) {
        if word.is_identifier() {
            let name = word.start..word.end;
            self.declare(name.clone(), name, owner, false);
        }
    }

    /// A compound assignment contains data words; substitutions in each word still execute.
    fn array_words(&mut self, mut i: usize, owner: Owner) -> usize {
        loop {
            i = self.skip_command_trivia(i);
            match self.b.get(i) {
                None => return i,
                Some(b')') => return i + 1,
                Some(b'\n') => i += 1,
                _ => i = self.word(i, owner, true).max(i + 1),
            }
        }
    }

    fn assignment(&mut self, frame: &mut Frame, word: Word, owner: Owner) -> Option<usize> {
        let Word {
            start,
            name_end,
            end,
        } = word;
        if start == name_end {
            return None;
        }
        let equals = match self.b.get(name_end..)? {
            [b'=', ..] => name_end,
            [b'+', b'=', ..] => name_end + 1,
            _ => return None,
        };
        let extent = if end == equals + 1 && self.b.get(end) == Some(&b'(') {
            self.array_words(end + 1, owner)
        } else {
            end
        };
        let var = self.declare(start..name_end, start..extent, owner, false);
        frame.assigned(var);
        Some(extent)
    }

    fn enter_control(&mut self, frame: &mut Frame, mode: State) -> State {
        frame.blocks.push(self.scope);
        self.enter();
        mode
    }

    fn leave_control(&mut self, frame: &mut Frame, keyword: &str) -> State {
        if let Some(enclosing) = frame.blocks.pop() {
            self.scope = enclosing;
        }
        if keyword == "esac" {
            frame.cases = frame.cases.saturating_sub(1);
        }
        State::Args
    }

    fn command_head(&mut self, frame: &mut Frame, word: Word, owner: Owner, plain: bool) -> State {
        let raw = &self.text[word.start..word.end];
        match raw {
            "function" => State::Function,
            "time" => State::Timed,
            "if" | "while" | "until" | "for" | "select" | "case" => self.begin_control(frame, raw),
            "then" | "else" | "elif" | "do" | "!" => State::Command,
            "fi" | "done" | "esac" => self.leave_control(frame, raw),
            _ => self.simple_command(raw, word, owner, plain),
        }
    }

    fn begin_control(&mut self, frame: &mut Frame, keyword: &str) -> State {
        let mode = match keyword {
            "for" | "select" => State::Loop,
            "case" => {
                frame.cases += 1;
                State::Case(CaseWord::Subject)
            }
            _ => State::Command,
        };
        self.enter_control(frame, mode)
    }

    // Once a shell keyword has been consumed, a builtin controls only the roles
    // of its operands. An ordinary command can emit a callable reference.
    fn simple_command(&mut self, raw: &str, word: Word, owner: Owner, plain: bool) -> State {
        let span = word.command_span(self.b, plain);
        let spelling = span.as_ref().map_or(raw, |range| &self.text[range.clone()]);
        if let Some(mode) = State::builtin(spelling) {
            return mode;
        }
        if let Some(span) = span {
            self.usage(span, owner, UseKind::Call);
        }
        State::Args
    }
}
