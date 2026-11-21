//! Written declarations and lexically provable uses for C-family source files.
//! A scope indexes distinct type/tag, value and namespace bindings. A use follows
//! its lexical parents or an explicit receiver. Unproved dispatch remains unresolved.
use super::{Analysis, Call, Declaration, Kind, Language, Token};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Type,
    Namespace,
    Value,
    Function,
}
struct Binding<'a> {
    id: usize,
    role: Role,
    visible: usize,
    body: Option<usize>,
    ty: Option<&'a str>,
    signature: Option<Vec<(&'a str, Option<&'a str>)>>,
    usable: bool,
}
#[derive(Default)]
struct Scope<'a> {
    parent: Option<usize>,
    owner: Option<usize>,
    container: Option<usize>,
    bindings: HashMap<&'a str, Vec<Binding<'a>>>,
}
struct Analyzer<'a, 't> {
    lang: Language,
    t: &'t [Token<'a>],
    out: Analysis,
    scopes: Vec<Scope<'a>>,
    at: Vec<usize>,
    pairs: Vec<usize>,
    body: HashMap<usize, usize>,
    skip: Vec<bool>,
    declared: Vec<bool>,
}
fn ident(t: &Token) -> bool {
    t.kind == Kind::Word && t.text.starts_with(|c: char| c == '_' || c.is_alphabetic())
}
fn keyword(s: &str) -> bool {
    matches!(
        s,
        "alignof"
            | "as"
            | "auto"
            | "await"
            | "bool"
            | "break"
            | "case"
            | "catch"
            | "char"
            | "class"
            | "const"
            | "continue"
            | "default"
            | "delete"
            | "do"
            | "double"
            | "else"
            | "enum"
            | "explicit"
            | "export"
            | "extension"
            | "extern"
            | "false"
            | "float"
            | "for"
            | "foreach"
            | "func"
            | "get"
            | "guard"
            | "if"
            | "import"
            | "in"
            | "init"
            | "int"
            | "interface"
            | "internal"
            | "is"
            | "let"
            | "long"
            | "namespace"
            | "new"
            | "null"
            | "nullptr"
            | "operator"
            | "out"
            | "override"
            | "private"
            | "protected"
            | "protocol"
            | "public"
            | "record"
            | "ref"
            | "register"
            | "return"
            | "self"
            | "set"
            | "short"
            | "signed"
            | "sizeof"
            | "static"
            | "string"
            | "struct"
            | "switch"
            | "template"
            | "this"
            | "throw"
            | "throws"
            | "true"
            | "try"
            | "typedef"
            | "typename"
            | "union"
            | "unsigned"
            | "using"
            | "var"
            | "virtual"
            | "void"
            | "volatile"
            | "while"
    )
}
fn primitive(s: &str) -> bool {
    matches!(
        s,
        "void"
            | "bool"
            | "char"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "signed"
            | "unsigned"
            | "string"
            | "object"
            | "byte"
            | "uint"
            | "ulong"
            | "nint"
            | "decimal"
            | "auto"
            | "var"
            | "size_t"
            | "ssize_t"
    )
}
impl<'a, 't> Analyzer<'a, 't> {
    fn word(&self, i: usize) -> &'a str {
        self.t.get(i).map_or("", |t| t.text)
    }
    fn prev(&self, i: usize) -> &'a str {
        i.checked_sub(1).map_or("", |j| self.word(j))
    }
    fn new(lang: Language, t: &'t [Token<'a>]) -> Self {
        let n = t.len();
        let mut pairs = vec![n; n];
        let mut stack = vec![];
        for (i, tok) in t.iter().enumerate() {
            match tok.text {
                "(" | "{" | "[" => stack.push((tok.text, i)),
                ")" | "}" | "]" => {
                    if let Some(&(open, j)) = stack.last()
                        && matches!((open, tok.text), ("(", ")") | ("{", "}") | ("[", "]"))
                    {
                        pairs[j] = i;
                        pairs[i] = j;
                        stack.pop();
                    }
                }
                _ => {}
            }
        }
        Self {
            lang,
            t,
            out: Analysis::default(),
            scopes: vec![Scope::default()],
            at: vec![0; n],
            pairs,
            body: HashMap::new(),
            skip: vec![false; n],
            declared: vec![false; n],
        }
    }
    fn scope(&mut self, parent: usize, owner: Option<usize>) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            owner: owner.or(self.scopes[parent].owner),
            container: self.scopes[parent].container,
            ..Scope::default()
        });
        id
    }
    fn lookup(
        &self,
        mut scope: usize,
        name: &str,
        role: Option<Role>,
        at: usize,
    ) -> Vec<&Binding<'a>> {
        loop {
            if let Some(bindings) = self.scopes[scope].bindings.get(name) {
                let v: Vec<_> = bindings
                    .iter()
                    .filter(|b| b.visible <= at && role.is_none_or(|r| r == b.role))
                    .collect();
                if !v.is_empty() {
                    return v;
                }
            }
            let Some(p) = self.scopes[scope].parent else {
                return vec![];
            };
            scope = p;
        }
    }
    fn add(&mut self, i: usize, scope: usize, kind: &'static str, role: Role) -> usize {
        let name = self.word(i);
        let id = self.out.declarations.len();
        self.declared[i] = true;
        self.out.declarations.push(Declaration {
            span: self.t[i].span.clone(),
            kind,
        });
        let visible = if matches!(
            self.lang,
            Language::Swift | Language::Csharp | Language::Cpp
        ) && role != Role::Value
        {
            0
        } else {
            i
        };
        self.scopes[scope]
            .bindings
            .entry(name)
            .or_default()
            .push(Binding {
                id,
                role,
                visible,
                ty: None,
                body: None,
                signature: None,
                usable: true,
            });
        id
    }
    fn binding(&mut self, i: usize, scope: usize) -> &mut Binding<'a> {
        let name = self.word(i);
        self.scopes[scope]
            .bindings
            .get_mut(name)
            .unwrap()
            .last_mut()
            .unwrap()
    }
    fn delimit(&self, start: usize, tokens: &[&str]) -> usize {
        let mut i = start;
        while i < self.t.len() {
            let w = self.word(i);
            if tokens.contains(&w) {
                break;
            }
            if matches!(w, "(" | "[") && self.pairs[i] < self.t.len() {
                i = self.pairs[i]
            }
            i += 1
        }
        i
    }
    fn parameters(&self, open: usize) -> Vec<(usize, usize)> {
        let close = self.pairs[open];
        if close >= self.t.len() {
            return vec![];
        }
        let mut spans = vec![];
        let mut start = open + 1;
        let mut i = start;
        while i <= close {
            if i == close || self.word(i) == "," {
                if start < i && !(i == start + 1 && self.word(start) == "void") {
                    spans.push((start, i))
                }
                start = i + 1;
            } else if matches!(self.word(i), "(" | "[" | "{") && self.pairs[i] < close {
                i = self.pairs[i]
            }
            i += 1;
        }
        spans
    }
    fn declared_type(&self, i: usize, scope: usize) -> Option<(&'a str, bool)> {
        let mut j = i.checked_sub(1)?;
        while matches!(self.word(j), "*" | "&" | "?" | "]") {
            if self.word(j) == "]" {
                j = self.pairs[j]
            }
            j = j.checked_sub(1)?
        }
        let ty = self.word(j);
        if self.t.get(j)?.kind != Kind::Word {
            return None;
        }
        if primitive(ty) {
            return Some((ty, true));
        }
        let tags = self.lookup(scope, ty, Some(Role::Type), i);
        if let Some(tag) = tags.first() {
            return Some((
                ty,
                tag.body.is_some() || self.lang == Language::Csharp || self.lang == Language::Swift,
            ));
        }
        if matches!(self.prev(j), "struct" | "class" | "enum" | "union") {
            return Some((ty, false));
        }
        if !keyword(ty) && (ty.ends_with("_t") || ty.chars().next().is_some_and(char::is_uppercase))
        {
            return Some((ty, true));
        }
        None
    }
    fn directives(&mut self) {
        let mut i = 0;
        let mut active = true;
        let mut stack = vec![];
        while i < self.t.len() {
            let line = self.t[i].line;
            let mut end = i + 1;
            while end < self.t.len() && self.t[end].line == line {
                end += 1
            }
            if self.word(i) == "#" {
                self.skip[i..end].fill(true);
                let command = self.word(i + 1);
                match command {
                    "if" | "ifdef" | "ifndef" => {
                        let lit = if command == "if" {
                            match self.word(i + 2) {
                                "1" if i + 3 == end => Some(true),
                                "0" if i + 3 == end => Some(false),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        stack.push((active, lit));
                        active = active && lit == Some(true)
                    }
                    "else" => {
                        if let Some(&(parent, lit)) = stack.last() {
                            active = parent && lit == Some(false)
                        }
                    }
                    "elif" => {
                        if let Some((parent, taken)) = stack.last_mut() {
                            let next = match self.word(i + 2) {
                                "1" if i + 3 == end => Some(true),
                                "0" if i + 3 == end => Some(false),
                                _ => None,
                            };
                            active = *parent && *taken == Some(false) && next == Some(true);
                            if *taken == Some(false) {
                                *taken = next
                            }
                        }
                    }
                    "endif" => {
                        if let Some((parent, _)) = stack.pop() {
                            active = parent
                        }
                    }
                    "define" if active && self.t.get(i + 2).is_some_and(ident) => {
                        self.add(i + 2, 0, "local_binding", Role::Value);
                    }
                    _ => {}
                }
            } else if !active {
                self.skip[i..end].fill(true)
            }
            i = end;
        }
    }
    fn container(&mut self, i: usize, scope: usize, prev: &str) -> bool {
        let (kind, role) = match (self.lang, prev) {
            (_, "namespace") => ("local_binding", Role::Namespace),
            (Language::C | Language::Cpp, "struct") => ("struct_specifier", Role::Type),
            (Language::C | Language::Cpp, "enum") => ("enum_specifier", Role::Type),
            (Language::C | Language::Cpp, "union") => ("local_binding", Role::Type),
            (Language::Cpp, "class") => ("class_specifier", Role::Type),
            (Language::Csharp, "class") => ("class_declaration", Role::Type),
            (Language::Csharp, "struct") => ("struct_declaration", Role::Type),
            (Language::Csharp, "enum") => ("enum_declaration", Role::Type),
            (Language::Csharp, "interface") => ("interface_declaration", Role::Type),
            (Language::Csharp, "record") => ("record_declaration", Role::Type),
            (Language::Swift, "struct" | "class" | "enum" | "actor" | "extension") => {
                ("class_declaration", Role::Type)
            }
            (Language::Swift, "protocol") => ("protocol_declaration", Role::Type),
            _ => return false,
        };
        if role == Role::Namespace {
            let (mut parent, mut name) = (scope, i);
            loop {
                let key = self.word(name);
                let existing = self.scopes[parent]
                    .bindings
                    .get(key)
                    .and_then(|bs| bs.iter().find(|b| b.role == role))
                    .and_then(|b| b.body);
                let child = if let Some(child) = existing {
                    self.declared[name] = true;
                    child
                } else {
                    let child = self.scope(parent, None);
                    self.scopes[child].container = Some(child);
                    self.add(name, parent, kind, role);
                    self.binding(name, parent).body = Some(child);
                    child
                };
                let separator = name + 1;
                if matches!(self.word(separator), ";" | "{") {
                    self.body.insert(separator, child);
                    break;
                }
                let next = if self.word(separator) == "." {
                    name + 2
                } else if self.word(separator) == ":" && self.word(separator + 1) == ":" {
                    name + 3
                } else {
                    break;
                };
                if !self.t.get(next).is_some_and(ident) {
                    break;
                }
                parent = child;
                name = next;
            }
            return true;
        }
        let name = self.word(i);
        let after = self.word(i + 1);
        let mut open = i + 1;
        if matches!(after, ":" | "<" | "where") {
            open = self.delimit(i + 1, &["{", ";", "}"])
        }
        let existing = self
            .lookup(scope, name, Some(role), i)
            .first()
            .and_then(|b| b.body);
        if prev == "extension" && existing.is_none() {
            return true;
        }
        if self.word(open) == "{" || role == Role::Namespace && self.word(open) == ";" {
            let child = existing.unwrap_or_else(|| self.scope(scope, None));
            self.scopes[child].container = Some(child);
            self.body.insert(open, child);
            if existing.is_none() || role != Role::Namespace && prev != "extension" {
                self.add(i, scope, kind, role);
                self.binding(i, scope).body = Some(child)
            } else {
                self.declared[i] = true
            }
        } else if existing.is_none() {
            self.add(i, scope, kind, role);
        } else {
            return false;
        }
        true
    }
    fn function(&mut self, i: usize, scope: usize) -> bool {
        let prev = self.prev(i);
        let open = i + 1;
        if self.word(open) != "(" || self.pairs[open] >= self.t.len() {
            return false;
        }
        let close = self.pairs[open];
        let mut body = close + 1;
        if self.lang == Language::Swift {
            if prev != "func" && self.word(i) != "init" {
                return false;
            }
            if matches!(self.word(body), "-" | "throws" | "async" | "where") {
                body = self.delimit(body, &["{", "}", ";", "func"])
            }
        } else {
            let constructor = self.scopes[scope].container == Some(scope)
                && self.scopes[scope].parent.is_some_and(|p| {
                    self.lookup(p, self.word(i), Some(Role::Type), i)
                        .iter()
                        .any(|b| b.body == Some(scope))
                });
            if self.declared_type(i, scope).is_none() && !constructor {
                return false;
            }
            if matches!(
                self.word(body),
                "const" | "noexcept" | "override" | "final" | "where"
            ) {
                body = self.delimit(body, &["{", ";", "}"])
            }
        }
        let has_body = self.word(body) == "{";
        let arrow =
            self.lang == Language::Csharp && self.word(body) == "=" && self.word(body + 1) == ">";
        if !has_body && !arrow && self.word(body) != ";" && self.lang != Language::Swift {
            return false;
        }
        let kind = match self.lang {
            Language::C | Language::Cpp if !has_body => "local_binding",
            Language::C | Language::Cpp => "function_definition",
            Language::Swift => "function_declaration",
            _ if self.declared_type(i, scope).is_none() => "constructor_declaration",
            _ => "method_declaration",
        };
        let id = self.add(i, scope, kind, Role::Function);
        let local = self.scope(scope, Some(id));
        if has_body {
            self.body.insert(body, local);
        } else if arrow {
            let end = self.delimit(body + 2, &[";", "}"]);
            for p in body..end {
                self.at[p] = local
            }
        }
        let mut labels = vec![];
        for (lo, hi) in self.parameters(open) {
            let end = (lo..hi).find(|&j| self.word(j) == "=").unwrap_or(hi);
            let (param, label, ty) = if self.lang == Language::Swift {
                let colon = (lo..end).find(|&j| self.word(j) == ":");
                let param = colon.and_then(|j| j.checked_sub(1)).filter(|&j| j >= lo);
                (param, self.word(lo), colon.map(|j| self.word(j + 1)))
            } else {
                let param = (lo..end)
                    .rev()
                    .find(|&j| ident(&self.t[j]) && !keyword(self.word(j)) && j > lo);
                (param, "", Some(self.word(lo)))
            };
            labels.push((label, ty));
            if let Some(param) = param {
                self.add(param, local, "parameter", Role::Value);
                self.binding(param, local).ty = ty;
            }
        }
        self.binding(i, scope).signature = Some(labels);
        true
    }
    fn declarations(&mut self) {
        self.directives();
        let mut scope = 0;
        let mut resume = vec![];
        for i in 0..self.t.len() {
            if self.at[i] == 0 {
                self.at[i] = scope
            }
            if self.skip[i] {
                continue;
            }
            let w = self.word(i);
            let prev = self.prev(i);
            match w {
                "{" => {
                    resume.push(scope);
                    scope = self
                        .body
                        .get(&i)
                        .copied()
                        .unwrap_or_else(|| self.scope(scope, None));
                    continue;
                }
                "}" => {
                    scope = resume.pop().unwrap_or(0);
                    continue;
                }
                ";" => {
                    if let Some(&ns) = self.body.get(&i) {
                        scope = ns
                    }
                    continue;
                }
                _ => {}
            }
            if !ident(&self.t[i]) || self.declared[i] || matches!(prev, "." | ">") {
                continue;
            }
            if self.container(i, scope, prev) {
                continue;
            }
            if keyword(w) && w != "init" && !(self.lang == Language::Swift && prev == "func") {
                continue;
            }
            if self.function(i, scope) {
                continue;
            }
            let (kind, ty, usable) = if self.lang == Language::Swift {
                if !matches!(prev, "let" | "var") {
                    continue;
                }
                let ty = if self.word(i + 1) == ":" {
                    Some(self.word(i + 2))
                } else {
                    None
                };
                ("property_declaration", ty, true)
            } else {
                if !matches!(self.word(i + 1), "=" | ";" | "," | "[" | "{" | ")") {
                    continue;
                }
                let Some((ty, complete)) = self.declared_type(i, scope) else {
                    continue;
                };
                (
                    if self.lang == Language::Csharp {
                        if self.word(i + 1) == "{" {
                            "property_declaration"
                        } else {
                            "variable_declarator"
                        }
                    } else {
                        "local_binding"
                    },
                    Some(ty),
                    complete,
                )
            };
            self.add(i, scope, kind, Role::Value);
            let b = self.binding(i, scope);
            b.ty = ty;
            b.usable = usable;
        }
    }
    fn qualifier(&self, i: usize) -> Option<usize> {
        if self.prev(i) == "." {
            i.checked_sub(2)
        } else if i >= 3 && matches!((self.word(i - 2), self.prev(i)), (":", ":") | ("-", ">")) {
            Some(i - 3)
        } else {
            None
        }
    }
    fn receiver(&self, i: usize, scope: usize) -> Option<Option<usize>> {
        let mut first = self.qualifier(i)?;
        let mut parts = vec![first];
        while let Some(prev) = self.qualifier(first) {
            first = prev;
            parts.push(prev)
        }
        let mut current = None;
        for q in parts.into_iter().rev() {
            let name = self.word(q);
            if !ident(&self.t[q]) {
                return Some(None);
            }
            if matches!(name, "this" | "self" | "Self") {
                current = self.scopes[scope].container;
                continue;
            }
            let choices = if let Some(owner) = current {
                self.scopes[owner]
                    .bindings
                    .get(name)
                    .map(|bs| bs.iter().collect())
                    .unwrap_or_default()
            } else {
                self.lookup(scope, name, None, i)
            };
            if choices.len() != 1 {
                return Some(None);
            }
            let b = choices[0];
            current = b.body.or_else(|| {
                b.ty.and_then(|ty| {
                    self.lookup(scope, ty, Some(Role::Type), i)
                        .first()
                        .and_then(|t| t.body)
                })
            });
            if current.is_none() {
                break;
            }
        }
        Some(current)
    }
    fn arg_type(&self, scope: usize, lo: usize, hi: usize) -> Option<&'a str> {
        let i = if self.lang == Language::Swift && self.word(lo + 1) == ":" {
            lo + 2
        } else {
            lo
        };
        if i >= hi {
            return None;
        }
        let s = self.word(i);
        let swift = self.lang == Language::Swift;
        if i + 1 == hi {
            if s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<i32>().is_ok() {
                return Some(if swift { "Int" } else { "int" });
            }
            if matches!(s, "true" | "false") {
                return Some(if swift { "Bool" } else { "bool" });
            }
            if self.t[i].kind == Kind::String && s.starts_with('"') {
                return match self.lang {
                    Language::Csharp => Some("string"),
                    Language::Swift => Some("String"),
                    _ => None,
                };
            }
            return self
                .lookup(scope, s, Some(Role::Value), i)
                .first()
                .and_then(|b| b.ty);
        }
        if i + 3 == hi
            && self.word(i + 1) == "."
            && [s, self.word(i + 2)]
                .iter()
                .all(|x| x.bytes().all(|c| c.is_ascii_digit()))
        {
            return Some(if swift { "Double" } else { "double" });
        }
        None
    }
    fn uses(&mut self) {
        for i in 0..self.t.len() {
            if self.skip[i]
                || self.declared[i]
                || !ident(&self.t[i])
                || (keyword(self.word(i))
                    && !(self.lang == Language::Swift && matches!(self.word(i), "set" | "get")))
            {
                continue;
            }
            let name = self.word(i);
            let s = self.at[i];
            let call = self.word(i + 1) == "(";
            let mut candidates = match self.receiver(i, s) {
                Some(Some(r)) => self.scopes[r]
                    .bindings
                    .get(name)
                    .map(|v| v.iter().collect())
                    .unwrap_or_default(),
                Some(None) => vec![],
                None => self.lookup(s, name, None, i),
            };
            if call {
                let args = self.parameters(i + 1);
                candidates.retain(|b| {
                    b.role == Role::Function
                        && b.signature.as_ref().is_some_and(|sig| {
                            if sig.len() != args.len() {
                                return false;
                            }
                            self.lang != Language::Swift
                                || sig.iter().zip(&args).all(|(&(label, _), &(lo, hi))| {
                                    let supplied = if lo + 1 < hi && self.word(lo + 1) == ":" {
                                        self.word(lo)
                                    } else {
                                        "_"
                                    };
                                    label == supplied
                                })
                        })
                });
                if candidates.len() > 1 {
                    let types = args
                        .iter()
                        .map(|&(lo, hi)| self.arg_type(s, lo, hi))
                        .collect::<Option<Vec<_>>>();
                    if let Some(types) = types {
                        let exact = candidates
                            .iter()
                            .copied()
                            .filter(|b| {
                                b.signature
                                    .as_ref()
                                    .unwrap()
                                    .iter()
                                    .zip(&types)
                                    .all(|((_, ty), arg)| *ty == Some(*arg))
                            })
                            .collect::<Vec<_>>();
                        if exact.len() == 1 {
                            candidates = exact
                        }
                    }
                }
            }
            candidates.retain(|b| b.usable);
            let targets = if candidates.len() == 1 {
                vec![candidates[0].id]
            } else {
                vec![]
            };
            if targets.is_empty() && !call {
                continue;
            }
            let id = self.out.calls.len();
            self.out.calls.push(Call {
                is_call: call,
                span: self.t[i].span.clone(),
                owner: self.scopes[s].owner,
            });
            if !targets.is_empty() {
                self.out.direct.push((id, targets))
            }
        }
    }
}
pub(super) fn analyze(lang: Language, t: &[Token]) -> Analysis {
    let mut a = Analyzer::new(lang, t);
    a.declarations();
    a.uses();
    a.out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn targets(lang: Language, text: &str, anchor: &str, suffix: &str) -> Vec<usize> {
        let t = super::super::lexer::tokens(text, lang);
        let a = analyze(lang, &t);
        let start = text.find(anchor).unwrap() + anchor.find(suffix).unwrap();
        let id = a.calls.iter().position(|c| c.span.start == start).unwrap();
        a.direct
            .iter()
            .find(|(site, _)| *site == id)
            .map(|(_, targets)| {
                targets
                    .iter()
                    .map(|&d| a.declarations[d].span.start)
                    .collect()
            })
            .unwrap_or_default()
    }
    #[test]
    fn cpp_namespaces_overloads_and_shadowing() {
        let text = "namespace A::B { int f(int x){return x;} int f(double x){return 2;} } namespace C { int f(int x){return x;} } int g(){int x=1;{int x=2;return A::B::f(x);}return C::f(x);}";
        assert_eq!(
            targets(Language::Cpp, text, "A::B::f(x)", "f(x)"),
            [text.find("f(int").unwrap()]
        );
        assert_eq!(
            targets(Language::Cpp, text, "C::f(x)", "f(x)"),
            [text.rfind("f(int").unwrap()]
        );
        assert_eq!(
            targets(Language::Cpp, text, "A::B::f(x)", "x)"),
            [text.find("x=2").unwrap()]
        );
    }
    #[test]
    fn swift_labels_and_extension_methods() {
        let t = "struct Box { func set(_ x: Int) {} } extension Box { func set(label x: Int) {} func run() { self.set(label: 1); self.set(2) } }";
        assert_eq!(
            targets(Language::Swift, t, "self.set(label: 1)", "set"),
            [t.find("set(label x").unwrap()]
        );
        assert_eq!(
            targets(Language::Swift, t, "self.set(2)", "set"),
            [t.find("set(_ x").unwrap()]
        );
    }
    #[test]
    fn csharp_file_scoped_namespace_and_typed_receiver() {
        let t = "namespace A.B; class Box { int Read(int x) {return x;} } class Other { int Read(int x){return 0;} int Run(Box box){return box.Read(1);} }";
        assert_eq!(
            targets(Language::Csharp, t, "box.Read(1)", "Read"),
            [t.find("Read").unwrap()]
        );
    }
}
