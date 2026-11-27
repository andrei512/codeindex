//! Named Rust items and the scopes introduced by their signatures and bodies.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Function,
    Struct,
    Enum,
    Trait,
    Module,
    Type,
    Const,
    Static,
}

impl Item {
    fn name(self, t: &[Token], keyword: usize) -> Option<usize> {
        let next = keyword + 1;
        let at = next + usize::from(self == Self::Static && word(t, next) == "mut");
        (ident(word(t, at)) || self == Self::Const && word(t, at) == "_").then_some(at)
    }

    // A type alias gets its type from the RHS; a static or const value gets it
    // from the written annotation. Required associated types have neither.
    fn declared_type(self, t: &[Token], pairs: &[usize], after_name: usize) -> Option<usize> {
        match self {
            Self::Type => type_rhs(t, pairs, after_name),
            Self::Static | Self::Const if word(t, after_name) == ":" => Some(after_name + 1),
            _ => None,
        }
    }

    fn declaration(keyword: &str) -> Option<ItemHeader> {
        let (item, kind, namespace) = match keyword {
            "struct" => (Self::Struct, "struct_item", Namespace::Either),
            "enum" => (Self::Enum, "enum_item", Namespace::Type),
            "trait" => (Self::Trait, "trait_item", Namespace::Type),
            "mod" => (Self::Module, "mod_item", Namespace::Type),
            "type" => (Self::Type, "type_item", Namespace::Type),
            _ => return Self::value_declaration(keyword),
        };
        Some(ItemHeader {
            item,
            kind,
            namespace,
        })
    }

    fn value_declaration(keyword: &str) -> Option<ItemHeader> {
        let (item, kind) = match keyword {
            "fn" => (Self::Function, "function_item"),
            "const" => (Self::Const, "const_item"),
            "static" => (Self::Static, "static_item"),
            _ => return None,
        };
        Some(ItemHeader {
            item,
            kind,
            namespace: Namespace::Value,
        })
    }

    fn binding_kind(self, kind: &'static str, associated: bool, typed: bool) -> &'static str {
        if self == Self::Type && associated && !typed {
            return "associated_type";
        }
        kind
    }
}

struct ItemHeader {
    item: Item,
    kind: &'static str,
    namespace: Namespace,
}
enum FunctionTail {
    Declaration,
    Body {
        open: usize,
        close: usize,
        body: usize,
        return_type: Option<usize>,
    },
}

struct RegisteredItem {
    item: Item,
    keyword: usize,
    name: usize,
    declaration: usize,
}

impl Extractor<'_, '_> {
    pub(super) fn macro_definition(&mut self, token: usize) {
        let (t, pairs) = (self.tokens, self.pairs);
        let macro_name = name(t[token].text);
        if !opaque_macro(macro_name) {
            let end = self
                .delimiters
                .iter()
                .rev()
                .find(|&&open| t[open].text == "{")
                .map_or(t.len(), |&open| pairs[open]);
            let slot = self.macro_ends.entry(macro_name).or_default();
            *slot = (*slot).max(end);
        }
        self.declare(BindingSite {
            token,
            scope: self.scope,
            kind: "macro_definition",
            visibility: 0,
            namespace: Namespace::Type,
        });
    }

    pub(super) fn named_item(&mut self, i: usize, prev: &str) {
        if self.excluded[i] || matches!(prev, "'" | "*") {
            return;
        }
        let t = self.tokens;
        let Some(header) = Item::declaration(t[i].text) else {
            return;
        };
        let Some(name) = header.item.name(t, i) else {
            return;
        };
        self.register_item(i, name, header);
    }

    fn register_item(&mut self, i: usize, id_token: usize, header: ItemHeader) {
        let ItemHeader {
            item,
            kind,
            namespace,
        } = header;
        let t = self.tokens;
        let after_name = id_token + 1;
        let rhs = item.declared_type(t, self.pairs, after_name);
        let kind = item.binding_kind(
            kind,
            self.evidence.scopes[self.scope].associated,
            rhs.is_some(),
        );
        let id = self.declare(BindingSite {
            token: id_token,
            scope: self.scope,
            kind,
            visibility: 0,
            namespace,
        });
        self.excluded[id_token] = true;
        if matches!(item, Item::Struct | Item::Enum | Item::Trait) && word(t, id_token + 1) != "<" {
            self.evidence.members.plain_types.insert(id);
        }
        if let Some(ty) = rhs {
            self.evidence.members.record_type(id, self.scope, t, ty);
        }
        let generic = match item {
            Item::Function | Item::Struct | Item::Enum | Item::Type | Item::Trait => {
                self.enter_generics(i, after_name)
            }
            _ => None,
        };
        let registered = RegisteredItem {
            item,
            keyword: i,
            name: id_token,
            declaration: id,
        };
        self.item_body(registered, generic);
    }

