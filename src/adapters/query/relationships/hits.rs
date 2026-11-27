//! Relationship rows are ordered by source location, then identity. Keep
//! evidence for the first 50 distinct rows, even for unordered input.
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

pub(super) type Key = (usize, usize, Option<Rc<str>>, Option<Rc<str>>);

#[derive(Default)]
pub(super) struct Hits {
    pub rows: BTreeMap<Key, BTreeSet<(Option<usize>, Rc<str>)>>,
    pub truncated: bool,
}

impl Hits {
    const LIMIT: usize = 50;

    /// A later location cannot contribute evidence to a retained row.
    /// At an equal location the identity still decides order and duplication.
    pub fn accepts_location(&mut self, file: usize, byte: usize) -> bool {
        if self.rows.len() == Self::LIMIT
            && let Some(((last_file, last_byte, _, _), _)) = self.rows.last_key_value()
            && (file, byte) > (*last_file, *last_byte)
        {
            self.truncated = true;
            return false;
        }
        true
    }

    pub fn save(&mut self, key: Key, context: Option<usize>, evidence: Option<&Rc<str>>) {
        self.save_row(key, |targets| {
            if let Some(target) = evidence {
                targets.insert((context, Rc::clone(target)));
            }
        });
    }

    pub fn save_row(
        &mut self,
        key: Key,
        evidence: impl FnOnce(&mut BTreeSet<(Option<usize>, Rc<str>)>),
    ) {
        evidence(self.rows.entry(key).or_default());
        if self.rows.len() > Self::LIMIT {
            self.rows.pop_last();
            self.truncated = true;
        }
    }
}
