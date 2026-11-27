use super::{
    ROOT, Range, Syntax,
    grammar::{CallableGrammar, Dart, TypedGrammar, file_or_type},
};

impl CallableGrammar for Dart {
    const EXPRESSION_BODY: bool = true;
    fn parameters(s: &mut Syntax<'_>, scope: usize, range: Range<usize>) {
        s.callable_parameters(scope, range, |s, p| {
            if p.is_empty() {
                return None;
            }
            let name = p.end - 1;
            (s.id(name) && (p.start..name).any(|i| s.w(i) == "Function")).then_some(name)
        });
    }
}
impl TypedGrammar for Dart {
    fn type_kind(s: &Syntax<'_>, i: usize) -> Option<&'static str> {
        Some(match s.w(i) {
            "class" | "mixin" | "extension" => "class_declaration",
            "enum" => "enum_declaration",
            "typedef" => "type_alias_declaration",
            _ => return None,
        })
    }
    const IMPLICIT_RETURN: bool = true;
    fn property(word: &str) -> bool {
        matches!(word, "get" | "set")
    }
    fn scope(s: &Syntax<'_>, name: usize) -> Option<usize> {
        file_or_type(s, name)
    }
    fn valid_suffix(s: &Syntax<'_>, at: usize, class: usize) -> bool {
        match s.w(at) {
            ";" => class != ROOT,
            "=" => s.w(at + 1) == ">",
            "{" | "throws" | "async" | "sync" | ":" => true,
            _ => false,
        }
    }
}

impl super::DeclarationGrammar for Dart {
    fn extract(s: &mut Syntax<'_>) {
        s.typed_declarations::<Self>();
    }
}
