//! Facts shared by explicit Java super calls in one source file.
use super::*;

#[derive(Default)]
pub(super) struct SuperMethods {
    parents: HashMap<usize, Option<usize>>,
    methods: HashMap<usize, Option<Signature>>,
}
struct Signature {
    arity: usize,
    accepts_arguments: bool,
}

impl SuperMethods {
    pub fn parent(&mut self, syntax: &Syntax, class: usize, names: &MemberNames) -> Option<usize> {
        *self
            .parents
            .entry(class)
            .or_insert_with(|| syntax.java_parent(class, names))
    }
    pub fn matches(&mut self, syntax: &Syntax, call: usize, method: usize) -> bool {
        let Some(signature) = self
            .methods
            .entry(method)
            .or_insert_with(|| syntax.java_signature(method))
        else {
            return false;
        };
        (signature.accepts_arguments || syntax.w(call + 2) == ")")
            && syntax.java_arity(call + 1) == Some(signature.arity)
    }
}

impl Syntax<'_> {
    fn java_signature(&self, id: usize) -> Option<Signature> {
        let method = &self.decls[id];
        method.body.as_ref()?;
        let after = if self.w(method.scope + 1) == "<" {
            self.angle_end(method.scope + 1).unwrap_or(method.scope) + 1
        } else {
            method.scope + 1
        };
        // An uninspected ancestor may declare a different overload.
        Some(Signature {
            arity: self.java_arity(method.at + 1)?,
            accepts_arguments: self.w(after) == "{",
        })
    }

    fn java_arity(&self, open: usize) -> Option<usize> {
        let end = self.close(open)?;
        let mut count = usize::from(end > open + 1);
        let mut i = open + 1;
        while i < end {
            match self.w(i) {
                "," => count += 1,
                "(" | "[" | "{" => i = self.close(i)?,
                "<" => i = self.angle_end(i)?,
                _ => {}
            }
            i += 1;
        }
        Some(count)
    }
    fn unqualified_type_at(&self, at: usize) -> bool {
        self.id(at) && self.w(at + 1) != "."
    }

    fn java_parent(&self, class: usize, lookup: &MemberNames<'_>) -> Option<usize> {
        if class == ROOT {
            return None;
        }
        let mut i = class + 1;
        if self.w(i) == "<" {
            i = self.angle_end(i)? + 1;
        }
        if self.w(i) != "extends" || !self.unqualified_type_at(i + 1) {
            return None;
        }
        let scope = self
            .types
            .get(&self.parent[class])
            .copied()
            .unwrap_or(self.parent[class]);
        let id = (*lookup.get(&(scope, self.w(i + 1)))?)?;
        (self.decls[id].role == Role::Type).then_some(self.decls[id].at)
    }
}
