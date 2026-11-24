//! A scope usually has one declaration with a given spelling.
use super::Binding;
use crate::domain::code::cache::{Wire, bad};
use std::{io, ops::Deref};

#[derive(Default)]
pub(super) enum Bindings {
    #[default]
    Empty,
    One(Binding),
    Many(Vec<Binding>),
}

impl Bindings {
    pub(super) fn push(&mut self, binding: Binding) {
        match self {
            Self::Empty => *self = Self::One(binding),
            Self::One(first) => *self = Self::Many(vec![*first, binding]),
            Self::Many(items) => items.push(binding),
        }
    }
}

impl Deref for Bindings {
    type Target = [Binding];
    fn deref(&self) -> &[Binding] {
        match self {
            Self::Empty => &[],
            Self::One(item) => std::slice::from_ref(item),
            Self::Many(items) => items,
        }
    }
}

impl<'a> IntoIterator for &'a Bindings {
    type Item = &'a Binding;
    type IntoIter = std::slice::Iter<'a, Binding>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Wire for Bindings {
    fn save(&self, out: &mut impl crate::domain::code::cache::Output) {
        self.len().save(out);
        for binding in self {
            binding.save(out);
        }
    }
    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let len = usize::load(input)?;
        if len > input.len() {
            return Err(bad());
        }
        Ok(match len {
            0 => Self::Empty,
            1 => Self::One(Binding::load(input)?),
            _ => Self::Many(
                (0..len)
                    .map(|_| Binding::load(input))
                    .collect::<io::Result<_>>()?,
            ),
        })
    }
}
