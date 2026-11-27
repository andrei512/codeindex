use super::{
    Syntax,
    grammar::{CallableGrammar, Java, TypedGrammar},
};

impl CallableGrammar for Java {}
impl TypedGrammar for Java {
    fn type_kind(s: &Syntax<'_>, i: usize) -> Option<&'static str> {
        Some(match s.w(i) {
            "class" => "class_declaration",
            "enum" => "enum_declaration",
            "interface" => "interface_declaration",
            "record" => "record_declaration",
            _ => return None,
        })
    }
    fn scope(s: &Syntax<'_>, name: usize) -> Option<usize> {
        s.types.get(&s.parent[name]).copied()
    }
    fn valid_suffix(s: &Syntax<'_>, at: usize, _: usize) -> bool {
        matches!(s.w(at), "{" | ";" | "throws" | "async" | "sync" | "=" | ":")
    }
}

impl super::DeclarationGrammar for Java {
    fn extract(s: &mut Syntax<'_>) {
        s.typed_declarations::<Self>();
    }
}
