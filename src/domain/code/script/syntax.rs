use super::angle_ends::AngleEnds;
use super::{Analysis, Binding, Evidence, Language, Lookup, Token, UseRequest, tok, word};
pub(super) fn paired(t: &[Token]) -> Vec<usize> {
    pair_delimiters(t, |_, _, _, _| {})
}

#[derive(Clone, Copy)]
pub(super) enum DelimiterEvent {
    Open,
    Close,
    Token,
}

pub(super) fn pair_delimiters(
    t: &[Token],
    mut observe: impl FnMut(usize, &str, &[usize], DelimiterEvent),
) -> Vec<usize> {
    let mut stack = Vec::new();
    let mut end = vec![t.len(); t.len()];
    for (i, token) in t.iter().enumerate() {
        let event = match token.text {
            "(" | "[" | "{" => {
                stack.push(i);
                DelimiterEvent::Open
            }
            ")" | "]" | "}" => {
                if let Some(&j) = stack.last()
                    && matches!(
                        (t[j].text, token.text),
                        ("(", ")") | ("[", "]") | ("{", "}")
                    )
                {
                    stack.pop();
                    end[j] = i;
                    end[i] = j;
                    DelimiterEvent::Close
                } else {
                    DelimiterEvent::Token
                }
            }
            _ => DelimiterEvent::Token,
        };
        observe(i, token.text, &stack, event);
    }
    end
}
/// Consecutive members share the same start byte even when whitespace
/// separates their dots. A call or another expression starts a fresh prefix.
#[derive(Default)]
pub(super) struct DottedPrefix {
    last: Option<(usize, usize)>,
}

impl DottedPrefix {
    pub(super) fn qualifier(
        &mut self,
        tokens: &[Token],
        member: usize,
    ) -> Option<std::ops::Range<usize>> {
        let base = member.checked_sub(2)?;
        if !word(&tokens[base]) {
            return None;
        }
        let start = match self.last {
            Some((last, start)) if last == base => start,
            _ => {
                let mut first = base;
                while let Some(previous) = dotted_prefix_token(tokens, first) {
                    first = previous;
                }
                tokens[first].span.start
            }
        };
        self.last = Some((member, start));
        Some(start..tokens[base].span.end)
    }
}

fn dotted_prefix_token(tokens: &[Token], at: usize) -> Option<usize> {
    let base = at.checked_sub(2)?;
    (tokens[at - 1].text == "." && word(&tokens[base])).then_some(base)
}

/// The name immediately before a member dot can supply an unqualified
/// receiver. Longer chains require a prior resolved occurrence.
pub(super) fn simple_receiver<'a, 's>(
    tokens: &'a [Token<'s>],
    member: usize,
) -> Option<&'a Token<'s>> {
    let at = member.checked_sub(2)?;
    let receiver = &tokens[at];
    (word(receiver) && (at == 0 || tokens[at - 1].text != ".")).then_some(receiver)
}

fn after_type_arguments(angles: &mut AngleEnds, at: usize) -> usize {
    if tok(angles.tokens, at) == "<" {
        angles.following(at).map_or(at, |n| n + 1)
    } else {
        at
    }
}

/// End of a name followed by zero or more literal dot-qualified names.
pub(super) fn dotted_name_end(tokens: &[Token], start: usize) -> usize {
    let mut next = start + 1;
    while tok(tokens, next) == "." && tokens.get(next + 1).is_some_and(word) {
        next += 2;
    }
    next
}

pub(super) fn type_body(angles: &mut AngleEnds, after_name: usize) -> Option<usize> {
    let t = angles.tokens;
    let mut p = after_type_arguments(angles, after_name);
    if tok(t, p) == "=" {
        p += 1;
    }
    while matches!(tok(t, p), "extends" | "implements" | ",") {
        p += 1;
        if !t.get(p).is_some_and(word) {
            break;
        }
        p = after_type_arguments(angles, dotted_name_end(t, p));
    }
    (tok(t, p) == "{").then_some(p)
}

pub(super) fn python_body(t: &[Token], close: usize) -> Option<usize> {
    let first = close + 1;
    let body = first
        + t.get(first..)
            .unwrap_or_default()
            .iter()
            .take_while(|token| token.line <= t[close].line + 1 && !matches!(token.text, ":" | ";"))
            .count();
    (tok(t, body) == ":").then_some(body)
}

enum AnnotationStep {
    Group,
    Generic,
    Arrow,
    Token,
    End,
}

impl AnnotationStep {
    fn read(token: &str, next: &str, type_atom: bool) -> Self {
        match token {
            "(" | "[" => Self::Group,
            "{" => Self::brace(type_atom),
            "<" => Self::Generic,
            "=" => Self::assignment(next),
            ";" | "}" | "," | ")" | "export" | "declare" | "function" => Self::End,
            _ => Self::Token,
        }
    }

    fn brace(type_atom: bool) -> Self {
        if type_atom { Self::Group } else { Self::End }
    }

    fn assignment(next: &str) -> Self {
        if next == ">" { Self::Arrow } else { Self::End }
    }
}

pub(super) enum SignatureEnd {
    Body(usize),
    Declaration(usize),
}

impl SignatureEnd {
    pub(super) fn parse(angles: &mut AngleEnds, pairs: &[usize], close: usize) -> Self {
        let t = angles.tokens;
        let after = close.saturating_add(1);
        let (p, fallback) = if tok(t, after) == ":" {
            Self::annotation_end(angles, pairs, after + 1)
        } else {
            (after, None)
        };
        if tok(t, p) == "{" {
            Self::Body(p)
        } else {
            Self::Declaration(fallback.unwrap_or(p))
        }
    }

    fn annotation_end(
        angles: &mut AngleEnds,
        pairs: &[usize],
        mut p: usize,
    ) -> (usize, Option<usize>) {
        let t = angles.tokens;
        let mut fallback = None;
        // A braced type can be the first atom or the operand of a type
        // operator. Only a brace after a completed type introduces the body.
        let mut type_atom = true;
        while p < t.len() {
            let token = tok(t, p);
            let step = AnnotationStep::read(token, tok(t, p + 1), type_atom);
            let next = match step {
                AnnotationStep::Group => pairs[p] + 1,
                AnnotationStep::Generic => {
                    // Overload recovery treats an incomplete generic as ordinary tokens.
                    fallback.get_or_insert(p);
                    let Some(end) = angles.following(p) else {
                        break;
                    };
                    end + 1
                }
                AnnotationStep::Arrow => p + 2,
                AnnotationStep::Token => p + 1,
                AnnotationStep::End => break,
            };
            type_atom = matches!(step, AnnotationStep::Arrow)
                || matches!(token, "|" | "&" | "?" | ":" | "extends");
            p = next;
        }
        (p, fallback)
    }
    pub(super) fn body(&self) -> Option<usize> {
        match *self {
            Self::Body(i) => Some(i),
            Self::Declaration(_) => None,
        }
    }
}

mod bindings;
pub(super) use bindings::{CallableSignature, ScopeFacts};
