//! Identify function declarators and bind their owners and signatures.
use super::{Analyzer, Role, ident};

/// Source ordinals for the written owner and name. A macro can supply an
/// owner whose spelling has no token range in this file.
pub(super) struct FunctionName {
    token: usize,
    start: usize,
    qualified_end: usize,
    macro_owner: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) enum FunctionBody {
    Block(usize),
    Expression(usize),
    Prototype,
}

use super::declarations::{C, Cpp, Csharp, Swift};

pub(super) trait FunctionGrammar {
    fn parameter<'a>(
        a: &Analyzer<'a, '_>,
        lo: usize,
        hi: usize,
    ) -> super::parameters::Parameter<'a> {
        a.type_first_parameter(lo, hi)
    }
    const LOCAL_INITIALIZER_AMBIGUITY: bool = false;
    const IMPLICIT_RESULT: bool = true;
    const EXPRESSION_BODY: bool = false;
    fn modifiers(_a: &mut Analyzer, _at: usize) {}
    fn accepts(_a: &Analyzer, _name: usize) -> bool {
        true
    }
    fn name(_a: &mut Analyzer, token: usize) -> FunctionName {
        FunctionName {
            token,
            start: token,
            qualified_end: token,
            macro_owner: None,
        }
    }
    fn tail(
        a: &Analyzer,
        name: &FunctionName,
        scope: usize,
        ty: Option<&str>,
    ) -> Option<FunctionBody> {
        a.typed_function_tail::<Self>(name, scope, ty)
    }
    fn kind(block: bool, _ty: Option<&str>) -> &'static str {
        if block {
            "function_definition"
        } else {
            "local_binding"
        }
    }
}
impl FunctionGrammar for C {}
impl FunctionGrammar for Cpp {
    const LOCAL_INITIALIZER_AMBIGUITY: bool = true;
    fn modifiers(a: &mut Analyzer, mut at: usize) {
        while let Some(token) = a.t.get(at) {
            match token.text {
                "override" | "final" => a.skip[at] = true,
                "const" | "volatile" | "&" => {}
                "noexcept" if a.word(at + 1) == "(" => at = a.pairs[at + 1],
                "noexcept" => {}
                _ => break,
            }
            at = at.saturating_add(1);
        }
    }
    fn name(a: &mut Analyzer, token: usize) -> FunctionName {
        a.cpp_name(token)
    }
}
impl FunctionGrammar for Csharp {
    const IMPLICIT_RESULT: bool = false;
    const EXPRESSION_BODY: bool = true;
    fn kind(_block: bool, ty: Option<&str>) -> &'static str {
        if ty.is_some() {
            "method_declaration"
        } else {
            "constructor_declaration"
        }
    }
}
impl FunctionGrammar for Swift {
    fn parameter<'a>(
        a: &Analyzer<'a, '_>,
        lo: usize,
        hi: usize,
    ) -> super::parameters::Parameter<'a> {
        a.swift_parameter(lo, hi)
    }
    fn accepts(a: &Analyzer, i: usize) -> bool {
        a.prev(i) == "func" || a.word(i) == "init"
    }
    fn tail(
        a: &Analyzer,
        name: &FunctionName,
        _scope: usize,
        _ty: Option<&str>,
    ) -> Option<FunctionBody> {
        let mut body = a.pairs[name.token + 1] + 1;
        if matches!(a.word(body), "-" | "throws" | "async" | "where") {
            body = a.delimit(body, &["{", "}", ";", "func"])
        }
        Some(if a.word(body) == "{" {
            FunctionBody::Block(body)
        } else {
            FunctionBody::Prototype
        })
    }
    fn kind(_block: bool, _ty: Option<&str>) -> &'static str {
        "function_declaration"
    }
}

