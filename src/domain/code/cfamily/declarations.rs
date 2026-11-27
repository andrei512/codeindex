//! Grammar-specific names, type containers and values used by the shared declaration walk.
use super::{Analyzer, DeclaredType, Role};

mod grammar;
pub(super) use grammar::Grammar;

fn follows_declarator(next: &str) -> bool {
    matches!(next, "=" | ";" | "," | "[" | "{" | ")")
}

pub(super) enum ContainerKind {
    Namespace,
    Extension,
    Type(&'static str),
}

fn c_tag(keyword: &str) -> Option<ContainerKind> {
    Some(ContainerKind::Type(match keyword {
        "struct" => "struct_specifier",
        "enum" => "enum_specifier",
        "union" => "local_binding",
        _ => return None,
    }))
}

pub(super) trait Dialect: super::functions::FunctionGrammar + super::overloads::CallGrammar {
    fn observe(&mut self, _token: &str) {}
    fn container(keyword: &str) -> Option<ContainerKind>;
    fn value(&self, analyzer: &mut Analyzer, i: usize, scope: usize, next: &str);
    fn syntax_word(_a: &Analyzer, _at: usize) -> bool {
        false
    }
    fn special_name(_previous: &str, _word: &str) -> bool {
        false
    }
}

pub(super) struct Swift;
impl Dialect for Swift {
    fn container(keyword: &str) -> Option<ContainerKind> {
        Some(match keyword {
            "extension" => ContainerKind::Extension,
            "struct" | "class" | "enum" | "actor" => ContainerKind::Type("class_declaration"),
            "protocol" => ContainerKind::Type("protocol_declaration"),
            _ => return None,
        })
    }
    fn value(&self, analyzer: &mut Analyzer, i: usize, scope: usize, next: &str) {
        if matches!(analyzer.prev(i), "let" | "var") {
            let ty = (next == ":").then(|| analyzer.word(i + 2));
            analyzer
                .add(i, scope, "property_declaration", Role::Value)
                .ty = ty;
        }
    }
    fn special_name(previous: &str, word: &str) -> bool {
        previous == "func" || word == "init"
    }
    fn syntax_word(a: &Analyzer, at: usize) -> bool {
        matches!(a.word(at), "get" | "set")
            && matches!(a.prev(at), "{" | "}")
            && a.word(at + 1) == "{"
    }
}

#[derive(Default)]
pub(super) struct C {
    in_typedef: bool,
}
impl Dialect for C {
    fn observe(&mut self, token: &str) {
        match token {
            "typedef" => self.in_typedef = true,
            ";" | "{" | "}" => self.in_typedef = false,
            _ => {}
        }
    }

