use super::*;
use crate::domain::code::Token;
use crate::domain::code::roles::suite_bodies::BodySyntax;
#[derive(PartialEq, Eq)]
enum Registration {
    Test,
    Suite,
}

fn registration(name: &str) -> Option<Registration> {
    match name {
        "it" | "specify" | "test" => Some(Registration::Test),
        "example" | "describe" | "context" | "feature" | "scenario" | "before" | "after"
        | "around" | "let" | "let!" | "subject" => Some(Registration::Suite),
        _ => None,
    }
}

pub(super) fn collect(s: &Syntax, res: &mut Roles, blocks: &std::cell::OnceCell<RubyBlocks>) {
    if res.base.0 != Support {
        return;
    }
    let bodies = BodySyntax {
        tokens: s.tokens,
        pairs: s.pairs,
    };
    let tests = s
        .tokens
        .iter()
        .enumerate()
        .filter_map(|(i, _)| test_registration(s, &bodies, blocks, i));
    for (i, last, reason) in tests {
        s.add(res, i, Some(last), Test.evidence(reason));
    }
}

fn test_registration(
    s: &Syntax,
    bodies: &BodySyntax,
    blocks: &std::cell::OnceCell<RubyBlocks>,
    i: usize,
) -> Option<(usize, usize, &'static str)> {
    let name = s.w(i);
    if name == "def" && s.w(i + 1).starts_with("test_") {
        let spans = blocks.get_or_init(|| RubyBlocks::read(s.tokens));
        return Some((i, spans.0[i], "Ruby test method"));
    }
    if !s.string(i + 1) || registration(name) != Some(Registration::Test) {
        return None;
    }
    let spans = blocks.get_or_init(|| RubyBlocks::read(s.tokens));
    let (_, last) = spans.registration_body(bodies, i)?;
    Some((i, last, "Ruby test block"))
}

/// One pairing of Ruby keyword blocks serves case ranges and control depth.
#[derive(Default)]
pub(in crate::domain::code) struct RubyBlocks(Vec<usize>);

enum Block {
    Do,
    Open,
    End,
}

impl Block {
    fn read(tokens: &[Token], i: usize) -> Option<Self> {
        let token = &tokens[i];
        let previous = i.checked_sub(1).map(|j| &tokens[j]);
        let keyword = match token.text {
            "if" | "unless" | "while" | "until" if begins_statement(token, previous) => Self::Open,
            "do" => Self::Do,
            "class" | "module" | "def" | "case" | "begin" | "for" => Self::Open,
            "end" => Self::End,
            _ => return None,
        };
        keyword_position(token, previous, tokens.get(i + 1)).then_some(keyword)
    }
}

fn begins_statement(token: &Token, previous: Option<&Token>) -> bool {
    previous.is_none_or(|p| {
        p.line < token.line || matches!(p.text, ";" | "=" | "(" | "then" | "else" | "do")
    })
}

fn keyword_position(token: &Token, previous: Option<&Token>, next: Option<&Token>) -> bool {
    let method_name = previous.is_some_and(|p| match p.text {
        "." | "def" => true,
        ":" => p.span.end == token.span.start,
        _ => false,
    });
    let label = next.is_some_and(|p| p.text == ":" && p.span.start == token.span.end);
    let word = token.kind == crate::domain::code::Kind::Word;
    word && !(method_name || label)
}

fn qualified(s: &BodySyntax, i: usize) -> bool {
    !s.unqualified(i) || namespace_member(s, i)
}

fn namespace_member(s: &BodySyntax, i: usize) -> bool {
    i.checked_sub(2)
        .is_some_and(|j| s.word(j) == ":" && s.word(j + 1) == ":")
}

fn registration_end(s: &BodySyntax, i: usize) -> usize {
    let mut j = i + 1;
    let line = s.tokens[i].line;
    while s.tokens.get(j).is_some_and(|token| token.line == line)
        && !matches!(s.word(j), "do" | "{" | ";" | "end")
    {
        j = next_argument(s, j);
    }
    j
}

fn next_argument(s: &BodySyntax, j: usize) -> usize {
    if matches!(s.word(j), "(" | "[") && s.pairs[j] < s.tokens.len() {
        s.pairs[j] + 1
    } else {
        j + 1
    }
}

fn loop_opener(tokens: &[Token], previous: usize, current: &Token) -> bool {
    tokens[previous].line == current.line
        && matches!(tokens[previous].text, "while" | "until" | "for")
}

impl RubyBlocks {
    pub(in crate::domain::code) fn control_end(&self, tokens: &[Token], i: usize) -> Option<usize> {
        let close = self.0[i];
        (close < tokens.len()
            && matches!(
                tokens[i].text,
                "if" | "unless" | "for" | "while" | "until" | "case" | "begin"
            ))
        .then_some(close)
    }

    pub(in crate::domain::code::roles) fn body(
        &self,
        s: &BodySyntax,
        i: usize,
    ) -> Option<(usize, usize)> {
        registration(s.word(i))?;
        self.registration_body(s, i)
    }

    fn registration_body(&self, s: &BodySyntax, i: usize) -> Option<(usize, usize)> {
        if qualified(s, i) {
            return None;
        }
        let j = registration_end(s, i);
        let last = match s.word(j) {
            "do" => self.0[j],
            "{" => s.pairs[j],
            _ => return None,
        };
        (last < s.tokens.len()).then_some((j, last))
    }
    // Pair Ruby block keywords in token order so close delimiters work on a
    // single line and inside an enclosing branch regardless of indentation.
    pub(in crate::domain::code) fn read(t: &[Token]) -> Self {
        let mut ends = vec![usize::MAX; t.len()];
        let mut stack: Vec<usize> = Vec::new();
        let blocks = t
            .iter()
            .enumerate()
            .filter_map(|(i, _)| Some((i, Block::read(t, i)?)));
        for (i, block) in blocks {
            match block {
                Block::Do if stack.last().is_some_and(|&j| loop_opener(t, j, &t[i])) => (),
                Block::Do | Block::Open => stack.push(i),
                Block::End => close_block(&mut stack, &mut ends, i),
            }
        }
        Self(ends)
    }
}

fn close_block(stack: &mut Vec<usize>, ends: &mut [usize], i: usize) {
    if let Some(j) = stack.pop() {
        ends[j] = i;
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::code::roles::classify;

    #[test]
    fn unqualified_cases_and_methods_own_their_full_keyword_block() {
        let text = "it 'x' do\nif ready\n work(/for/, :end) # end\nend\n verify\nend\nreporter.it 'message' do; work; end\ndef test_me\nwhile ready do\n work\nend\n verify\nend\n";
        let roles = classify("spec/example_spec.rb", text);
        assert_eq!(
            roles
                .regions
                .iter()
                .map(|r| (r.first, r.last))
                .collect::<Vec<_>>(),
            [(1, 6), (8, 13)]
        );
    }
}
