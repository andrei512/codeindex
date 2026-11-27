//! JSX text and attribute spellings are outside lexical JavaScript lookup.
//! Expression containers retain their normal tokens.
use super::{Token, tok, word};
use std::{collections::HashMap, ops::Range};
#[derive(Default)]
pub(super) struct Markup {
    ignored: Vec<u64>,
    pub sites: HashMap<usize, Site>,
}
#[derive(Clone, Copy)]
pub(super) enum Site {
    Component(Option<usize>),
    Property(usize),
}
impl Markup {
    pub(super) fn ignored(&self, token: usize) -> bool {
        self.ignored
            .get(token / 64)
            .is_some_and(|word| word & (1u64 << (token % 64)) != 0)
    }
    fn ignore_range(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let first = range.start / 64;
        let last = (range.end - 1) / 64;
        if self.ignored.len() <= last {
            self.ignored.resize(last + 1, 0);
        }
        for word in first..=last {
            let left = range.start.saturating_sub(word * 64);
            let right = (range.end - 1 - word * 64).min(63);
            self.ignored[word] |= (u64::MAX << left) & (u64::MAX >> (63 - right));
        }
    }
}
// Scratch is reused across tags. Only a successfully closed tag commits its ranges.
#[derive(Default)]
struct Attributes {
    names: Vec<usize>,
    expressions: Vec<Range<usize>>,
}

// JSX member expressions and intrinsic names share a written name grammar.
// Hyphens and colons keep the whole name in markup, including a mixed suffix.
struct Name {
    end: usize,
    component: bool,
}
impl Name {
    fn read(tokens: &[Token], start: usize, tag: bool) -> Option<Self> {
        let first = tokens.get(start).filter(|t| word(t))?;
        let mut end = start + 1;
        let mut markup = false;
        while let Some(after) = tokens.get(end + 1) {
            let separator = tok(tokens, end);
            if !name_separator(separator, tag) || !word(after) {
                break;
            }
            markup |= separator != ".";
            end += 2;
        }
        let names_component = end > start + 1 || first.text.starts_with(char::is_uppercase);
        let component_name = tag && !markup;
        let component = component_name && names_component;
        Some(Self { end, component })
    }
}

fn name_separator(separator: &str, tag: bool) -> bool {
    matches!(separator, "-" | ":") || (tag && separator == ".")
}

#[derive(Clone, Copy)]
struct IslandTokens<'t, 's> {
    tokens: &'t [Token<'s>],
    pairs: &'t [usize],
}

impl IslandTokens<'_, '_> {
    fn expression_end(self, open: usize) -> Option<usize> {
        self.pairs
            .get(open)
            .copied()
            .filter(|&end| end < self.tokens.len())
    }
}

impl Attributes {
    fn value(&mut self, island: IslandTokens<'_, '_>, at: usize) -> Option<usize> {
        let token = island.tokens.get(at)?;
        if token.kind == super::Kind::String {
            return Some(at + 1);
        }
        let close = (token.text == "{").then(|| island.expression_end(at))??;
        self.expressions.push(at..close + 1);
        Some(close + 1)
    }
    fn read(&mut self, island: IslandTokens<'_, '_>, mut p: usize) -> usize {
        self.names.clear();
        self.expressions.clear();
        loop {
            match self.attribute(island, p) {
                Ok(next) => p = next,
                Err(end) => return end,
            }
        }
    }

