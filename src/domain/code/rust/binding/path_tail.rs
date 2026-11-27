//! A written import can prepend path components to a borrowed lookup suffix.
//! Frames live only for the recursive lookup that consumes them.
#[derive(Clone, Copy)]
pub(super) enum Tail<'a> {
    Slice(&'a [String]),
    Prefix(&'a [String], &'a Self),
    Name(&'a str, &'a Self),
}

impl<'a> Tail<'a> {
    pub(super) fn prepend(names: &'a [String], rest: &'a Self) -> Self {
        if names.is_empty() {
            *rest
        } else if rest.is_empty() {
            Self::Slice(names)
        } else {
            Self::Prefix(names, rest)
        }
    }

    pub(super) fn is_empty(self) -> bool {
        matches!(self, Self::Slice([]))
    }

    pub(super) fn split_first(self) -> Option<(&'a str, Self)> {
        match self {
            Self::Slice(names) => names
                .split_first()
                .map(|(name, tail)| (name.as_str(), Self::Slice(tail))),
            Self::Name(name, tail) => Some((name, *tail)),
            Self::Prefix(names, tail) => {
                let (name, more) = names.split_first()?;
                Some((name, Self::prepend(more, tail)))
            }
        }
    }
}
