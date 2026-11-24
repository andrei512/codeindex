//! Written declarations and lexically provable uses for C-family source files.
//! A scope indexes distinct type/tag, value and namespace bindings. A use follows
//! its lexical parents or an explicit receiver. Unproved dispatch remains unresolved.
use super::{Analysis, Call, Declaration, Kind, Language, Token};
use std::collections::HashMap;
mod directives;
mod owner_macro;
mod condition;
mod standard;
pub(crate) mod sdk;
pub(crate) mod project;
pub(super) mod csharp;

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
    is_const: bool,
    definition: bool,
    signature: Option<Vec<(&'a str, Option<&'a str>)>>,
    usable: bool,
}
#[derive(Default)]
struct Scope<'a> {
    parent: Option<usize>,
    owner: Option<usize>,
    const_receiver: bool,
    container: Option<usize>,
    bindings: HashMap<&'a str, Vec<Binding<'a>>>,
    external_owners: HashMap<std::borrow::Cow<'a, str>, usize>,
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
    macro_owners: HashMap<usize, String>,
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
    fn is_keyword(&self, name: &str) -> bool {
        keyword(name) && !(self.lang == Language::Cpp && matches!(name, "get" | "set"))
    }
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
            macro_owners: HashMap::new(),
        }
    }
    fn scope(&mut self, parent: usize, owner: Option<usize>) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            owner: owner.or(self.scopes[parent].owner),
            container: self.scopes[parent].container,
            const_receiver: self.scopes[parent].const_receiver,
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
                is_const: false,
                definition: false,
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
        if self.word(j) == ">" && matches!(self.lang, Language::Cpp | Language::Csharp) {
            let mut depth = 1;
            while depth > 0 {
                j = j.checked_sub(1)?;
                match self.word(j) {
                    ">" => depth += 1,
                    "<" => depth -= 1,
                    ";" | "{" | "}" => return None,
                    _ => {}
                }
            }
            j = j.checked_sub(1)?;
            return ident(&self.t[j]).then(|| (self.word(j), true));
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
    fn has_written_return(&self, name: usize) -> bool {
        let mut start = name;
        while start > 0 && matches!(self.prev(start), "*" | "&" | "const" | "volatile") {
            start -= 1;
        }
        let Some(mut j) = start.checked_sub(1) else {
            return false;
        };
        if self.word(j) == ")" {
            // Function-like annotations often wrap the return type.
            let Some(annotation) = self.pairs[j].checked_sub(1) else {
                return false;
            };
            if annotation >= j {
                return false;
            }
            j = annotation;
        }
        ident(&self.t[j]) && !keyword(self.word(j))
    }
    fn external_owner(&self, mut scope: usize, name: &str) -> Option<usize> {
        loop {
            if let Some(owner) = self.scopes[scope].external_owners.get(name) {
                return Some(*owner);
            }
            scope = self.scopes[scope].parent?;
        }
    }
    fn function(&mut self, i: usize, enclosing: usize) -> bool {
        let destructor = self.lang == Language::Cpp && self.prev(i) == "~";
        let qualified_end = if destructor { i - 1 } else { i };
        let mut declarator = qualified_end;
        if self.lang == Language::Cpp {
            while declarator >= 3
                && self.prev(declarator) == ":"
                && self.word(declarator - 2) == ":"
            {
                declarator -= 3;
            }
        }
        let prev = self.prev(i);
        let open = i + 1;
        if self.word(open) != "(" || self.pairs[open] >= self.t.len() {
            return false;
        }
        let macro_owner = if self.lang == Language::Cpp
            && declarator == qualified_end
            && declarator > 0
        {
            self.macro_owners.remove(&(declarator - 1))
        } else {
            None
        };
        if macro_owner.is_some() {
            declarator -= 1;
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
            let scope = enclosing;
            let constructor = macro_owner.as_deref() == Some(self.word(i))
                || (macro_owner.is_none() && declarator < qualified_end
                    && self.word(qualified_end - 3) == self.word(i))
                || self.scopes[scope].container == Some(scope)
                    && self.scopes[scope].parent.is_some_and(|p| {
                        self.lookup(p, self.word(i), Some(Role::Type), i)
                            .iter()
                            .any(|b| b.body == Some(scope))
                    });
            if constructor && self.word(body) == ":" {
                let mut j = body + 1;
                loop {
                    j = self.delimit(j, &["(", "{", ";", "}"]);
                    if !matches!(self.word(j), "(" | "{") || self.pairs[j] >= self.t.len() {
                        return false;
                    }
                    j = self.pairs[j] + 1;
                    if self.word(j) == "{" {
                        body = j;
                        break;
                    }
                    if self.word(j) != "," {
                        return false;
                    }
                    j += 1;
                }
            }
            let typed = self.declared_type(declarator, enclosing).is_some();
            // At namespace/file scope a body after a parameter list establishes a
            // function even when the return typedef or annotation comes from a header.
            let written_return = matches!(self.lang, Language::C | Language::Cpp)
                && self.scopes[enclosing].owner.is_none()
                && matches!(self.word(body), "{" | "const" | "noexcept")
                && self.has_written_return(declarator);
            if !typed && !written_return && !constructor {
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
        let scope = if let Some(name) = macro_owner {
            if !has_body || self.scopes[enclosing].owner.is_some() {
                return false;
            }
            let Some(owner) = self.macro_owner_scope(enclosing, name, i) else {
                return false;
            };
            owner
        } else if declarator == qualified_end {
            enclosing
        } else if let Some(Some(scope)) = self.receiver(qualified_end, enclosing) {
            scope
        } else if has_body && self.scopes[enclosing].owner.is_none() {
            // Out-of-class definitions can introduce an owner known only in a
            // header. Keep the scope without manufacturing a type declaration.
            let mut scope = enclosing;
            for part in (declarator..qualified_end).step_by(3) {
                let name = self.word(part);
                if !ident(&self.t[part]) {
                    return false;
                }
                if let Some(body) = self.scopes[scope]
                    .bindings
                    .get(name)
                    .and_then(|bs| bs.iter().find_map(|b| b.body))
                    .or_else(|| self.scopes[scope].external_owners.get(name).copied())
                {
                    scope = body;
                } else {
                    let body = self.scope(scope, None);
                    self.scopes[body].container = Some(body);
                    self.scopes[scope].external_owners.insert(name.into(), body);
                    scope = body;
                }
            }
            scope
        } else {
            return false;
        };
        if self.lang == Language::Cpp
            && !has_body
            && self.scopes[scope].owner.is_some()
            && self.parameters(open).iter().any(|&(lo, hi)| {
                self.arg_type(scope, lo, hi).is_some()
                    || (lo..hi).any(|j| matches!(self.word(j), "!" | "+" | "." | "/"))
            })
        {
            return false; // A value initialized with constructor arguments.
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
                let type_name = (lo..end)
                    .find(|&j| {
                        ident(&self.t[j])
                            && !matches!(
                                self.word(j),
                                "const"
                                    | "volatile"
                                    | "ref"
                                    | "out"
                                    | "in"
                                    | "this"
                                    | "struct"
                                    | "class"
                            )
                    })
                    .map(|j| self.word(j));
                let ty = if type_name == Some("char") && (lo..end).any(|j| self.word(j) == "*") {
                    Some("char*")
                } else {
                    type_name
                };
                (param, "", ty)
            };
            labels.push((label, ty));
            if let Some(param) = param {
                self.add(param, local, "parameter", Role::Value);
                self.binding(param, local).ty = ty;
                self.binding(param, local).is_const = (lo..end).any(|j| self.word(j) == "const");
            }
        }
        let result_type = self.declared_type(declarator, enclosing).map(|(ty, _)| ty);
        self.scopes[local].const_receiver = self.word(close + 1) == "const";
        let is_const = self.scopes[local].const_receiver;
        let binding = self.binding(i, scope);
        binding.ty = result_type;
        binding.signature = Some(labels);
        binding.is_const = is_const;
        binding.definition = has_body || arrow;
        true
    }
    fn declarations(&mut self) {
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
                "{" if self.lang == Language::Cpp && prev == "namespace" => {
                    // The implicit using-directive makes unnamed namespace members
                    // available in their enclosing namespace within this file.
                    resume.push(scope);
                    continue;
                }
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
            if !ident(&self.t[i])
                || self.declared[i]
                || (prev == "." || (prev == ">" && (i < 2 || self.word(i - 2) == "-")))
            {
                continue;
            }
            if self.container(i, scope, prev) {
                continue;
            }
            if self.is_keyword(w)
                && w != "init"
                && !(self.lang == Language::Swift && prev == "func")
            {
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
                if self.lang == Language::C && self.word(i + 1) == ";" {
                    let start = (0..i)
                        .rev()
                        .take_while(|&j| !matches!(self.word(j), ";" | "{" | "}"))
                        .any(|j| self.word(j) == "typedef");
                    if start {
                        self.add(i, scope, "local_binding", Role::Type);
                        continue;
                    }
                }
                if !(matches!(self.word(i + 1), "=" | ";" | "," | "[" | "{" | ")")
                    || self.lang == Language::Cpp && self.word(i + 1) == "(")
                {
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
            let ty = if self.lang == Language::Cpp && ty == Some("auto") && self.word(i + 1) == "="
            {
                self.initializer_type(i + 2, scope).or(ty)
            } else {
                ty
            };
            let is_const = self.lang == Language::Cpp
                && (i.saturating_sub(2)..i).any(|j| self.word(j) == "const");
            self.add(i, scope, kind, Role::Value);
            let b = self.binding(i, scope);
            b.ty = ty;
            b.is_const = is_const;
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
        if self.lang == Language::Cpp && self.word(first) == ")" {
            let constructor = self.pairs[first].checked_sub(1)?;
            let choices = self.lookup(scope, self.word(constructor), Some(Role::Type), i);
            return Some(match choices.as_slice() {
                [ty] => ty.body,
                _ => None,
            });
        }
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
            if choices.is_empty() {
                current = if let Some(owner) = current {
                    self.scopes[owner].external_owners.get(name).copied()
                } else {
                    self.external_owner(scope, name)
                };
                if current.is_some() {
                    continue;
                }
            }
            if choices.len() != 1 {
                return Some(None);
            }
            let b = choices[0];
            current = b.body.or_else(|| {
                b.ty.and_then(|ty| {
                    self.lookup(scope, ty, Some(Role::Type), i)
                        .first()
                        .and_then(|t| t.body)
                        .or_else(|| self.external_owner(scope, ty))
                })
            });
            if current.is_none() {
                break;
            }
        }
        Some(current)
    }
    fn initializer_type(&self, i: usize, scope: usize) -> Option<&'a str> {
        if self.word(i + 1) != "(" {
            return None;
        }
        let close = self.pairs[i + 1];
        if self.word(close + 1) == "." && self.word(close + 3) == "(" {
            let end = self.pairs[close + 3];
            if self.word(end + 1) != ";" {
                return None;
            }
            let owner = self.receiver(close + 2, scope)??;
            let bindings = self.scopes[owner].bindings.get(self.word(close + 2))?;
            let [method] = bindings.as_slice() else {
                return None;
            };
            method.ty
        } else if self.word(close + 1) == ";" {
            let bindings = self.lookup(scope, self.word(i), Some(Role::Type), i);
            (!bindings.is_empty()).then(|| self.word(i))
        } else {
            None
        }
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
                    Language::Cpp => Some("char*"),
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
                || (self.is_keyword(self.word(i))
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
            // A call through a parameter names that binding. Its runtime callee
            // is unknown, so only a lexical reference may enter the index.
            let parameter_call = call && self.qualifier(i).is_none()
                && matches!(candidates.as_slice(), [b] if self.out.declarations[b.id].kind == "parameter");
            if call && !parameter_call {
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
                if matches!(self.lang, Language::C | Language::Cpp) && candidates.len() > 1 {
                    // A written prototype and its definition denote the same function.
                    let definitions: Vec<_> = candidates
                        .iter()
                        .filter(|b| b.definition)
                        .map(|b| (&b.signature, b.is_const))
                        .collect();
                    candidates.retain(|b| {
                        b.definition || !definitions.contains(&(&b.signature, b.is_const))
                    });
                }
                if self.lang == Language::Cpp && candidates.len() > 1 {
                    let receiver_const = match self.qualifier(i) {
                        Some(q) if self.word(q) == "this" => Some(self.scopes[s].const_receiver),
                        Some(q) => match self
                            .lookup(s, self.word(q), Some(Role::Value), i)
                            .as_slice()
                        {
                            [binding] => Some(binding.is_const),
                            _ => None,
                        },
                        None if self.scopes[s].container.is_some() => {
                            Some(self.scopes[s].const_receiver)
                        }
                        None => None,
                    };
                    match receiver_const {
                        Some(true) => candidates.retain(|b| b.is_const),
                        Some(false) => {
                            let mutable: Vec<_> = candidates
                                .iter()
                                .filter(|b| !b.is_const)
                                .map(|b| &b.signature)
                                .collect();
                            candidates.retain(|b| !b.is_const || !mutable.contains(&&b.signature));
                        }
                        None => {}
                    }
                }
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
            if self.lang == Language::Csharp && call && candidates.len() > 1 {
                let call_id = self.out.calls.len();
                if let Some(site) = self.csharp_deferred(i, s, call_id, &candidates) {
                    csharp::record(&mut self.out, site);
                }
            }
            if targets.is_empty() && !call {
                continue;
            }
            let id = self.out.calls.len();
            self.out.calls.push(Call {
                is_call: call && !parameter_call,
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
    a.directives();
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
    fn undeclared_owner_does_not_merge_with_another_class() {
        let src = "void A::f() {} void B::f() {} A::~A() { f(); }";
        assert_eq!(
            targets(Language::Cpp, src, "{ f();", "f()"),
            [src.find("f()").unwrap()]
        );
    }

    #[test]
    fn unresolved_condition_does_not_activate_either_branch() {
        let src = "#if UNKNOWN\nvoid a() {}\n#else\nvoid b() {}\n#endif\nvoid c() {}";
        let tokens = super::super::lexer::tokens(src, Language::C);
        let analysis = analyze(Language::C, &tokens);
        let functions: Vec<_> = analysis
            .declarations
            .iter()
            .filter(|d| d.kind == "function_definition")
            .map(|d| &src[d.span.clone()])
            .collect();
        assert_eq!(functions, ["c"]);
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
