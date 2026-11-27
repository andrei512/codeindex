//! An opening type brace records the owner of its subsequent members.
use super::{Role, Syntax};

impl Syntax<'_> {
    pub(super) fn add_type_name(&mut self, i: usize, kind: &'static str) -> bool {
        if !self.type_name_at(i + 1) || matches!(self.prev(i), "." | ":") {
            return false;
        }
        self.add(i + 1, kind, Role::Type, self.decl_scope(i));
        true
    }

    fn type_name_at(&self, i: usize) -> bool {
        self.id(i) && self.w(i) != "on"
    }

    pub(super) fn register_type(&mut self, i: usize, kind: &'static str) {
        if self.add_type_name(i, kind)
            && kind != "type_alias_declaration"
            && let Some(open) = self.seek(
                i + 2,
                &[
                    "{",
                    "=",
                    "class",
                    "fun",
                    "function",
                    "object",
                    "interface",
                    "val",
                    "var",
                ],
            )
            && self.w(open) == "{"
        {
            self.types.insert(open, i + 1);
        }
    }
}
