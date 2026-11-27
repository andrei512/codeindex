//! A renamed file may reuse any earlier copy with matching bytes and language.
//! Keep catalog order and allocate a list only for hashes with multiple paths.
use super::{BlobId, Checkpoint};
use std::collections::{HashMap, hash_map::Entry};

enum Paths {
    One(usize),
    Many(Vec<usize>),
}

impl Paths {
    fn insert(entry: Entry<'_, BlobId, Self>, id: usize) {
        match entry {
            Entry::Vacant(slot) => { slot.insert(Self::One(id)); }
            Entry::Occupied(mut slot) => slot.get_mut().push(id),
        }
    }

    fn push(&mut self, id: usize) {
        match self {
            Self::One(first) => *self = Self::Many(vec![*first, id]),
            Self::Many(ids) => ids.push(id),
        }
    }
}

pub(super) struct BlobIndex(HashMap<BlobId, Paths>);

impl BlobIndex {
    pub fn new(checkpoint: &Checkpoint) -> Self {
        let mut paths = HashMap::new();
        for (id, row) in checkpoint.rows.iter().enumerate() {
            let Some(blob) = row.blob else { continue };
            Paths::insert(paths.entry(blob), id);
        }
        Self(paths)
    }

    pub fn paths(&self, blob: BlobId) -> &[usize] {
        match self.0.get(&blob) {
            None => &[],
            Some(Paths::One(id)) => std::slice::from_ref(id),
            Some(Paths::Many(ids)) => ids,
        }
    }
}
