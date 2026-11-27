use super::{
    Binding, Role, Syntax,
    grammar::{CallableGrammar, Hack, KeywordGrammar, Php, PhpGrammar},
};

struct PhpFamily<const HACK: bool>;

impl<const HACK: bool> CallableGrammar for PhpFamily<HACK> {}
impl<const HACK: bool> KeywordGrammar for PhpFamily<HACK> {
    fn after_name(s: &Syntax<'_>, next: usize) -> Option<usize> {
        if HACK {
            s.written_generic_end(next)
        } else {
            Some(next)
        }
    }

    fn method_binding(s: &Syntax<'_>, start: usize, _: usize) -> Binding {
        s.php_method_binding(start)
    }
}

impl<const HACK: bool> PhpGrammar for PhpFamily<HACK> {
    fn type_kind(word: &str) -> Option<&'static str> {
        const TYPES: &[(&str, &str)] = &[
            ("enum", "enum_declaration"),
            ("class", "class_declaration"),
            ("trait", "trait_item"),
            ("interface", "interface_declaration"),
        ];
        if HACK && hack_type_alias(word) {
            return Some("type_alias_declaration");
        }
        TYPES
            .iter()
            .find(|(name, _)| word.eq_ignore_ascii_case(name))
            .map(|(_, kind)| *kind)
    }
}

fn hack_type_alias(word: &str) -> bool {
    word.eq_ignore_ascii_case("type") || word.eq_ignore_ascii_case("newtype")
}

#[derive(Default)]
struct MethodModifiers {
    restricted: bool,
    static_method: bool,
    direct: bool,
}
impl MethodModifiers {
    fn consume(&mut self, word: &str) -> bool {
        if word.eq_ignore_ascii_case("static") {
            self.static_method = true;
        } else if word.eq_ignore_ascii_case("private") {
            self.restricted = true;
            self.direct = true;
        } else if word.eq_ignore_ascii_case("final") {
            self.direct = true;
        } else if ["protected", "internal", "abstract"]
            .iter()
            .any(|m| word.eq_ignore_ascii_case(m))
        {
            self.restricted = true;
        } else if !["public", "async", "readonly"]
            .iter()
            .any(|m| word.eq_ignore_ascii_case(m))
        {
            return false;
        }
        true
    }
}

impl Syntax<'_> {
    /// Named class calls need a public static implementation. Receiver-local
    /// calls may also bind directly to private and final methods.
    pub(super) fn php_method_binding(&self, start: usize) -> Binding {
        let mut flags = MethodModifiers::default();
        for token in self.t[..start].iter().rev() {
            if !flags.consume(token.text) {
                break;
            }
        }
        match (flags.static_method, flags.restricted) {
            (true, false) => Binding::PublicStatic,
            (true, true) => Binding::ScopedStatic,
            (false, _) => Binding::direct(flags.direct),
        }
    }
}

impl Syntax<'_> {
    pub(super) fn php_declarations<G: PhpGrammar>(&mut self) {
        for i in 0..self.t.len() {
            self.php_declaration::<G>(i);
        }
    }

    fn php_declaration<G: PhpGrammar>(&mut self, i: usize) {
        let word = self.w(i);
        if word.eq_ignore_ascii_case("function") {
            self.keyword_function::<G>(i);
        } else if word.eq_ignore_ascii_case("const") {
            self.php_constant(i);
        } else if let Some(kind) = G::type_kind(word) {
            self.register_type(i, kind);
        } else if self.php_property_at(i, word) {
            self.add(i, "property_declaration", Role::Value, self.decl_scope(i));
        }
    }

    fn php_constant(&mut self, i: usize) {
        if let Some(at) = self.name_before(i + 1, &["=", ";"]) {
            self.add(at, "const_item", Role::Value, self.decl_scope(i));
        }
    }

    fn php_property_at(&self, i: usize, word: &str) -> bool {
        let named = word.starts_with('$') && word.len() > 1;
        named && self.field_position(i)
    }
}

impl super::DeclarationGrammar for Php {
    fn extract(s: &mut Syntax<'_>) {
        s.php_declarations::<PhpFamily<false>>();
    }
}

impl super::DeclarationGrammar for Hack {
    fn extract(s: &mut Syntax<'_>) {
        s.php_declarations::<PhpFamily<true>>();
    }
}
