//! File bindings are the fallback. Disjoint local scopes can be searched by
//! position; overlapping scopes use lexical specificity and declaration order.
use super::Binding;
use std::ops::Range;

#[derive(Clone, Copy)]
pub(super) enum Resolved {
    Local(Binding),
    File(Binding),
    Absent,
}

impl Resolved {
    pub(super) fn binding(self) -> Option<Binding> {
        match self {
            Self::Local(binding) | Self::File(binding) => Some(binding),
            Self::Absent => None,
        }
    }
}

struct Local {
    scope: Range<usize>,
    binding: Binding,
    furthest_end: usize,
}

#[derive(Default)]
pub(super) struct ScopedBindings {
    pub(super) file: Option<Binding>,
    local: Vec<Local>,
    starts_out_of_order: bool,
    mixed: bool,
}

impl ScopedBindings {
    pub(super) fn bind_local(&mut self, scope: Range<usize>, binding: Binding) {
        debug_assert!(!scope.is_empty());
        let mut furthest_end = scope.end;
        if let Some(previous) = self.local.last() {
            furthest_end = furthest_end.max(previous.furthest_end);
            self.starts_out_of_order |= previous.scope.start > scope.start;
            self.mixed |= previous.binding != binding;
        }
        self.local.push(Local {
            scope,
            binding,
            furthest_end,
        });
    }

    pub(super) fn at(&self, at: usize, file_end: usize) -> Resolved {
        self.local_at(at)
            .map(Resolved::Local)
            .or_else(|| self.file.filter(|_| at < file_end).map(Resolved::File))
            .unwrap_or(Resolved::Absent)
    }

    fn local_at(&self, at: usize) -> Option<Binding> {
        // Even when declarations arrive out of order, no range in this prefix
        // reaches the use. In source order we can also stop at the next scope.
        let first = self.local.partition_point(|local| local.furthest_end <= at);
        let mut containing = self.local[first..]
            .iter()
            .take_while(|local| self.starts_out_of_order || local.scope.start <= at)
            .filter(|local| local.scope.contains(&at));
        // Shadowing declarations often all mean Other. Their relative scope
        // widths cannot change the answer; any containing scope proves it.
        let best = if self.mixed {
            // Prefer the latest declaration when two scopes have the same width.
            containing.max_by_key(|local| std::cmp::Reverse(local.scope.len()))
        } else {
            containing.next()
        };
        best.map(|local| local.binding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_agrees_with_the_narrowest_latest_containing_declaration() {
        let end = 5;
        let ranges = [0..5, 1..1, 1..2, 2..4, 1..4, 0..2, 3..5];
        for a in &ranges {
            for b in &ranges {
                for c in &ranges {
                    let declared = [(a, Binding::Case), (b, Binding::Suite), (c, Binding::Other)];
                    verify_lookup(end, declared);
                }
            }
        }
    }

    fn verify_lookup(end: usize, declared: [(&Range<usize>, Binding); 3]) {
        let mut scopes = super::super::Scopes {
            file_end: end,
            ..Default::default()
        };
        for (range, binding) in declared {
            scopes.bind(range.clone(), "test", binding);
        }
        let scopes = scopes.with_locals();
        for at in 0..=end {
            let expected = declared
                .iter()
                .rev()
                .filter(|(r, _)| r.contains(&at))
                .min_by_key(|(r, _)| r.len())
                .map(|(_, kind)| *kind as u8);
            let found = scopes
                .names
                .get("test")
                .and_then(|b| b.at(at, end).binding());
            assert_eq!(found.map(|v| v as u8), expected);
        }
    }
}
