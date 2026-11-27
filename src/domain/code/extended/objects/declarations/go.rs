//! Go declaration groups and receiver ownership. Type declarations are complete
//! before the first method looks up its receiver, including forward declarations.
use super::{CallableKind, CallableName, Go, Role, Syntax};
use std::{cell::OnceCell, collections::HashMap};

enum DeferredDecl {
    Method(usize),
    Function(usize),
    Values(usize),
}

struct GoReceiver {
    method: usize,
    scope: usize,
    binding: Option<usize>,
}

fn type_kind(next: &str) -> &'static str {
    match next {
        "struct" => "struct_declaration",
        "interface" => "interface_declaration",
        _ => "type_alias_declaration",
    }
}

fn value_kind(keyword: &str) -> &'static str {
    if keyword == "const" {
        "const_item"
    } else {
        "variable_declarator"
    }
}

impl<'a> Syntax<'a> {
    fn go_values(&mut self, keyword: usize) {
        let first = keyword + 1;
        if self.w(first) == "(" {
            self.go_group(first);
        } else {
            self.go_value_names(
                first,
                self.t.len(),
                value_kind(self.w(keyword)),
                self.decl_scope(keyword),
            );
        }
    }

    fn go_value_names(
        &mut self,
        mut at: usize,
        end: usize,
        kind: &'static str,
        scope: usize,
    ) -> usize {
        while at < end && self.id(at) {
            self.add(at, kind, Role::Value, scope);
            if !self.next_group_name(at, end) {
                break;
            }
            at += 2;
        }
        at
    }
    fn next_group_name(&self, at: usize, end: usize) -> bool {
        self.w(at + 1) == "," && at + 2 < end && self.id(at + 2)
    }

    fn go_group(&mut self, open: usize) {
        let Some(end) = self.close(open) else { return };
        let keyword = self.prev(open);
        let value = (keyword != "type").then(|| value_kind(keyword));
        let scope = self.decl_scope(open);
        let mut j = open + 1;
        let mut start = true;
        while j < end {
            if start {
                j = self.group_declaration(j, end, value, scope);
            }
            (j, start) = self.advance_group(j, end);
        }
    }
    fn group_declaration(
        &mut self,
        at: usize,
        end: usize,
        value: Option<&'static str>,
        scope: usize,
    ) -> usize {
        if !self.id(at) {
            return at;
        }
        if let Some(kind) = value {
            return self.go_value_names(at, end, kind, scope);
        }
        self.add(at, type_kind(self.w(at + 1)), Role::Type, scope);
        at
    }

    fn advance_group(&self, at: usize, end: usize) -> (usize, bool) {
        self.group_item_end(at)
            .map_or((end, false), |last| (last + 1, self.group_separator(last)))
    }

    fn group_item_end(&self, at: usize) -> Option<usize> {
        match self.w(at) {
            "(" | "[" | "{" => self.close(at),
            _ => Some(at),
        }
    }

    fn group_separator(&self, at: usize) -> bool {
        if self.w(at) == ";" {
            return true;
        }
        if matches!(self.w(at), "=" | "," | "+" | "-" | "*" | "/") {
            return false;
        }
        self.t
            .get(at + 1)
            .is_some_and(|next| next.line > self.t[at].line)
    }

    pub(super) fn go_declarations(&mut self) {
        // Register types first so receivers can resolve declarations that
        // follow them. Replay only value/function starts in source order.
        let mut remaining = Vec::new();
        remaining.extend((0..self.t.len()).filter_map(|i| self.go_declaration_start(i)));
        let types = OnceCell::new();
        for declaration in remaining {
            match declaration {
                DeferredDecl::Values(i) => self.go_values(i),
                DeferredDecl::Method(i) => self.go_method(i, &types),
                DeferredDecl::Function(i) => self.keyword_function::<Go>(i),
            }
        }
    }

    fn go_declaration_start(&mut self, at: usize) -> Option<DeferredDecl> {
        match (self.w(at), self.w(at + 1)) {
            ("type", "(") => {
                self.go_group(at + 1);
                None
            }
            ("type", _) => {
                self.add_type_name(at, type_kind(self.w(at + 2)));
                None
            }
            ("func", "(") => Some(DeferredDecl::Method(at)),
            ("func", _) => Some(DeferredDecl::Function(at)),
            ("var" | "const", _) => Some(DeferredDecl::Values(at)),
            _ => None,
        }
    }

    fn go_method(&mut self, i: usize, types: &OnceCell<HashMap<&'a str, usize>>) {
        if let Some(receiver) = self.go_receiver(i, types) {
            self.named_function::<Go>(
                i,
                CallableName {
                    at: receiver.method,
                    scope: receiver.scope,
                    kind: CallableKind::Method,
                    receiver: receiver.binding,
                },
            );
        }
    }

    fn go_receiver(
        &self,
        start: usize,
        types: &OnceCell<HashMap<&'a str, usize>>,
    ) -> Option<GoReceiver> {
        let first = start + 2;
        let method = self.close(start + 1)? + 1;
        // Anonymous functions have no method name following their parameter list.
        if !self.id(method) || self.w(method + 1) != "(" {
            return None;
        }
        let ty = (first..method - 1)
            .take_while(|&i| self.w(i) != "[")
            .filter(|&i| self.id(i))
            .last();
        let scope = ty
            .and_then(|i| {
                let types = types.get_or_init(|| {
                    let mut types = HashMap::new();
                    for decl in self.decls.iter().filter(|d| d.role == Role::Type) {
                        types.entry(self.w(decl.at)).or_insert(decl.at);
                    }
                    types
                });
                types.get(self.w(i)).copied()
            })
            .unwrap_or(start);
        Some(GoReceiver {
            method,
            scope,
            binding: ty.filter(|&i| i != first && self.id(first)).map(|_| first),
        })
    }
}

use super::grammar::{CallableGrammar, KeywordGrammar};
use std::ops::Range;
impl CallableGrammar for Go {
    fn parameters(s: &mut Syntax<'_>, scope: usize, range: Range<usize>) {
        s.callable_parameters(scope, range, |s, p| {
            (s.id(p.start) && s.w(p.start + 1) == "func").then_some(p.start)
        });
    }
}
impl KeywordGrammar for Go {
    fn after_name(s: &Syntax<'_>, next: usize) -> Option<usize> {
        if s.w(next) == "[" {
            Some(s.close(next)? + 1)
        } else {
            Some(next)
        }
    }
}

impl super::DeclarationGrammar for Go {
    fn extract(s: &mut Syntax<'_>) {
        s.go_declarations();
    }
}
