//! Walk declaration boundaries once for the scoring passes that own tokens.
use std::iter::Peekable;
use std::ops::Range;

pub(super) struct Opening<T> {
    pub start: usize,
    pub end: usize,
    pub owner: T,
}

pub(super) fn with_registrations<T>(
    declarations: impl Iterator<Item = Opening<T>>,
    registrations: impl Iterator<Item = Opening<T>>,
) -> impl Iterator<Item = Opening<T>> {
    let mut declarations = declarations.peekable();
    // The suite pass admits one registration at each opening token. Preserve
    // its prefix when malformed syntax produces two registrations for one brace.
    let mut previous = None;
    let mut registrations = registrations
        .take_while(move |item| previous.replace(item.start) != Some(item.start))
        .peekable();
    std::iter::from_fn(move || match (declarations.peek(), registrations.peek()) {
        (Some(decl), Some(registration)) if registration.start < decl.start => registrations.next(),
        (Some(_), _) => declarations.next(),
        (None, _) => registrations.next(),
    })
}

pub(super) struct OwnerCursor<T> {
    position: usize,
    limit: usize,
    stack: Vec<Opening<T>>,
    file: T,
}

impl<T> OwnerCursor<T> {
    pub(super) fn new(limit: usize, file: T) -> Self {
        Self {
            position: 0,
            limit,
            stack: Vec::new(),
            file,
        }
    }

    pub(super) fn next_run(
        &mut self,
        openings: &mut Peekable<impl Iterator<Item = Opening<T>>>,
        mut finish: impl FnMut(T),
    ) -> Option<(&mut T, Range<usize>)> {
        let start = self.position;
        if start == self.limit {
            return None;
        }
        while self.stack.last().is_some_and(|unit| unit.end <= start) {
            finish(self.stack.pop().unwrap().owner);
        }
        while let Some(unit) = openings.next_if(|unit| unit.start == start) {
            self.stack.push(unit);
        }
        let next = openings.peek().map_or(self.limit, |unit| unit.start);
        let (owner, end) = self
            .stack
            .last_mut()
            .map_or((&mut self.file, self.limit), |unit| {
                (&mut unit.owner, unit.end)
            });
        self.position = next.min(end).min(self.limit).max(start + 1);
        Some((owner, start..self.position))
    }

    pub(super) fn finish(self, mut finish: impl FnMut(T)) -> T {
        for unit in self.stack.into_iter().rev() {
            finish(unit.owner);
        }
        self.file
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_before_opening_and_gives_registrations_the_shared_start() {
        let open = |start, end, owner| Opening { start, end, owner };
        let declarations = [
            open(0, 2, "first"),
            open(2, 6, "outer"),
            open(4, 5, "inner"),
        ];
        let registrations = [open(2, 4, "registration")];
        let mut starts =
            with_registrations(declarations.into_iter(), registrations.into_iter()).peekable();
        let mut owners = OwnerCursor::new(7, "<file>");
        let mut finished = Vec::new();
        for (name, span, retired) in [
            ("first", 0..2, &[][..]),
            ("registration", 2..4, &["first"][..]),
            ("inner", 4..5, &["first", "registration"][..]),
            ("outer", 5..6, &["first", "registration", "inner"][..]),
            (
                "<file>",
                6..7,
                &["first", "registration", "inner", "outer"][..],
            ),
        ] {
            let (owner, run) = owners
                .next_run(&mut starts, |owner| finished.push(owner))
                .unwrap();
            assert_eq!((*owner, run), (name, span));
            assert_eq!(finished, retired);
        }
        assert!(owners.next_run(&mut starts, |_| unreachable!()).is_none());
    }
}
