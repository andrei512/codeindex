//! PHP and Hack namespace changes in source order. Unqualified files need no entries.
use super::*;

#[derive(Default)]
pub(super) struct Namespaces {
    changes: Vec<(usize, usize)>,
    pub(super) blocks: HashSet<usize>,
}

impl Namespaces {
    pub fn new(tokens: &[Token], pairs: &[usize], starts: Vec<usize>) -> Self {
        let mut result = Self::default();
        let mut names = HashMap::new();
        let mut close = None;
        for at in starts {
            result.expire(&mut close, at);
            close = result
                .declaration(tokens, pairs, (at, &mut names))
                .unwrap_or(close);
        }
        if let Some(end) = close {
            result.change(end, ROOT);
        }
        result
    }

    fn expire(&mut self, active: &mut Option<usize>, at: usize) {
        if let Some(end) = active.filter(|&end| end <= at) {
            self.change(end, ROOT);
            *active = None;
        }
    }

    fn declaration(
        &mut self,
        tokens: &[Token],
        pairs: &[usize],
        (at, names): (usize, &mut HashMap<String, usize>),
    ) -> Option<Option<usize>> {
        let start = at + 1;
        let open = namespace_head(tokens, start)?;
        let region = namespace_id(&tokens[start..open], at, names);
        let close = match tokens.get(open)?.text {
            ";" => None,
            "{" => {
                self.blocks.insert(open);
                Some(pairs[open].min(tokens.len()))
            }
            _ => return None,
        };
        self.change(at, region);
        Some(close)
    }

    fn change(&mut self, at: usize, region: usize) {
        if self.changes.last().map_or(ROOT, |&(_, last)| last) != region {
            self.changes.push((at, region));
        }
    }

    pub fn at(&self, token: usize) -> usize {
        let end = self.changes.partition_point(|&(at, _)| at <= token);
        end.checked_sub(1).map_or(ROOT, |i| self.changes[i].1)
    }
}

fn namespace_head(tokens: &[Token], start: usize) -> Option<usize> {
    let head = tokens.get(start)?;
    // The anonymous form must be followed immediately by its opening brace.
    if head.kind != Kind::Word && head.text != "{" {
        return None;
    }
    let name = tokens[start..]
        .iter()
        .take_while(|t| t.kind == Kind::Word || t.text == "\\")
        .count();
    Some(start + name)
}

fn namespace_id(tokens: &[Token], at: usize, names: &mut HashMap<String, usize>) -> usize {
    if tokens.is_empty() {
        return ROOT;
    }
    let mut name: String = tokens.iter().map(|t| t.text).collect();
    name.make_ascii_lowercase();
    *names.entry(name).or_insert(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_namespace_resumes_its_identity_and_braced_scope_ends_at_close() {
        let php = "<?php namespace A; function first() {} namespace Other { function middle() {} } function outer() {} namespace a; function again() {}";
        let s = Syntax::new(Language::Php, php);
        let scope = |name| {
            s.namespaces
                .at(s.t.iter().position(|t| t.text == name).unwrap())
        };
        assert_ne!(scope("first"), ROOT);
        assert_ne!(scope("first"), scope("middle"));
        assert_eq!(scope("first"), scope("again"));
        assert_eq!(scope("outer"), ROOT);
        let s = Syntax::new(Language::Php, "<?php function stand_alone() {}");
        assert!(s.namespaces.changes.is_empty());
    }
}
