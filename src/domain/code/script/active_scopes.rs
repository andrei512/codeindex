//! Lexical scopes close at a delimiter in ECMAScript and on dedent in Python.
use super::Token;

#[derive(Default)]
pub(super) struct ActiveScopes {
    frames: Vec<Frame>,
    indentation: Option<(usize, usize)>,
}

struct Frame {
    scope: usize,
    boundary: usize,
}

impl ActiveScopes {
    pub fn enter(&mut self, scope: usize, boundary: usize) {
        self.frames.push(Frame { scope, boundary });
    }

    pub fn indentation(&mut self, source: &str, token: &Token) -> usize {
        if let Some((line, indent)) = self.indentation
            && line == token.line
        {
            return indent;
        }
        let indent = super::super::indentation(source, token.span.start);
        self.indentation = Some((token.line, indent));
        indent
    }

    pub fn enclosing(&mut self, python: bool, source: &str, tokens: &[Token], i: usize) -> usize {
        if self.frames.is_empty() {
            return 0;
        }
        if python {
            self.dedent(source, tokens, i);
        } else {
            while self.frames.last().is_some_and(|frame| frame.boundary <= i) {
                self.frames.pop();
            }
        }
        self.frames.last().map_or(0, |frame| frame.scope)
    }

    fn dedent(&mut self, source: &str, tokens: &[Token], i: usize) {
        let token = &tokens[i];
        let same_line = i
            .checked_sub(1)
            .is_some_and(|previous| tokens[previous].line == token.line);
        if same_line {
            return;
        }
        let indent = self.indentation(source, token);
        while self
            .frames
            .last()
            .is_some_and(|frame| frame.boundary >= indent)
        {
            self.frames.pop();
        }
    }
}
