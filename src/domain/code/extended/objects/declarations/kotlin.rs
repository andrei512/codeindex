use super::{
    Binding, CallableName, ROOT, Range, Role, Syntax,
    grammar::{CallableGrammar, KeywordGrammar, Kotlin},
};

impl Kotlin {
    pub(super) fn type_kind(s: &Syntax<'_>, i: usize) -> Option<&'static str> {
        Some(match s.w(i) {
            "class" if s.prev(i) == "enum" => "enum_declaration",
            "class" | "object" => "class_declaration",
            "interface" => "interface_declaration",
            "typealias" => "type_alias_declaration",
            _ => return None,
        })
    }
}
impl CallableGrammar for Kotlin {
    const EXPRESSION_BODY: bool = true;
    fn ends_expression(word: &str) -> bool {
        matches!(
            word,
            "fun" | "val" | "var" | "class" | "private" | "public" | "internal"
        )
    }
    fn parameters(s: &mut Syntax<'_>, scope: usize, range: Range<usize>) {
        s.callable_parameters(scope, range, |s, p| {
            let at = p.start;
            let callable = s.w(at + 1) == ":" && s.w(at + 2) == "(";
            if !s.id(at) || !callable {
                return None;
            }
            let end = s.close(at + 2)?;
            (end < p.end && s.callable_return_arrow(end + 1)).then_some(at)
        });
    }
}
impl KeywordGrammar for Kotlin {
    fn name(s: &Syntax<'_>, start: usize, mut at: usize) -> Option<CallableName> {
        if s.w(at) == "<" {
            at = s.angle_end(at)? + 1;
        }
        let mut scope = s.decl_scope(start);
        while s.id(at) && s.w(at + 1) == "." {
            scope = start;
            at += 2;
        }
        s.lexical_callable(start, at, scope)
    }
    fn method_binding(s: &Syntax<'_>, start: usize, scope: usize) -> Binding {
        Binding::direct(
            !s.has_modifier(start, &["open", "override", "abstract"])
                && (scope == ROOT || s.prev(scope) != "interface"),
        )
    }
}

impl Syntax<'_> {
    pub(super) fn kotlin_declarations(&mut self) {
        for i in 0..self.t.len() {
            self.kotlin_declaration(i);
        }
    }

    fn callable_return_arrow(&self, at: usize) -> bool {
        self.w(at) == "-" && self.w(at + 1) == ">"
    }

    fn kotlin_declaration(&mut self, i: usize) {
        if let Some(kind) = Kotlin::type_kind(self, i) {
            self.register_type(i, kind);
        }
        match self.w(i) {
            "fun" => self.keyword_function::<Kotlin>(i),
            "val" | "var" if self.id(i + 1) => {
                self.add(
                    i + 1,
                    "property_declaration",
                    Role::Value,
                    self.decl_scope(i),
                );
            }
            _ => {}
        }
    }
}

impl super::DeclarationGrammar for Kotlin {
    fn extract(s: &mut Syntax<'_>) {
        s.kotlin_declarations();
    }
}
