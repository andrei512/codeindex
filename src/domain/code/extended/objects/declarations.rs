use super::{Binding, ROOT, Role, Syntax};
use std::ops::Range;

mod dart;
mod go;
mod grammar;
mod java;
mod kotlin;
mod objc;
mod php;
mod types;
use grammar::{CallableGrammar, Go, TypedGrammar};

#[derive(Clone, Copy)]
enum CallableKind {
    Function,
    Method,
    Constructor,
}

impl CallableKind {
    fn binding(self, method: impl FnOnce() -> Binding) -> Binding {
        match self {
            Self::Method => method(),
            Self::Constructor | Self::Function => Binding::Direct,
        }
    }

    fn declaration(self) -> (&'static str, Role) {
        match self {
            Self::Function => ("function_declaration", Role::Function),
            Self::Method => ("method_declaration", Role::Method),
            Self::Constructor => ("constructor_declaration", Role::Method),
        }
    }
}

struct CallableName {
    at: usize,
    scope: usize,
    kind: CallableKind,
    receiver: Option<usize>,
}

pub(super) trait DeclarationGrammar {
    fn extract(s: &mut Syntax<'_>);
}

impl<'a> Syntax<'a> {
    fn name_before(&self, start: usize, stops: &[&str]) -> Option<usize> {
        let end = self.seek(start, stops)?;
        (end > start && self.id(end - 1)).then(|| end - 1)
    }

    fn typed_declarations<G: TypedGrammar>(&mut self) {
        for i in 0..self.t.len() {
            if let Some(kind) = G::type_kind(self, i) {
                self.register_type(i, kind);
            }
            self.typed_value_or_callable::<G>(i);
        }
    }

    fn typed_value_or_callable<G: TypedGrammar>(&mut self, i: usize) {
        if G::property(self.w(i)) && self.id(i + 1) && self.declaration_level(i) {
            self.add(
                i + 1,
                "property_declaration",
                Role::Value,
                self.decl_scope(i),
            );
        } else if let Some(scope) = self.typed_function_scope::<G>(i) {
            self.typed_function::<G>(i, scope);
        } else if self.typed_field(i) {
            self.add(i, "property_declaration", Role::Value, self.decl_scope(i));
        }
    }

    fn typed_field(&self, i: usize) -> bool {
        self.field_position(i)
            && self.id(i)
            && i > 0
            && (self.id(i - 1) || matches!(self.prev(i), "]" | ">" | "?"))
    }

    fn field_position(&self, i: usize) -> bool {
        matches!(self.w(i + 1), "=" | ";" | ",")
            && !self.in_parameters[i]
            && self.types.contains_key(&self.parent[i])
            && !self.prev(i).eq_ignore_ascii_case("const")
    }

    fn typed_function_scope<G: TypedGrammar>(&self, name: usize) -> Option<usize> {
        if self.w(name + 1) != "(" || self.in_parameters[name] {
            return None;
        }
        if !self.callable_name(name) {
            return None;
        }
        G::scope(self, name)
    }
    fn decl_scope(&self, i: usize) -> usize {
        if let Some(&owner) = self.objc_type.get(i)
            && owner != ROOT
        {
            return owner;
        }
        let brace = self.parent[i];
        if self.namespaces.blocks.contains(&brace) {
            return ROOT;
        }
        // Non-type braces get a distinct scope, so nested callable declarations
        // cannot become candidates for bare file-level calls.
        self.types.get(&brace).copied().unwrap_or(brace)
    }
    fn declaration_level(&self, i: usize) -> bool {
        !self.in_parameters[i]
            && (self.parent[i] == ROOT
                || self.types.contains_key(&self.parent[i])
                || self.namespaces.blocks.contains(&self.parent[i]))
    }
    fn keyword_function<G: grammar::KeywordGrammar>(&mut self, start: usize) {
        let first = start + 1 + usize::from(self.w(start + 1) == "&");
        let Some(name) = G::name(self, start, first) else {
            return;
        };
        self.named_function::<G>(start, name);
    }

    fn named_function<G: grammar::KeywordGrammar>(&mut self, start: usize, name: CallableName) {
        let Some(open) = G::after_name(self, name.at + 1) else {
            return;
        };
        if self.w(open) != "(" {
            return;
        }
        let binding = name
            .kind
            .binding(|| G::method_binding(self, start, name.scope));
        self.record_callable::<G>(
            name,
            binding,
            start,
            self.close(open).map(|end| open + 1..end),
        );
    }
    fn record_callable<G: CallableGrammar>(
        &mut self,
        name: CallableName,
        binding: Binding,
        start: usize,
        parameters: Option<Range<usize>>,
    ) {
        let (kind, role) = name.kind.declaration();
        let id = self.add(name.at, kind, role, name.scope);
        self.decls[id].receiver = name.receiver;
        self.decls[id].binding = binding;
        if let Some(parameters) = parameters {
            let after = parameters.end + 1;
            G::parameters(self, name.at, parameters);
            self.decls[id].signature = Some(start..after);
            self.decls[id].body = self.function_body::<G>(after);
        }
    }

