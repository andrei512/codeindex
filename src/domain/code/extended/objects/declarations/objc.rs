use super::{
    Binding, ROOT, Role, Syntax,
    grammar::{CallableGrammar, ObjectiveC, TypedGrammar, file_or_type},
};

impl CallableGrammar for ObjectiveC {}
impl TypedGrammar for ObjectiveC {
    fn scope(s: &Syntax<'_>, name: usize) -> Option<usize> {
        if s.objc_type[name] == ROOT {
            file_or_type(s, name)
        } else {
            None
        }
    }
    fn valid_suffix(s: &Syntax<'_>, at: usize, _: usize) -> bool {
        matches!(s.w(at), "{" | ";")
    }
}

use std::borrow::Cow;
impl Syntax<'_> {
    pub(super) fn objc_types(&mut self) {
        let mut owner = ROOT;
        for i in 0..self.t.len() {
            if self.prev(i) == "@" {
                owner = self.objc_directive(i).unwrap_or(owner);
            }
            self.objc_type[i] = owner;
        }
    }
    fn objc_directive(&mut self, i: usize) -> Option<usize> {
        let kind = match self.w(i) {
            "end" => return Some(ROOT),
            "interface" | "implementation" => "class_declaration",
            "protocol" => "protocol_declaration",
            _ => return None,
        };
        let name = i + 1;
        if !self.id(name) {
            return None;
        }
        self.add(name, kind, Role::Type, ROOT);
        Some(name)
    }
    fn objc_method_attributes(&self, mut j: usize) -> Option<(bool, usize)> {
        let mut direct = false;
        while self.w(j) == "__attribute__" && self.w(j + 1) == "(" {
            let end = self.close(j + 1)?;
            let mut attr = j + 3; // __attribute__((name, ...))
            while attr < end && self.w(attr) != ")" {
                direct |= self.w(attr) == "objc_direct";
                attr += 1;
                if self.w(attr) == "(" {
                    attr = self.close(attr)? + 1;
                }
                if self.w(attr) != "," {
                    break;
                }
                attr += 1;
            }
            j = end + 1;
        }
        Some((direct, j))
    }

    fn objc_selector(&self, start: usize) -> Option<(usize, String, usize)> {
        let ret = self.close(start + 1)?;
        let name = ret + 1;
        if !self.id(name) {
            return None;
        }
        let mut key = format!("{}{}", self.w(start), self.w(name));
        let mut j = name + 1;
        if self.w(j) == ":" {
            key.push(':');
            j += 1;
            while j < self.t.len() && !matches!(self.w(j), "{" | ";" | "}" | "__attribute__") {
                if self.w(j) == "(" {
                    let close = self.close(j)?;
                    j = close;
                } else if self.id(j) && self.w(j + 1) == ":" {
                    key.push_str(self.w(j));
                    key.push(':');
                    j += 1;
                }
                j += 1;
            }
        }
        Some((name, key, j))
    }

    pub(super) fn objc_method(&mut self, start: usize) {
        let Some((name, key, j)) = self.objc_selector(start) else {
            return;
        };
        let Some((direct, j)) = self.objc_method_attributes(j) else {
            return;
        };
        if !matches!(self.w(j), "{" | ";") {
            return;
        }
        let class = self.objc_type[start];
        let id = self.add(name, "method_declaration", Role::Method, class);
        // Objective-C message dispatch is dynamic except for explicitly direct methods.
        self.decls[id].binding = Binding::direct(direct);
        self.decls[id].key = Cow::Owned(key);
        self.decls[id].signature = Some(start..j);
        if self.w(j) == "{"
            && let Some(end) = self.close(j)
        {
            self.decls[id].body = Some(j + 1..end);
        }
    }
}

impl Syntax<'_> {
    pub(super) fn objc_declarations(&mut self) {
        self.objc_types();
        for i in 0..self.t.len() {
            self.objc_declaration(i);
        }
    }

    fn objc_declaration(&mut self, i: usize) {
        match self.w(i) {
            "-" | "+" if self.is_objc_method(i) => self.objc_method(i),
            "property" if self.prev(i) == "@" => self.objc_property(i),
            _ => self.objc_typed_function(i),
        }
    }

    fn is_objc_method(&self, i: usize) -> bool {
        self.w(i + 1) == "(" && self.objc_type[i] != ROOT
    }

    fn objc_property(&mut self, i: usize) {
        if let Some(at) = self.name_before(i + 1, &[";"]) {
            self.add(at, "property_declaration", Role::Value, self.objc_type[i]);
        }
    }

    fn objc_typed_function(&mut self, i: usize) {
        if let Some(scope) = self.typed_function_scope::<ObjectiveC>(i) {
            self.typed_function::<ObjectiveC>(i, scope);
        }
    }
}

impl super::DeclarationGrammar for ObjectiveC {
    fn extract(s: &mut Syntax<'_>) {
        s.objc_declarations();
    }
}
