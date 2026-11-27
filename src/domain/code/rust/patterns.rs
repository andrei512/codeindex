use super::*;

/// An arm may begin at the exact token where its previous sibling ends.
/// The entry scope wins there. Nested expression closures sharing an end
/// restore the first scheduled (outermost) parent.
pub(super) struct Transitions {
    next: usize,
    changes: std::collections::BTreeMap<usize, Change>,
}

enum Change {
    Enter(usize),
    Leave(usize),
}

impl Default for Transitions {
    fn default() -> Self {
        Self {
            next: usize::MAX,
            changes: Default::default(),
        }
    }
}

impl Transitions {
    pub fn enter(&mut self, token: usize, scope: usize) {
        self.next = self.next.min(token);
        self.changes.insert(token, Change::Enter(scope));
    }

    pub fn leave_arm(&mut self, token: usize, scope: usize) {
        self.next = self.next.min(token);
        let slot = self.changes.entry(token).or_insert(Change::Leave(scope));
        if let Change::Leave(parent) = slot {
            *parent = scope;
        }
    }

    pub fn leave_closure(&mut self, token: usize, scope: usize) {
        self.next = self.next.min(token);
        self.changes.entry(token).or_insert(Change::Leave(scope));
    }

    pub fn at(&mut self, token: usize) -> Option<usize> {
        while self.next <= token {
            let (position, change) = self.changes.pop_first()?;
            self.next = self
                .changes
                .first_key_value()
                .map_or(usize::MAX, |(&next, _)| next);
            if position == token {
                let (Change::Enter(scope) | Change::Leave(scope)) = change;
                return Some(scope);
            }
            // The parser jumped over this token (for example an opaque macro).
        }
        None
    }
}

fn arm_bounds(
    t: &[Token],
    pairs: &[usize],
    first: usize,
    close: usize,
) -> Option<(std::ops::Range<usize>, usize)> {
    let mut guard = None;
    let arrow = top_level(t, pairs, first..close, |i, token| {
        if token == "if" {
            guard.get_or_insert(i);
        }
        token == "=" && word(t, i + 1) == ">"
    });
    if arrow >= close {
        return None;
    }
    let expr = arrow + 2;
    let end = if word(t, expr) == "{" && pairs[expr] != usize::MAX {
        pairs[expr] + 1
    } else {
        until_before(t, pairs, expr..close, &[",", "}"])
    };
    Some((first..guard.unwrap_or(arrow), end))
}

/// A block arm's comma is optional; expression arms stop at a top-level comma.
pub(super) struct Arm {
    pub first: usize,
    pub pattern: std::ops::Range<usize>,
    pub end: usize,
}

pub(super) fn arms<'a>(
    t: &'a [Token],
    pairs: &'a [usize],
    open: usize,
) -> impl Iterator<Item = Arm> + 'a {
    let close = pairs[open];
    let mut first = open + 1;
    std::iter::from_fn(move || {
        if close == usize::MAX {
            return None;
        }
        while first < close && word(t, first) == "," {
            first += 1;
        }
        if first >= close {
            return None;
        }
        let (pattern, end) = arm_bounds(t, pairs, first, close)?;
        let arm = Arm {
            first,
            pattern,
            end,
        };
        first = end;
        Some(arm)
    })
}

pub(super) struct Pattern<'a> {
    tokens: &'a [Token<'a>],
    range: std::ops::Range<usize>,
}

fn binding_name(spelling: &str) -> bool {
    (ident(spelling) || spelling == "self")
        && name(spelling).starts_with(|c: char| c == '_' || c.is_lowercase())
}

impl<'a> Pattern<'a> {
    pub fn new(tokens: &'a [Token<'a>], range: std::ops::Range<usize>) -> Self {
        Self { tokens, range }
    }

    pub fn binds(&self, i: usize) -> bool {
        let t = self.tokens;
        let spelling = word(t, i);
        if !binding_name(spelling) {
            return false;
        }
        let previous = i.checked_sub(1).map_or("", |j| word(t, j));
        match previous {
            "'" | "." => return false,
            ":" if i > 1 && word(t, i - 2) == ":" => return false,
            _ => {}
        }
        i + 1 >= self.range.end || word(t, i + 1) != ":"
    }

    pub fn declarations(&self) -> impl Iterator<Item = (usize, &'a str)> + '_ {
        self.range
            .clone()
            .filter(|&i| self.binds(i))
            .map(|i| (i, word(self.tokens, i)))
    }

    pub fn has_alternatives(&self) -> bool {
        self.tokens
            .get(self.range.clone())
            .is_some_and(|tokens| tokens.iter().any(|token| token.text == "|"))
    }
}