    pub(super) fn angle_end(&self, start: usize) -> Option<usize> {
        let mut depth = 0;
        for i in start..self.t.len() {
            match self.w(i) {
                "<" => depth += 1,
                ">" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                "{" | "}" | ";" => break,
                _ => {}
            }
        }
        None
    }
    fn has_modifier(&self, start: usize, words: &[&str]) -> bool {
        (0..start)
            .rev()
            .take_while(|&j| !matches!(self.w(j), ";" | "{" | "}" | "=" | "(" | ")"))
            .any(|j| words.contains(&self.w(j)))
    }
    fn callable_kind<G: TypedGrammar>(&self, name: usize, class: usize) -> Option<CallableKind> {
        let kind = if class == ROOT {
            CallableKind::Function
        } else if self.w(class) == self.w(name) {
            CallableKind::Constructor
        } else {
            CallableKind::Method
        };
        let prev_word = name.checked_sub(1).is_some_and(|p| self.id(p))
            || matches!(self.prev(name), "]" | "*" | "?" | ">");
        if !matches!(kind, CallableKind::Constructor) && !prev_word && !G::IMPLICIT_RETURN {
            return None;
        }
        Some(kind)
    }
    fn typed_function<G: TypedGrammar>(&mut self, name: usize, class: usize) {
        if matches!(
            self.prev(name),
            "." | "@"
                | "="
                | ":"
                | "return"
                | "new"
                | "set"
                | "get"
                | "class"
                | "record"
                | "interface"
        ) {
            return;
        }
        let Some(kind) = self.callable_kind::<G>(name, class) else {
            return;
        };
        let Some(end) = self.close(name + 1) else {
            return;
        };
        if !G::valid_suffix(self, end + 1, class) {
            return;
        }
        if !self.declaration_prefix(name) {
            return;
        }
        let binding = kind.binding(|| {
            Binding::direct(
                self.has_modifier(name, &["private", "static", "final"])
                    || self.has_modifier(class, &["final"]),
            )
        });
        let callable = CallableName {
            at: name,
            scope: class,
            kind,
            receiver: None,
        };
        self.record_callable::<G>(callable, binding, name, Some(name + 2..end));
    }

    /// Skip balanced annotation arguments and array dimensions while checking
    /// whether the name appears in a declaration or inside an initializer.
    fn declaration_prefix(&self, name: usize) -> bool {
        let mut i = name;
        while i > 0 {
            i -= 1;
            match self.w(i) {
                ";" | "{" | "}" => break,
                "=" | "(" | "[" | "return" | "new" | "->" => return false,
                ")" | "]" => {
                    let Some(open) = self.close(i) else {
                        return false;
                    };
                    i = open;
                }
                _ => {}
            }
        }
        true
    }
    fn written_generic_end(&self, at: usize) -> Option<usize> {
        if self.w(at) != "<" {
            return Some(at);
        }
        Some(self.angle_end(at)? + 1)
    }

    fn callable_parameters(
        &mut self,
        scope: usize,
        parameters: Range<usize>,
        recognize: impl Fn(&Self, Range<usize>) -> Option<usize>,
    ) {
        let mut first = parameters.start;
        let mut i = first;
        while i <= parameters.end {
            if i == parameters.end || self.w(i) == "," {
                self.record_parameter(scope, first..i, &recognize);
                first = i + 1;
            }
            i = self
                .after_parameter_token(i, parameters.end)
                .unwrap_or(parameters.end + 1);
        }
    }

    fn after_parameter_token(&self, i: usize, end: usize) -> Option<usize> {
        if i < end && matches!(self.w(i), "(" | "[" | "{") {
            return Some(self.close(i)? + 1);
        }
        Some(i + 1)
    }

    fn record_parameter(
        &mut self,
        scope: usize,
        bounds: Range<usize>,
        recognize: impl Fn(&Self, Range<usize>) -> Option<usize>,
    ) {
        if let Some(name) = recognize(self, bounds) {
            self.add(name, "parameter", Role::Value, scope);
        }
    }

    fn function_body<G: CallableGrammar>(&self, after: usize) -> Option<Range<usize>> {
        let at = self.seek_if(after, |word| {
            matches!(
                word,
                "{" | "func" | "fun" | "function" | "class" | "val" | "var"
            ) || G::EXPRESSION_BODY && word == "="
        })?;
        match self.w(at) {
            "{" => self.close(at).map(|close| at + 1..close),
            "=" => Some(at + 1..self.expression_end::<G>(at + 1)),
            _ => None,
        }
    }

    fn expression_end<G: CallableGrammar>(&self, first: usize) -> usize {
        let mut i = first;
        while i < self.t.len() {
            match self.w(i) {
                ";" | "}" => break,
                "(" | "[" | "{" => {
                    let Some(close) = self.close(i) else { break };
                    i = close;
                }
                word if G::ends_expression(word) && self.begins_next_line(i, first) => {
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        i
    }
    fn begins_next_line(&self, at: usize, first: usize) -> bool {
        at > first && self.t[at].line > self.t[at - 1].line
    }
}
