use super::{Language, ROOT, Role, Syntax, reserved};
use std::borrow::Cow;

impl Syntax<'_> {
    pub(super) fn extract(&mut self) {
        self.type_declarations();
        let mut i = 0;
        while i < self.t.len() {
            let s = self.w(i);
            match self.lang {
                Language::Go if s == "func" => self.keyword_function(i),
                Language::Go if matches!(s, "var" | "const") => self.go_values(i),
                Language::Kotlin if s == "fun" => self.keyword_function(i),
                Language::Kotlin if matches!(s, "val" | "var") && self.id(i + 1) => {
                    self.add(
                        i + 1,
                        "property_declaration",
                        Role::Value,
                        self.decl_scope(i),
                    );
                }
                Language::Php | Language::Hack if s.eq_ignore_ascii_case("function") => {
                    self.keyword_function(i)
                }
                Language::Php | Language::Hack if s.eq_ignore_ascii_case("const") => {
                    if let Some(end) = self.seek(i + 1, &["=", ";"])
                        && end > i + 1
                        && self.id(end - 1)
                    {
                        self.add(end - 1, "const_item", Role::Value, self.decl_scope(i));
                    }
                }
                Language::ObjectiveC
                    if matches!(s, "-" | "+")
                        && self.w(i + 1) == "("
                        && self.objc_type[i] != ROOT =>
                {
                    self.objc_method(i);
                }
                Language::ObjectiveC if s == "property" && self.prev(i) == "@" => {
                    if let Some(end) = self.seek(i + 1, &[";"])
                        && end > i + 1
                        && self.id(end - 1)
                    {
                        self.add(
                            end - 1,
                            "property_declaration",
                            Role::Value,
                            self.objc_type[i],
                        );
                    }
                }
                Language::Dart
                    if matches!(s, "get" | "set")
                        && self.id(i + 1)
                        && self.declaration_level(i) =>
                {
                    self.add(
                        i + 1,
                        "property_declaration",
                        Role::Value,
                        self.decl_scope(i),
                    );
                }
                Language::Java | Language::Dart | Language::ObjectiveC
                    if self.id(i)
                        && !reserved(s)
                        && self.w(i + 1) == "("
                        && self.declaration_level(i) =>
                {
                    self.typed_function(i);
                }
                Language::Java | Language::Dart | Language::Php | Language::Hack
                    if !self.in_parameters[i]
                        && self.types.contains_key(&self.parent[i])
                        && matches!(self.w(i + 1), "=" | ";" | ",") =>
                {
                    let php = matches!(self.lang, Language::Php | Language::Hack);
                    let field = if php {
                        s.starts_with('$') && s.len() > 1
                    } else {
                        self.id(i)
                            && i > 0
                            && (self.id(i - 1) || matches!(self.prev(i), "]" | ">" | "?"))
                    };
                    if field && !self.prev(i).eq_ignore_ascii_case("const") {
                        self.add(i, "property_declaration", Role::Value, self.decl_scope(i));
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    fn decl_scope(&self, i: usize) -> usize {
        if self.lang == Language::ObjectiveC && self.objc_type[i] != ROOT {
            return self.objc_type[i];
        }
        let brace = self.parent[i];
        if self.namespace_blocks.contains(&brace) {
            return ROOT;
        }
        // Non-type braces get a distinct scope, so nested callable declarations
        // cannot become candidates for bare file-level calls.
        self.types.get(&brace).copied().unwrap_or(brace)
    }
    fn declaration_level(&self, i: usize) -> bool {
        !self.in_parameters[i]
            && (self.parent[i] == ROOT
                || self.types.contains_key(&self.parent[i])
                || self.namespace_blocks.contains(&self.parent[i]))
    }
    fn type_declarations(&mut self) {
        let mut objc = ROOT;
        for i in 0..self.t.len() {
            let s = self.w(i);
            if self.lang == Language::ObjectiveC {
                if self.prev(i) == "@" {
                    match s {
                        "end" => objc = ROOT,
                        "interface" | "implementation" | "protocol" if self.id(i + 1) => {
                            objc = i + 1;
                            let kind = if s == "protocol" {
                                "protocol_declaration"
                            } else {
                                "class_declaration"
                            };
                            self.add(i + 1, kind, Role::Type, ROOT);
                            // Both @interface and @implementation name the same
                            // type. Keep the written declarations separate.
                        }
                        _ => {}
                    }
                }
                self.objc_type[i] = objc;
                continue;
            }
            if !self.id(i) {
                continue;
            }
            let keyword = if matches!(self.lang, Language::Php | Language::Hack)
                && s.as_bytes().iter().any(u8::is_ascii_uppercase)
            {
                Cow::Owned(s.to_ascii_lowercase())
            } else {
                Cow::Borrowed(s)
            };
            let kind = match (self.lang, keyword.as_ref()) {
                (Language::Go, "type") => {
                    if self.w(i + 1) == "(" {
                        self.go_group(i + 1, true);
                    }
                    if self.w(i + 2) == "struct" {
                        "struct_declaration"
                    } else if self.w(i + 2) == "interface" {
                        "interface_declaration"
                    } else {
                        "type_alias_declaration"
                    }
                }
                (
                    Language::Java
                    | Language::Kotlin
                    | Language::Dart
                    | Language::Php
                    | Language::Hack,
                    "class",
                ) => "class_declaration",
                (
                    Language::Java | Language::Kotlin | Language::Php | Language::Hack,
                    "interface",
                ) => "interface_declaration",
                (Language::Java | Language::Dart | Language::Php | Language::Hack, "enum") => {
                    "enum_declaration"
                }
                (Language::Java, "record") => "record_declaration",
                (Language::Kotlin, "object") => "class_declaration",
                (Language::Kotlin, "typealias") | (Language::Hack, "type" | "newtype") => {
                    "type_alias_declaration"
                }
                (Language::Dart, "mixin" | "extension") => "class_declaration",
                (Language::Dart, "typedef") => "type_alias_declaration",
                (Language::Php | Language::Hack, "trait") => "trait_item",
                _ => continue,
            };
            if !self.id(i + 1)
                || self.prev(i) == "."
                || self.prev(i) == ":"
                || self.w(i + 1) == "on"
            {
                continue;
            }
            let kind = if self.lang == Language::Kotlin && self.prev(i) == "enum" && s == "class" {
                "enum_declaration"
            } else {
                kind
            };
            self.add(i + 1, kind, Role::Type, self.decl_scope(i));
            if self.lang != Language::Go && kind != "type_alias_declaration" {
                let mut j = i + 2;
                while j < self.t.len() {
                    let v = self.w(j);
                    if v == "{" {
                        self.types.insert(j, i + 1);
                        break;
                    }
                    if matches!(
                        v,
                        ";" | "}"
                            | "="
                            | "class"
                            | "fun"
                            | "function"
                            | "object"
                            | "interface"
                            | "val"
                            | "var"
                    ) {
                        break;
                    }
                    if matches!(v, "(" | "[") {
                        let Some(close) = self.close(j) else { break };
                        j = close;
                    }
                    j += 1;
                }
            }
        }
    }
    fn go_values(&mut self, i: usize) {
        if self.w(i + 1) == "(" {
            self.go_group(i + 1, false);
            return;
        }
        let mut j = i + 1;
        while self.id(j) {
            self.add(
                j,
                if self.w(i) == "const" {
                    "const_item"
                } else {
                    "variable_declarator"
                },
                Role::Value,
                self.decl_scope(i),
            );
            if self.w(j + 1) != "," {
                break;
            }
            j += 2;
        }
    }
    fn go_group(&mut self, open: usize, types: bool) {
        let Some(end) = self.close(open) else { return };
        let mut j = open + 1;
        let mut start = true;
        while j < end {
            if start && self.id(j) {
                let (kind, role) = if types {
                    (
                        match self.w(j + 1) {
                            "struct" => "struct_declaration",
                            "interface" => "interface_declaration",
                            _ => "type_alias_declaration",
                        },
                        Role::Type,
                    )
                } else {
                    (
                        if self.prev(open) == "const" {
                            "const_item"
                        } else {
                            "variable_declarator"
                        },
                        Role::Value,
                    )
                };
                self.add(j, kind, role, self.decl_scope(open));
            }
            if matches!(self.w(j), "(" | "[" | "{") {
                let Some(close) = self.close(j) else { break };
                j = close;
            }
            start = self.w(j) == ";"
                || self
                    .t
                    .get(j + 1)
                    .is_some_and(|next| next.line > self.t[j].line)
                    && !matches!(self.w(j), "=" | "," | "+" | "-" | "*" | "/");
            j += 1;
        }
    }
    fn keyword_function(&mut self, start: usize) {
        let mut j = start + 1;
        let mut method = !self.declaration_level(start) || self.decl_scope(start) != ROOT;
        let mut scope = self.decl_scope(start);
        let mut receiver = None;
        if self.lang == Language::Go && self.w(j) == "(" {
            let Some(end) = self.close(j) else { return };
            // A Go method receiver is a named local type, optionally a pointer.
            // Anonymous func literals have no method name after the parameters.
            if !self.id(end + 1) || self.w(end + 2) != "(" {
                return;
            }
            method = true;
            let ty = (j + 1..end)
                .take_while(|&k| self.w(k) != "[")
                .filter(|&k| self.id(k))
                .last();
            receiver = ty.filter(|&k| k != j + 1 && self.id(j + 1)).map(|_| j + 1);
            scope = ty
                .and_then(|k| {
                    self.decls
                        .iter()
                        .find(|d| d.role == Role::Type && d.key == self.w(k))
                        .map(|d| d.at)
                })
                .unwrap_or(start);
            j = end + 1;
        } else if self.w(j) == "&" {
            j += 1;
        }
        if self.lang == Language::Kotlin {
            if self.w(j) == "<" {
                let Some(end) = self.angle_end(j) else { return };
                j = end + 1;
            }
            while self.id(j) && self.w(j + 1) == "." {
                method = true;
                scope = start;
                j += 2;
            }
        }
        if !self.id(j) {
            return;
        }
        let name = j;
        j += 1;
        if self.lang == Language::Go && self.w(j) == "[" {
            let Some(end) = self.close(j) else { return };
            j = end + 1;
        }
        if self.lang == Language::Hack && self.w(j) == "<" {
            let Some(end) = self.angle_end(j) else { return };
            j = end + 1;
        }
        if self.w(j) != "(" {
            return;
        }
        let role = if method { Role::Method } else { Role::Function };
        let kind = if method {
            "method_declaration"
        } else {
            "function_declaration"
        };
        let id = self.add(name, kind, role, scope);
        self.decls[id].receiver = receiver;
        if self.lang == Language::Kotlin
            && method
            && (self.has_modifier(start, &["open", "override", "abstract"])
                || (scope != ROOT && self.prev(scope) == "interface"))
        {
            self.decls[id].bindable = false;
        }
        if matches!(self.lang, Language::Php | Language::Hack) && method {
            self.decls[id].bindable = self.has_modifier(start, &["final", "private", "static"]);
        }
        self.function_body(id, start, j);
    }
    pub(super) fn angle_end(&self, start: usize) -> Option<usize> {
        let mut depth = 0;
        for i in start..self.t.len() {
            match self.w(i) {
                "<" => depth += 1,
                ">" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                "{" | "}" | ";" => break,
                _ => {}
            }
        }
        None
    }
    fn has_modifier(&self, start: usize, words: &[&str]) -> bool {
        (0..start)
            .rev()
            .take_while(|&j| !matches!(self.w(j), ";" | "{" | "}" | "=" | "(" | ")"))
            .any(|j| words.contains(&self.w(j)))
    }
    fn typed_function(&mut self, name: usize) {
        let class = self.decl_scope(name);
        if self.lang == Language::Java && !self.types.contains_key(&self.parent[name]) {
            return;
        }
        if self.lang == Language::ObjectiveC && self.objc_type[name] != ROOT {
            return;
        }
        if matches!(
            self.prev(name),
            "." | "@"
                | "="
                | ":"
                | "return"
                | "new"
                | "set"
                | "get"
                | "class"
                | "record"
                | "interface"
        ) {
            return;
        }
        let constructor = class != ROOT && self.w(class) == self.w(name);
        let prev_word = name.checked_sub(1).is_some_and(|p| self.id(p))
            || matches!(self.prev(name), "]" | "*" | "?" | ">");
        if !constructor && !prev_word && self.lang != Language::Dart {
            return;
        }
        // An initializer, annotation argument or expression is not a method signature.
        let mut j = name;
        while j > 0 {
            j -= 1;
            match self.w(j) {
                ";" | "{" | "}" => break,
                "=" | "(" | "[" | "return" | "new" | "->" => return,
                ")" | "]" => {
                    let Some(open) = self.close(j) else { return };
                    j = open;
                }
                _ => {}
            }
        }
        let Some(end) = self.close(name + 1) else {
            return;
        };
        let next = self.w(end + 1);
        if !matches!(next, "{" | ";" | "throws" | "async" | "sync" | "=" | ":") {
            return;
        }
        if self.lang == Language::Dart && next == ";" && class == ROOT {
            return;
        }
        if self.lang == Language::Dart && next == "=" && self.w(end + 2) != ">" {
            return;
        }
        if self.lang == Language::ObjectiveC && next != "{" && next != ";" {
            return;
        }
        let role = if class != ROOT {
            Role::Method
        } else {
            Role::Function
        };
        let kind = if constructor {
            "constructor_declaration"
        } else if role == Role::Method {
            "method_declaration"
        } else {
            "function_declaration"
        };
        let id = self.add(name, kind, role, class);
        if role == Role::Method {
            self.decls[id].bindable = constructor
                || self.has_modifier(name, &["private", "static", "final"])
                || self.has_modifier(class, &["final"]);
        }
        self.function_body(id, name, name + 1);
    }
    fn callable_parameters(&mut self, function: usize, open: usize, end: usize) {
        if !matches!(self.lang, Language::Go | Language::Kotlin | Language::Dart) {
            return;
        }
        let mut first = open + 1;
        let mut i = first;
        while i <= end {
            if i == end || self.w(i) == "," {
                let name = match self.lang {
                    Language::Go if self.id(first) && self.w(first + 1) == "func" => Some(first),
                    Language::Kotlin
                        if self.id(first)
                            && self.w(first + 1) == ":"
                            && self.w(first + 2) == "(" =>
                    {
                        self.close(first + 2)
                            .filter(|&j| j < i && self.w(j + 1) == "-" && self.w(j + 2) == ">")
                            .map(|_| first)
                    }
                    Language::Dart
                        if i > first
                            && self.id(i - 1)
                            && (first..i - 1).any(|j| self.w(j) == "Function") =>
                    {
                        Some(i - 1)
                    }
                    _ => None,
                };
                if let Some(name) = name {
                    self.add(name, "parameter", Role::Value, self.decls[function].at);
                }
                first = i + 1;
            } else if matches!(self.w(i), "(" | "[" | "{") {
                let Some(close) = self.close(i) else { return };
                i = close;
            }
            i += 1;
        }
    }
    fn function_body(&mut self, id: usize, start: usize, params: usize) {
        let Some(end) = self.close(params) else {
            return;
        };
        self.callable_parameters(id, params, end);
        self.decls[id].signature = Some(start..end + 1);
        let mut i = end + 1;
        while i < self.t.len() {
            match self.w(i) {
                "{" => {
                    if let Some(stop) = self.close(i) {
                        self.decls[id].body = Some(i + 1..stop);
                    }
                    break;
                }
                "=" if matches!(self.lang, Language::Kotlin | Language::Dart) => {
                    let mut stop = i + 1;
                    while stop < self.t.len() && !matches!(self.w(stop), ";" | "}") {
                        if matches!(self.w(stop), "(" | "[" | "{") {
                            let Some(close) = self.close(stop) else { break };
                            stop = close;
                        }
                        if self.lang == Language::Kotlin
                            && stop > i + 1
                            && self.t[stop].line > self.t[stop - 1].line
                            && matches!(
                                self.w(stop),
                                "fun" | "val" | "var" | "class" | "private" | "public" | "internal"
                            )
                        {
                            break;
                        }
                        stop += 1;
                    }
                    self.decls[id].body = Some(i + 1..stop);
                    break;
                }
                ";" | "}" | "func" | "fun" | "function" | "class" | "val" | "var" => break,
                "(" | "[" => {
                    let Some(close) = self.close(i) else { break };
                    i = close;
                }
                _ => {}
            }
            i += 1;
        }
    }
    fn objc_method(&mut self, start: usize) {
        let Some(ret) = self.close(start + 1) else {
            return;
        };
        let name = ret + 1;
        if !self.id(name) {
            return;
        }
        let mut key = format!("{}{}", self.w(start), self.w(name));
        let mut j = name + 1;
        if self.w(j) == ":" {
            key.push(':');
            j += 1;
            while j < self.t.len() && !matches!(self.w(j), "{" | ";" | "}" | "__attribute__") {
                if self.w(j) == "(" {
                    let Some(close) = self.close(j) else { return };
                    j = close;
                } else if self.id(j) && self.w(j + 1) == ":" {
                    key.push_str(self.w(j));
                    key.push(':');
                    j += 1;
                }
                j += 1;
            }
        }
        let mut direct = false;
        while self.w(j) == "__attribute__" && self.w(j + 1) == "(" {
            let Some(end) = self.close(j + 1) else { return };
            let mut attr = j + 3; // __attribute__((name, ...))
            while attr < end && self.w(attr) != ")" {
                direct |= self.w(attr) == "objc_direct";
                attr += 1;
                if self.w(attr) == "(" {
                    let Some(close) = self.close(attr) else {
                        return;
                    };
                    attr = close + 1;
                }
                if self.w(attr) != "," {
                    break;
                }
                attr += 1;
            }
            j = end + 1;
        }
        if !matches!(self.w(j), "{" | ";") {
            return;
        }
        let class = self.objc_type[start];
        let id = self.add(name, "method_declaration", Role::Method, class);
        // Objective-C message dispatch is dynamic except for explicitly direct methods.
        self.decls[id].bindable = direct;
        self.decls[id].key = key;
        self.decls[id].signature = Some(start..j);
        if self.w(j) == "{"
            && let Some(end) = self.close(j)
        {
            self.decls[id].body = Some(j + 1..end);
        }
    }
}
