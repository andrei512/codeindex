//! Named fields and enum variants collected by the item extractor.
use super::*;

impl Extractor<'_, '_> {
    pub(super) fn fields(&mut self, owner: usize, open: usize, is_enum: bool) {
        let (t, pairs) = (self.tokens, self.pairs);
        let close = pairs[open].min(t.len());
        let mut i = open + 1;
        while i < close {
            i = self.field(owner, i, close, is_enum);
        }
    }

    fn field(&mut self, owner: usize, i: usize, close: usize, is_enum: bool) -> usize {
        let (t, pairs) = (self.tokens, self.pairs);
        if let Some(after) = field_prefix(t, pairs, i) {
            return after;
        }
        let named = is_enum || word(t, i + 1) == ":";
        if named && ident(word(t, i)) {
            self.record_field(owner, i, is_enum);
        }
        next_field(t, pairs, i, close)
    }
    fn record_field(&mut self, owner: usize, i: usize, is_enum: bool) {
        let t = self.tokens;
        let id = self.analysis.declarations.len();
        self.analysis.declarations.push(Declaration {
            span: t[i].span.clone(),
            kind: if is_enum {
                "enum_variant"
            } else {
                "field_declaration"
            },
        });
        self.evidence.members.definitions.push((owner, id));
        self.excluded[i] = true;
        if !is_enum {
            self.evidence.members.record_type(id, self.scope, t, i + 2);
        } else if word(t, i + 1) == "{" {
            self.fields(id, i + 1, false);
        }
    }
}

fn field_prefix(t: &[Token], pairs: &[usize], i: usize) -> Option<usize> {
    let next = i + 1;
    match word(t, i) {
        "#" if word(t, next) == "[" => after_group(pairs, next),
        "pub" if word(t, next) == "(" => Some(after_group(pairs, next).unwrap_or(next)),
        "pub" => Some(next),
        _ => None,
    }
}

fn next_field(t: &[Token], pairs: &[usize], i: usize, close: usize) -> usize {
    let mut depth = 0usize;
    let end = top_level(t, pairs, i..close, |_, token| match token {
        "<" => {
            depth += 1;
            false
        }
        ">" => {
            depth = depth.saturating_sub(1);
            false
        }
        "," => depth == 0,
        _ => false,
    });
    if end < close {
        return end + 1;
    }
    end
}