    fn container(keyword: &str) -> Option<ContainerKind> {
        c_tag(keyword)
    }
    fn value(&self, analyzer: &mut Analyzer, i: usize, scope: usize, next: &str) {
        if next == ";" && self.in_typedef {
            // Preserve an underlying type only when the declaration establishes
            // it. A forward tag or a plausible external spelling is insufficient.
            let underlying = analyzer
                .declared_type(i, scope)
                .and_then(|ty| match ty.proof {
                    super::TypeProof::Known(name) => Some(name),
                    _ => None,
                });
            analyzer.add(i, scope, "local_binding", Role::Type).ty = underlying;
        } else if follows_declarator(next) {
            analyzer.typed_value(i, scope, "local_binding");
        }
    }
}

pub(super) struct Csharp;
impl Dialect for Csharp {
    fn special_name(_: &str, word: &str) -> bool {
        word == "init"
    }
    fn container(keyword: &str) -> Option<ContainerKind> {
        if keyword == "namespace" {
            return Some(ContainerKind::Namespace);
        }
        Some(ContainerKind::Type(match keyword {
            "class" => "class_declaration",
            "struct" => "struct_declaration",
            "enum" => "enum_declaration",
            "interface" => "interface_declaration",
            "record" => "record_declaration",
            _ => return None,
        }))
    }
    fn value(&self, analyzer: &mut Analyzer, i: usize, scope: usize, next: &str) {
        if !follows_declarator(next) {
            return;
        }
        let kind = if next == "{" {
            "property_declaration"
        } else {
            "variable_declarator"
        };
        analyzer.typed_value(i, scope, kind);
    }
}

pub(super) struct Cpp;
impl Cpp {
    // C++ auto inference accepts a complete initializer ending at this
    // declaration. A member result needs a uniquely known temporary receiver.
    fn initializer_type<'a>(
        analyzer: &Analyzer<'a, '_>,
        i: usize,
        scope: usize,
    ) -> Option<&'a str> {
        if analyzer.word(i + 1) != "(" {
            return None;
        }
        let after = analyzer.pairs[i + 1] + 1;
        match analyzer.word(after) {
            ";" => {
                let name = analyzer.word(i);
                analyzer
                    .lookup(scope, name, Some(Role::Type), i)
                    .next()
                    .map(|_| name)
            }
            "." if analyzer.word(after + 2) == "("
                && analyzer.word(analyzer.pairs[after + 2] + 1) == ";" =>
            {
                let member = after + 1;
                let owner = analyzer.receiver(member, scope)??;
                let &[id] = analyzer.ids(owner, analyzer.word(member)) else {
                    return None;
                };
                analyzer.bindings[id].ty
            }
            _ => None,
        }
    }
}
impl Dialect for Cpp {
    fn container(keyword: &str) -> Option<ContainerKind> {
        match keyword {
            "namespace" => Some(ContainerKind::Namespace),
            "class" => Some(ContainerKind::Type("class_specifier")),
            other => c_tag(other),
        }
    }
    fn value(&self, analyzer: &mut Analyzer, i: usize, scope: usize, next: &str) {
        if next != "(" && !follows_declarator(next) {
            return;
        }
        let Some(mut ty) = analyzer.declared_type(i, scope) else {
            return;
        };
        if ty.name == "auto" && next == "=" {
            ty.name = Self::initializer_type(analyzer, i + 2, scope).unwrap_or(ty.name);
        }
        let is_const = analyzer.t[i.saturating_sub(2)..i]
            .iter()
            .any(|t| t.text == "const");
        let binding = analyzer.add(i, scope, "local_binding", Role::Value);
        ty.apply(binding);
        binding.is_const = is_const;
    }
}

impl<'a> Analyzer<'a, '_> {
    fn typed_value(&mut self, i: usize, scope: usize, kind: &'static str) {
        if let Some(ty) = self.declared_type(i, scope) {
            ty.apply(self.add(i, scope, kind, Role::Value));
        }
    }
}

impl<'a> super::TypeProof<'a> {
    pub(super) fn named(
        grammar: &Grammar,
        name: &'a str,
        tag: TagEvidence<'_, 'a>,
    ) -> Option<Self> {
        let TagEvidence {
            keyword,
            binding: tag,
            native_pointer,
        } = tag;
        if let Some(underlying) = tag.and_then(|b| b.ty) {
            return Some(Self::Known(underlying));
        }
        if tag.is_some() || keyword {
            return Some(Self::tagged(grammar, name, tag, native_pointer));
        }
        let plausible = name.ends_with("_t") || name.starts_with(char::is_uppercase);
        (!grammar.reserved(name) && plausible).then_some(Self::Written)
    }

    fn tagged(
        grammar: &Grammar,
        name: &'a str,
        tag: Option<&super::Binding<'a>>,
        native_pointer: bool,
    ) -> Self {
        match tag {
            _ if native_pointer => Self::Known(name),
            Some(binding) if binding.body.is_some() => Self::Known(name),
            Some(_) if grammar.incomplete_tag_usable => Self::Written,
            _ => Self::Incomplete,
        }
    }
}

pub(super) struct TagEvidence<'b, 'a> {
    pub keyword: bool,
    pub binding: Option<&'b super::Binding<'a>>,
    pub native_pointer: bool,
}
impl<'a> DeclaredType<'a> {
    fn apply(self, binding: &mut super::Binding<'a>) {
        binding.ty = Some(self.name);
        binding.usable = !matches!(self.proof, super::TypeProof::Incomplete);
    }
}