    fn attribute(&mut self, island: IslandTokens<'_, '_>, at: usize) -> Result<usize, usize> {
        if tok(island.tokens, at) == "{" {
            return self.value(island, at).ok_or(at);
        }
        let name = Name::read(island.tokens, at, false).ok_or(at)?;
        self.names.extend((name.end == at + 1).then_some(at));
        if tok(island.tokens, name.end) != "=" {
            return Ok(name.end);
        }
        let value = name.end + 1;
        self.value(island, value).ok_or(value)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TagKind {
    Open,
    Close,
    Empty,
}

impl TagKind {
    fn from_markers(closing: bool, empty: bool) -> Self {
        match (closing, empty) {
            (true, _) => Self::Close,
            (_, true) => Self::Empty,
            _ => Self::Open,
        }
    }

    fn depth(self, depth: usize) -> usize {
        match self {
            Self::Close => depth.saturating_sub(1),
            Self::Open => depth + 1,
            Self::Empty => depth,
        }
    }
}

struct Tag {
    end: usize,
    component: Option<Range<usize>>,
    kind: TagKind,
}

impl Tag {
    fn mark(&self, start: usize, attributes: &Attributes, islands: &mut Islands) {
        // The component name precedes all expression containers. Keep both in
        // lexical lookup; the remaining ranges are markup.
        let mut start = if let Some(component) = &self.component {
            islands.markup.ignore_range(start..component.start);
            component.end
        } else {
            start
        };
        for expression in &attributes.expressions {
            islands.markup.ignore_range(start..expression.start);
            start = expression.end;
            islands.expression(expression.start, expression.end - 1);
        }
        islands.markup.ignore_range(start..self.end + 1);
        if let Some(component) = &self.component
            && self.kind != TagKind::Close
            && !attributes.names.is_empty()
        {
            let terminal = component.end - 1;
            islands.markup.sites.insert(terminal, Site::Component(None));
            islands.markup.sites.extend(
                attributes
                    .names
                    .iter()
                    .map(|&attr| (attr, Site::Property(terminal))),
            );
        }
    }

    fn outer_start(t: &[Token], i: usize, closing: bool) -> bool {
        !closing
            && i.checked_sub(1).is_none_or(|before| {
                matches!(
                    tok(t, before),
                    "(" | "[" | "," | "=" | "return" | ":" | ">" | "&" | "|" | "{" | "?" | ";"
                )
            })
    }

    fn read(
        island: IslandTokens<'_, '_>,
        i: usize,
        depth: usize,
        attributes: &mut Attributes,
    ) -> Option<Self> {
        let t = island.tokens;
        let closing = tok(t, i + 1) == "/";
        if depth == 0 && !Self::outer_start(t, i, closing) {
            return None;
        }
        let name = i + 1 + usize::from(closing);
        let (attributes_start, component) = Self::head(t, name)?;
        let after = attributes.read(island, attributes_start);
        let empty = tok(t, after) == "/";
        let p = after + usize::from(empty);
        let kind = TagKind::from_markers(closing, empty);
        if tok(t, p) != ">" {
            return None;
        }
        let named_open = kind == TagKind::Open && name != attributes_start;
        if depth == 0 && Self::type_arguments(t, p, named_open) {
            return None;
        }
        Some(Self {
            end: p,
            component,
            kind,
        })
    }

    fn head(tokens: &[Token], name: usize) -> Option<(usize, Option<Range<usize>>)> {
        if tok(tokens, name) == ">" {
            return Some((name, None));
        }
        let parsed = Name::read(tokens, name, true)?;
        Some((parsed.end, parsed.component.then_some(name..parsed.end)))
    }

    fn type_arguments(tokens: &[Token], end: usize, named_open: bool) -> bool {
        named_open && tok(tokens, end + 1) == "("
    }
}

struct Islands {
    markup: Markup,
    pending: Vec<Range<usize>>,
}
impl Islands {
    // Both child text and attribute values use exactly the interior of a
    // matched JavaScript expression, after its surrounding tag has committed.
    fn expression(&mut self, open: usize, close: usize) -> usize {
        self.pending.push(open + 1..close);
        close + 1
    }
    fn tag(
        &mut self,
        island: IslandTokens<'_, '_>,
        at: usize,
        depth: &mut usize,
        attributes: &mut Attributes,
    ) -> usize {
        let Some(tag) = Tag::read(island, at, *depth, attributes) else {
            return at + 1;
        };
        tag.mark(at, attributes, self);
        *depth = tag.kind.depth(*depth);
        tag.end + 1
    }
    fn scan(&mut self, island: IslandTokens<'_, '_>, first: usize, attributes: &mut Attributes) {
        let tokens = island.tokens;
        let mut depth = 0usize;
        let mut i = first;
        while i < tokens.len() {
            match (tok(tokens, i), depth) {
                ("<", _) => i = self.tag(island, i, &mut depth, attributes),
                (_, 0) => i += 1,
                _ => i = self.child(island, i),
            }
        }
    }

    fn child(&mut self, island: IslandTokens<'_, '_>, at: usize) -> usize {
        let tokens = island.tokens;
        if tok(tokens, at) == "{"
            && let Some(close) = island.expression_end(at)
        {
            return self.expression(at, close);
        }
        let end = tokens[at + 1..]
            .iter()
            .position(|token| matches!(token.text, "<" | "{"))
            .map_or(tokens.len(), |next| at + 1 + next);
        self.markup.ignore_range(at..end);
        end
    }
}

pub(super) fn classify(tokens: &[Token], end: &[usize], first_tag: usize) -> Markup {
    let mut islands = Islands {
        markup: Markup::default(),
        pending: vec![first_tag..tokens.len()],
    };
    let mut attributes = Attributes::default();
    while let Some(region) = islands.pending.pop() {
        let island = IslandTokens {
            tokens: &tokens[..region.end],
            pairs: end,
        };
        islands.scan(island, region.start, &mut attributes);
    }
    islands.markup
}
