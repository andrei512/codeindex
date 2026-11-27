//! Declarations and overloads must be complete before resolving uses. Keep just
//! the eligible names in source order, together with their lexical scopes.
use std::ops::Range;

#[derive(Default)]
pub(super) struct PendingUses {
    names: Vec<(usize, usize)>,
    expression: Vec<(Range<usize>, usize)>,
}

impl PendingUses {
    pub fn push(&mut self, token: usize, lexical: usize) {
        // Token positions advance. Only the newest containing expression wins.
        // Older expired ranges can wait until they reach the top or a declaration.
        while self.expression.last().is_some_and(|(range, _)| range.end <= token) {
            self.expression.pop();
        }
        let owner = self.expression.iter().rev()
            .find_map(|(range, scope)| range.contains(&token).then_some(*scope))
            .unwrap_or(lexical);
        self.names.push((token, owner));
    }

    /// A C# expression-bodied member owns subsequent uses in its body. The
    /// most recently declared containing body wins. Forget finished bodies as
    /// the parser advances, including bodies that contained no names at all.
    pub fn expression(&mut self, declaration: usize, range: Range<usize>, scope: usize) {
        self.expression.retain(|(r, _)| r.end > declaration);
        if !range.is_empty() {
            self.expression.push((range, scope));
        }
    }

    pub fn scopes(self) -> impl Iterator<Item = (usize, usize)> {
        self.names.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_expression_bodies_restore_the_enclosing_scope() {
        let ranges = [0..3, 3..9, 1..8, 4..6, 2..2];
        for a in &ranges {
            for b in &ranges {
                for c in &ranges {
                    check_overlapping_bodies(a, b, c);
                }
            }
        }
    }
    fn check_overlapping_bodies(a: &Range<usize>, b: &Range<usize>, c: &Range<usize>) {
        let mut scopes = PendingUses::default();

        let mut expected = [0, 0, 1, 1, 1, 1, 0, 0, 0, 0];
        for (r, id) in [(a, 2), (b, 3), (c, 4)] {
            scopes.expression(0, r.clone(), id);
            expected[r.clone()].fill(id);
        }
        for i in 0..10 {
            scopes.push(i, usize::from((2..6).contains(&i)));
        }
        for ((i, actual), wanted) in scopes.scopes().zip(expected) {
            assert_eq!(actual, wanted, "{a:?} {b:?} {c:?} token {i}");
        }
    }
}