impl<'a> Analyzer<'a, '_> {
    fn is_constructor(&self, name: &FunctionName, scope: usize) -> bool {
        let spelling = self.word(name.token);
        if name.macro_owner.as_deref() == Some(spelling) {
            return true;
        }
        if name.macro_owner.is_none()
            && name.start < name.qualified_end
            && self.word(name.qualified_end - 3) == spelling
        {
            return true;
        }
        self.scopes[scope].container == Some(scope)
            && self.scopes[scope].parent.is_some_and(|parent| {
                self.lookup(parent, spelling, Some(Role::Type), name.token)
                    .any(|binding| binding.body == Some(scope))
            })
    }

    fn constructor_body(&self, start: usize) -> Option<usize> {
        let mut j = start;
        loop {
            let end = self.constructor_initializer_end(j)?;
            match self.word(end) {
                "{" => return Some(end),
                "," => j = end + 1,
                _ => return None,
            }
        }
    }

    fn constructor_initializer_end(&self, start: usize) -> Option<usize> {
        let open = self.delimit(start, &["(", "{", ";", "}"]);
        if !matches!(self.word(open), "(" | "{") {
            return None;
        }
        let close = self.pairs[open];
        (close < self.t.len()).then(|| close + 1)
    }

    fn external_definition_scope(
        &mut self,
        enclosing: usize,
        declarator: usize,
        qualified_end: usize,
    ) -> Option<usize> {
        // Out-of-class definitions can introduce an owner known only in a
        // header. Keep the scope without manufacturing a type declaration.
        let mut scope = enclosing;
        for part in (declarator..qualified_end).step_by(3) {
            let name = self.word(part);
            if !ident(&self.t[part]) {
                return None;
            }
            scope = self
                .ids(scope, name)
                .iter()
                .find_map(|&id| self.bindings[id].body)
                .or_else(|| self.external_at(scope, name))
                .unwrap_or_else(|| self.create_external_scope(scope, name.into()));
        }
        Some(scope)
    }

    fn cpp_name(&mut self, i: usize) -> FunctionName {
        let destructor = self.prev(i) == "~";
        let qualified_end = if destructor { i - 1 } else { i };
        let mut declarator = qualified_end;
        while self.has_qualifier(declarator) {
            declarator -= 3;
        }
        let macro_owner = if declarator == qualified_end && declarator > 0 {
            self.macro_owners.remove(&(declarator - 1))
        } else {
            None
        };
        if macro_owner.is_some() {
            declarator -= 1;
        }
        FunctionName {
            token: i,
            start: declarator,
            qualified_end,
            macro_owner,
        }
    }

    fn has_qualifier(&self, token: usize) -> bool {
        let preceding = token.checked_sub(3);
        preceding.is_some_and(|part| self.word(part + 1) == ":" && self.word(part + 2) == ":")
    }

    fn definition_scope(
        &mut self,
        name: FunctionName,
        enclosing: usize,
        has_body: bool,
    ) -> Option<usize> {
        if let Some(owner) = name.macro_owner {
            if !has_body || self.scopes[enclosing].owner.is_some() {
                return None;
            }
            self.macro_owner_scope(enclosing, owner, name.token)
        } else if name.start == name.qualified_end {
            Some(enclosing)
        } else if let Some(Some(scope)) = self.receiver(name.qualified_end, enclosing) {
            Some(scope)
        } else if has_body && self.scopes[enclosing].owner.is_none() {
            self.external_definition_scope(enclosing, name.start, name.qualified_end)
        } else {
            None
        }
    }

    fn typed_function_tail<G: FunctionGrammar + ?Sized>(
        &self,
        name: &FunctionName,
        enclosing: usize,
        result_type: Option<&str>,
    ) -> Option<FunctionBody> {
        let after_parameters = self.pairs[name.token + 1] + 1;
        let spelling = self.word(after_parameters);
        if spelling == ":" {
            if !self.is_constructor(name, enclosing) {
                return None;
            }
            return self
                .constructor_body(after_parameters + 1)
                .map(FunctionBody::Block);
        }
        if result_type.is_none() && !self.admits_untyped_tail::<G>(name, enclosing, spelling) {
            return None;
        }
        let body = match spelling {
            "const" | "noexcept" | "override" | "final" | "where" => {
                self.delimit(after_parameters, &["{", ";", "}"])
            }
            _ => after_parameters,
        };
        self.written_function_body::<G>(body)
    }

    fn written_function_body<G: FunctionGrammar + ?Sized>(
        &self,
        body: usize,
    ) -> Option<FunctionBody> {
        match self.word(body) {
            "{" => Some(FunctionBody::Block(body)),
            "=" if G::EXPRESSION_BODY && self.word(body + 1) == ">" => {
                Some(FunctionBody::Expression(body))
            }
            ";" => Some(FunctionBody::Prototype),
            _ => None,
        }
    }

    fn admits_untyped_tail<G: FunctionGrammar + ?Sized>(
        &self,
        name: &FunctionName,
        enclosing: usize,
        spelling: &str,
    ) -> bool {
        // At file scope an annotation or header typedef can supply the return type.
        self.is_constructor(name, enclosing)
            || G::IMPLICIT_RESULT
                && self.scopes[enclosing].owner.is_none()
                && matches!(spelling, "{" | "const" | "noexcept")
                && self.has_written_return(name.start)
    }

    pub(super) fn function<G: FunctionGrammar>(&mut self, i: usize, enclosing: usize) -> bool {
        let open = i + 1;
        let closed = self.pairs[open] < self.t.len();
        if !(closed && G::accepts(self, i)) {
            return false;
        }
        let name = G::name(self, i);
        let close = self.pairs[open];
        let result_type = self.declared_type(name.start, enclosing).map(|ty| ty.name);
        let Some(body) = G::tail(self, &name, enclosing, result_type) else {
            return false;
        };
        let has_body = matches!(body, FunctionBody::Block(_));
        let Some(scope) = self.definition_scope(name, enclosing, has_body) else {
            return false;
        };
        let ambiguous =
            G::LOCAL_INITIALIZER_AMBIGUITY && !has_body && self.scopes[scope].owner.is_some();
        let Some(mut parameters) = self.function_parameters::<G>(open, scope, ambiguous) else {
            return false;
        };
        G::modifiers(self, close + 1);
        let kind = G::kind(has_body, result_type);
        let is_const = self.word(close + 1) == "const";
        let binding = self.add(i, scope, kind, Role::Function);
        binding.ty = result_type;
        binding.is_const = is_const;
        binding.definition = !matches!(body, FunctionBody::Prototype);
        let id = binding.id;
        let local = self.function_body_scope(body, scope, id, i);
        let mut labels = Vec::new();
        while let Some(parameter) = parameters.next::<G>(self) {
            labels.push((parameter.label, parameter.ty));
            self.register_parameter(parameter, local);
        }
        self.bindings[id].signature = Some(labels);
        true
    }
    fn register_parameter(
        &mut self,
        parameter: super::parameters::Parameter<'a>,
        local: Option<usize>,
    ) {
        let Some(name) = parameter.name else { return };
        let binding = match local {
            Some(scope) => self.add(name, scope, "parameter", Role::Value),
            None => self.declaration(name, "parameter", Role::Value),
        };
        binding.ty = parameter.ty;
        binding.is_const = parameter.is_const;
    }

    fn function_body_scope(
        &mut self,
        body: FunctionBody,
        parent: usize,
        id: usize,
        name: usize,
    ) -> Option<usize> {
        let local = match body {
            FunctionBody::Prototype => return None,
            FunctionBody::Block(open) => {
                let local = self.scope(parent, Some(id));
                self.body.insert(open, local);
                local
            }
            FunctionBody::Expression(arrow) => {
                let local = self.scope(parent, Some(id));
                let end = self.delimit(arrow + 2, &[";", "}"]);
                self.pending.expression(name, arrow..end, local);
                local
            }
        };
        self.scopes[local].const_receiver = self.bindings[id].is_const;
        Some(local)
    }
}
