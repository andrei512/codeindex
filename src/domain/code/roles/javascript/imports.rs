//! Stage an import's bindings until the complete clause has been recognized.
use super::{HashSet, Syntax, ident, literal};

pub(super) struct Clause<'s, 'buf> {
    pub module: &'s str,
    pub names: &'buf [(&'s str, &'s str)],
    pub from: usize,
}

struct Cursor<'s> {
    syntax: &'s Syntax<'s>,
    at: usize,
}

impl<'s> Cursor<'s> {
    fn take(&mut self, token: &str) -> bool {
        if self.syntax.w(self.at) != token {
            return false;
        }
        self.at += 1;
        true
    }

    fn name(&mut self) -> Option<&'s str> {
        if !ident(self.syntax, self.at) {
            return None;
        }
        self.at += 1;
        Some(self.syntax.w(self.at - 1))
    }

    fn named(&mut self, names: &mut Vec<(&'s str, &'s str)>) -> Option<()> {
        let mut start = self.at - 1;
        let end = self.syntax.close(start)?;
        let tokens = &self.syntax.tokens[..=end];
        crate::domain::code::script::js_named_specifiers(
            tokens,
            &mut start,
            |original, local, type_only| {
                if !type_only {
                    names.push((tokens[local].text, tokens[original].text));
                }
            },
        )?;
        self.at = start;
        Some(())
    }
}

impl<'s, 'buf> Clause<'s, 'buf> {
    pub fn parse(
        s: &'s Syntax,
        start: usize,
        names: &'buf mut Vec<(&'s str, &'s str)>,
    ) -> Option<Self> {
        let mut cursor = Cursor {
            syntax: s,
            at: start,
        };
        if cursor.take("type") {
            return None;
        }
        names.clear();
        if let Some(local) = cursor.name() {
            names.push((local, "default"));
            cursor.take(",");
        }
        if cursor.take("*") {
            cursor.take("as").then_some(())?;
            names.push((cursor.name()?, "*"));
        } else if cursor.take("{") {
            cursor.named(names)?;
        } else if names.is_empty() {
            return None;
        }
        let from = cursor.at;
        cursor.take("from").then_some(())?;
        Some(Self {
            module: literal(s, cursor.at),
            names,
            from,
        })
    }
}

/// Imports are hoisted, so a call can precede its declaration.
pub(super) fn retain_used(s: &Syntax, imports: &mut Vec<(&str, &str, &str)>) {
    let mut candidates = super::call_candidates(s);
    if let [(_, _, local)] = imports.as_slice() {
        if !candidates.any(|head| head.name == *local && head.relative(s)) {
            imports.clear();
        }
        return;
    }
    if imports.is_empty() {
        return;
    }
    retain_used_group(s, imports, candidates);
}

fn retain_used_group<'a>(
    s: &Syntax,
    imports: &mut Vec<(&str, &str, &str)>,
    candidates: impl Iterator<Item = super::CallHead<'a>>,
) {
    let mut unused: HashSet<_> = imports.iter().map(|&(_, _, local)| local).collect();
    let complete = candidates
        .filter(|head| head.relative(s))
        .any(|head| unused.remove(head.name) && unused.is_empty());
    if complete {
        return;
    }
    imports.retain(|&(_, _, local)| !unused.contains(local));
}
