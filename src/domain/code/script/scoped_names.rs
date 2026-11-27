//! Own names in parser evidence and borrow the caller's spelling for a lookup.
use crate::domain::code::cache::{Output, Wire};
use std::{borrow::Cow, collections::HashMap, io};

#[derive(Default)]
pub(super) struct ScopedNames {
    by_name: HashMap<(usize, Cow<'static, str>), usize>,
}

impl ScopedNames {
    pub(super) fn get(&self, scope: usize, name: &str) -> Option<usize> {
        self.by_name.get(&(scope, Cow::Borrowed(name))).copied()
    }

    pub(super) fn insert(&mut self, scope: usize, name: &str, id: usize) {
        self.insert_owned(scope, name.to_owned(), id);
    }

    pub(super) fn insert_owned(&mut self, scope: usize, name: String, id: usize) {
        self.by_name.insert((scope, Cow::Owned(name)), id);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub(super) fn longest_name(&self) -> usize {
        self.by_name
            .keys()
            .map(|(_, name)| name.len())
            .max()
            .unwrap_or(0)
    }

    pub(super) fn declare(&mut self, scope: usize, name: &str, id: usize) -> bool {
        if self.get(scope, name).is_some() {
            return false;
        }
        self.insert(scope, name, id);
        true
    }
}

impl Wire for Cow<'static, str> {
    fn save(&self, out: &mut impl Output) {
        self.len().save(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn load(input: &mut &[u8]) -> io::Result<Self> {
        String::load(input).map(Cow::Owned)
    }
}

crate::domain::code::cache::record!(ScopedNames { by_name });

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn borrowed_member_keys_preserve_legacy_evidence_bytes() {
        let mut old = HashMap::new();
        old.insert((7usize, String::from("run")), 1usize);
        old.insert((1, String::from("run")), 2);
        old.insert((1, String::from("init")), 3);
        let mut bytes = Vec::new();
        old.save(&mut bytes);
        let members = ScopedNames::load(&mut bytes.as_slice()).unwrap();
        assert_eq!(members.get(7, "run"), Some(1));
        assert_eq!(members.get(1, "run"), Some(2));
        let mut encoded = Vec::new();
        members.save(&mut encoded);
        assert_eq!(bytes, encoded);
    }
}