    fn item_body(&mut self, registered: RegisteredItem, generic: Option<GenericSignature>) {
        let RegisteredItem {
            item,
            keyword,
            name,
            declaration: id,
        } = registered;
        let after_name = name + 1;
        match item {
            Item::Function => self.function(id, name, generic),
            Item::Trait => {
                if let Some(open) = self.body_open(after_name, generic) {
                    let child = self.associated_scope(open);
                    self.evidence.members.trait_scopes.insert(child, id);
                }
            }
            Item::Struct | Item::Enum => self.field_body(item, id, after_name, generic),
            Item::Module => self.module_item(keyword, name),
            _ => {}
        }
    }

    fn field_body(
        &mut self,
        item: Item,
        id: usize,
        start: usize,
        generic: Option<GenericSignature>,
    ) {
        let Some(open) = self.body_open(start, generic) else {
            return;
        };
        if self.pairs[open] != usize::MAX {
            self.body_scopes
                .entry(open)
                .or_insert_with(|| new_scope(&mut self.evidence, self.scope));
            self.fields(id, open, item == Item::Enum);
        }
    }

    fn module_item(&mut self, keyword: usize, name: usize) {
        let t = self.tokens;
        let next = name + 1;
        let paths = self
            .module_attribute
            .take()
            .filter(|(at, _)| *at == keyword)
            .map(|(_, paths)| paths);
        let module = match word(t, next) {
            ";" => paths.map_or(Module::File(None), Module::Paths),
            "{" => {
                let child = self.evidence.scopes.len();
                self.evidence.scopes.push(Scope {
                    parent: Some(self.scope),
                    module: child,
                    ..Default::default()
                });
                self.body_scopes.insert(next, child);
                paths.map_or(Module::Inline(child), |paths| {
                    Module::InlinePaths(child, paths)
                })
            }
            _ => return,
        };
        self.evidence.scopes[self.scope].module(t[name].text, module, t[name].span.start);
    }
    fn function(&mut self, id: usize, id_token: usize, generic: Option<GenericSignature>) {
        let t = self.tokens;
        let Some(tail) = self.function_tail(id_token, generic) else {
            return;
        };
        let FunctionTail::Body {
            open,
            close,
            body,
            return_type,
        } = tail
        else {
            self.analysis.declarations[id].kind = "function_signature_item";
            return;
        };
        let child = self.evidence.scopes.len();
        self.evidence.scopes.push(Scope {
            parent: Some(self.scope),
            module: self.evidence.scopes[self.scope].module,
            owner: Some(id),
            ..Default::default()
        });
        self.body_scopes.insert(body, child);
        if let Some(return_type) = return_type {
            self.evidence.members.record_type(id, child, t, return_type);
        }
        self.formal_parameters(open, close, child, false);
    }
    fn function_tail(
        &self,
        name: usize,
        generic: Option<GenericSignature>,
    ) -> Option<FunctionTail> {
        let (t, pairs) = (self.tokens, self.pairs);
        let start = generic.map_or(name + 1, |signature| signature.after_parameters);
        let open = until(t, pairs, start, &["(", "{", ";", "}"]);
        if word(t, open) != "(" || pairs[open] == usize::MAX {
            return None;
        }
        let close = pairs[open];
        let body = self.signature_end(close + 1, generic);
        match word(t, body) {
            ";" => Some(FunctionTail::Declaration),
            "{" => {
                let return_type =
                    ((word(t, close + 1), word(t, close + 2)) == ("-", ">")).then_some(close + 3);
                Some(FunctionTail::Body {
                    open,
                    close,
                    body,
                    return_type,
                })
            }
            _ => None,
        }
    }
}
