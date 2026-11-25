//! File-local declarations and direct calls in brace-delimited languages.
//! A method key includes its declaring type. An Objective-C key additionally
//! includes the full selector and whether it is an instance or class method.
use super::super::{Analysis, Call, Declaration, Kind, Language, Token};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

mod declarations;
mod scan;
mod shadows;
#[cfg(test)]
mod tests;

const ROOT: usize = usize::MAX;
#[derive(Clone, Copy, Eq, PartialEq)]
enum Role {
    Type,
    Function,
    Method,
    Value,
}
struct Decl {
    at: usize,
    kind: &'static str,
    role: Role,
    scope: usize,
    key: String,
    body: Option<Range<usize>>,
    signature: Option<Range<usize>>,
    bindable: bool,
    receiver: Option<usize>,
}

struct Syntax<'a> {
    lang: Language,
    t: Vec<Token<'a>>,
    pair: Vec<Option<usize>>,
    parent: Vec<usize>,
    types: HashMap<usize, usize>,
    in_parameters: Vec<bool>,
    namespace: Vec<usize>,
    namespace_blocks: HashSet<usize>,
    decls: Vec<Decl>,
    objc_type: Vec<usize>,
}

impl<'a> Syntax<'a> {
    fn new(lang: Language, source: &'a str) -> Self {
        let t = scan::scan(source, lang);
        let mut pair = vec![None; t.len()];
        let mut parent = Vec::with_capacity(t.len());
        let mut opens = Vec::new();
        let mut in_parameters = Vec::with_capacity(t.len());
        let mut brace = ROOT;
        for (i, tok) in t.iter().enumerate() {
            parent.push(brace);
            in_parameters.push(opens.last().is_some_and(|&(s, _)| matches!(s, "(" | "[")));
            match tok.text {
                "(" | "[" | "{" => {
                    opens.push((tok.text, i));
                    if tok.text == "{" {
                        brace = i;
                    }
                }
                ")" | "]" | "}" => {
                    if let Some(&(left, j)) = opens.last()
                        && matches!((left, tok.text), ("(", ")") | ("[", "]") | ("{", "}"))
                    {
                        opens.pop();
                        pair[i] = Some(j);
                        pair[j] = Some(i);
                        if left == "{" {
                            brace = parent[j];
                        }
                    }
                }
                _ => {}
            }
        }
        let objc_type = if lang == Language::ObjectiveC {
            vec![ROOT; t.len()]
        } else {
            Vec::new()
        };
        let mut namespace_blocks = HashSet::new();
        let mut namespace = Vec::new();
        if matches!(lang, Language::Php | Language::Hack) {
            let mut region = ROOT;
            let mut end = t.len();
            let mut regions = HashMap::new();
            for (i, tok) in t.iter().enumerate() {
                if i == end {
                    region = ROOT;
                }
                if tok.text.eq_ignore_ascii_case("namespace")
                    && parent[i] == ROOT
                    && t.get(i + 1)
                        .is_some_and(|v| v.kind == Kind::Word || v.text == "{")
                {
                    let mut j = i + 1;
                    while j < t.len() && (t[j].kind == Kind::Word || t[j].text == "\\") {
                        j += 1;
                    }
                    let mut name = t[i + 1..j].iter().map(|tok| tok.text).collect::<String>();
                    name.make_ascii_lowercase();
                    let scope = if name.is_empty() {
                        ROOT
                    } else {
                        *regions.entry(name).or_insert(i)
                    };
                    match t.get(j).map(|v| v.text) {
                        Some(";") => {
                            region = scope;
                            end = t.len();
                        }
                        Some("{") => {
                            region = scope;
                            end = pair[j].unwrap_or(t.len());
                            namespace_blocks.insert(j);
                        }
                        _ => {}
                    }
                }
                namespace.push(region);
            }
        }
        Self {
            lang,
            t,
            pair,
            parent,
            types: HashMap::new(),
            in_parameters,
            namespace,
            namespace_blocks,
            decls: vec![],
            objc_type,
        }
    }
    fn w(&self, i: usize) -> &'a str {
        self.t.get(i).map_or("", |t| t.text)
    }
    fn prev(&self, i: usize) -> &'a str {
        i.checked_sub(1).map_or("", |j| self.w(j))
    }
    fn id(&self, i: usize) -> bool {
        self.t.get(i).is_some_and(|t| {
            t.kind == Kind::Word && t.text.starts_with(|c: char| c == '_' || c.is_alphabetic())
        })
    }
    fn close(&self, i: usize) -> Option<usize> {
        self.pair.get(i).copied().flatten()
    }
    fn name(&self, i: usize) -> String {
        if matches!(self.lang, Language::Php | Language::Hack) {
            self.w(i).to_ascii_lowercase()
        } else {
            self.w(i).to_owned()
        }
    }
    fn add(&mut self, at: usize, kind: &'static str, role: Role, scope: usize) -> usize {
        let n = self.decls.len();
        self.decls.push(Decl {
            at,
            kind,
            role,
            scope,
            key: self.name(at),
            body: None,
            signature: None,
            bindable: true,
            receiver: None,
        });
        n
    }
    // Skip balanced subexpressions while looking for a declaration's delimiters.
    fn seek(&self, mut i: usize, stops: &[&str]) -> Option<usize> {
        while i < self.t.len() {
            let s = self.w(i);
            if stops.contains(&s) {
                return Some(i);
            }
            if matches!(s, ";" | "{" | "}") {
                return None;
            }
            if matches!(s, "(" | "[") {
                i = self.close(i)?;
            }
            i += 1;
        }
        None
    }
    fn finish(mut self, source: &str, score: Option<&mut super::super::score::Saved>) -> Analysis {
        self.extract();
        self.decls.sort_by_key(|d| d.at);
        self.decls.dedup_by_key(|d| d.at);
        let n = self.t.len();
        let mut owner = vec![None; n];
        let mut blocked = vec![false; n];
        let mut definitions = HashSet::new();
        let mut lookup: HashMap<(usize, &str), Vec<usize>> = HashMap::new();
        let mut out = Analysis::default();
        for (id, d) in self.decls.iter().enumerate() {
            out.declarations.push(Declaration {
                span: self.t[d.at].span.clone(),
                kind: d.kind,
            });
            definitions.insert(d.at);
            lookup
                .entry((self.lookup_scope(d.scope, d.at), &d.key))
                .or_default()
                .push(id);
            if let Some(r) = &d.signature {
                blocked[r.clone()].fill(true);
            }
            if let Some(r) = &d.body {
                owner[r.clone()].fill(Some(id));
            }
        }
        let shadows = self.shadow_bindings(&owner, &definitions, &mut blocked);
        let php = matches!(self.lang, Language::Php | Language::Hack);
        // Imports and namespaces can change bare lookup. This profile exposes
        // their written declarations and leaves ambiguous imported targets open.
        let unusual_lookup = self.t.iter().enumerate().any(|(i, t)| {
            php && t.text.eq_ignore_ascii_case("use")
                || self.lang == Language::Go
                    && t.text == "."
                    && self.t.get(i + 1).is_some_and(|t| t.kind == Kind::String)
        });
        for (i, t) in self.t.iter().enumerate() {
            if !self.id(i) || definitions.contains(&i) || blocked[i] || reserved(t.text) {
                continue;
            }
            let previous = self.prev(i);
            let type_ref = matches!(
                previous,
                "new" | "extends" | "implements" | "instanceof" | "is" | "as"
            );
            let message = if self.lang == Language::ObjectiveC {
                self.message(i)
            } else {
                None
            };
            let is_call = self.w(i + 1) == "(" || message.is_some();
            let value = !is_call && self.function_value(i);
            if !type_ref && !is_call && !value {
                continue;
            }
            if matches!(previous, "@" | "#") {
                continue;
            }
            if self.lang == Language::ObjectiveC && previous == "selector" {
                continue;
            }
            let caller = owner[i];
            let class = caller
                .map(|id| self.decls[id].scope)
                .or_else(|| self.kotlin_initializer_type(i))
                .unwrap_or(ROOT);
            // Parameters and the implementations invoked through them are distinct.
            if (is_call || value)
                && !matches!(previous, "." | ":" | ">")
                && message.is_none()
                && let Some(caller_id) = caller
                && let Some(ids) = lookup.get(&(self.decls[caller_id].at, t.text))
                && let [parameter] = ids.as_slice()
                && self.decls[*parameter].kind == "parameter"
                && !shadows.contains(&self.parent, i, t.text, None)
            {
                out.direct.push((out.calls.len(), vec![*parameter]));
                out.calls.push(Call {
                    is_call: false,
                    span: t.span.clone(),
                    owner: caller,
                });
                continue;
            }
            let mut super_call = false;
            let mut scope = class;
            let mut key = self.name(i);
            let mut safe = !unusual_lookup;
            let mut want = Role::Function;
            if let Some((ref selector, receiver)) = message {
                want = Role::Method;
                safe = receiver == "self"
                    && caller.is_some_and(|id| self.decls[id].role == Role::Method);
                let sign = caller
                    .map(|id| self.decls[id].key.get(..1).unwrap_or(""))
                    .unwrap_or("");
                key = format!("{sign}{selector}");
            } else if type_ref {
                scope = ROOT;
                want = Role::Type;
            } else if value && previous == ":" {
                if self.lang == Language::Java {
                    want = Role::Method;
                    safe = class != ROOT
                        && i >= 3
                        && self.prev(i - 3) != "."
                        && self.w(i - 3) == self.w(class);
                } else if class != ROOT && lookup.contains_key(&(class, key.as_str())) {
                    want = Role::Method;
                } else {
                    scope = ROOT;
                    safe &= class == ROOT;
                }
            } else if matches!(previous, "." | ">" | ":" | "\\") {
                want = Role::Method;
                safe = match previous {
                    "." if self.lang == Language::Go && i >= 2 => caller.is_some_and(|id| {
                        let d = &self.decls[id];
                        d.receiver.is_some_and(|r| self.w(i - 2) == self.w(r))
                            && self.prev(i - 2) != "."
                            && !shadows.contains(&self.parent, i, self.w(i - 2), None)
                    }),
                    "." if self.lang == Language::Java && i >= 2 && self.w(i - 2) == "super" => {
                        if let Some(parent) = self.java_parent(class, &lookup) {
                            scope = parent;
                            super_call = self.prev(i - 2) != ".";
                        }
                        super_call
                    }
                    "." => i >= 2 && self.w(i - 2) == "this" && self.prev(i - 2) != ".",
                    ">" => i >= 3 && self.w(i - 2) == "-" && self.w(i - 3) == "$this",
                    ":" => i >= 3 && self.w(i - 2) == ":" && self.w(i - 3) == "self",
                    _ => false,
                } && class != ROOT;
            } else if php || matches!(self.lang, Language::Go | Language::ObjectiveC) {
                scope = ROOT;
            } else if class != ROOT && lookup.contains_key(&(class, key.as_str())) {
                want = Role::Method;
            } else {
                if class != ROOT && matches!(self.lang, Language::Kotlin | Language::Dart) {
                    // An inherited or extension member can hide a top-level name.
                    safe = false;
                }
                scope = ROOT;
            }
            if want == Role::Function
                || (want == Role::Method
                    && message.is_none()
                    && !matches!(previous, "." | ">" | ":")
                    && matches!(self.lang, Language::Kotlin | Language::Dart))
            {
                safe &= !shadows.contains(&self.parent, i, t.text, caller);
            }
            if safe
                && let Some(ids) = lookup.get(&(self.lookup_scope(scope, i), key.as_str()))
                && let [id] = ids.as_slice()
                && self.decls[*id].role == want
                && (self.decls[*id].bindable || super_call)
                && (!super_call || self.java_super_matches(i, &self.decls[*id]))
            {
                out.direct.push((out.calls.len(), vec![*id]));
            } else if value {
                continue;
            }
            out.calls.push(Call {
                is_call,
                span: t.span.clone(),
                owner: caller,
            });
        }
        if let Some(dest) = score {
            *dest = super::super::score::with_tokens(self.lang, source, &self.t, &out);
        }
        out
    }
    fn lookup_scope(&self, scope: usize, at: usize) -> usize {
        // Namespace declarations and type declarations have distinct token IDs.
        if scope == ROOT {
            self.namespace.get(at).copied().unwrap_or(ROOT)
        } else {
            scope
        }
    }

    fn function_value(&self, i: usize) -> bool {
        let next = self.w(i + 1);
        let standalone = matches!(next, "" | ";" | "," | ")" | "}")
            || self.t.get(i + 1).is_some_and(|t| t.line > self.t[i].line);
        if !standalone {
            return false;
        }
        match self.lang {
            Language::Go | Language::Dart | Language::ObjectiveC => {
                matches!(self.prev(i), "=" | "(" | "," | "return" | ".")
                    || self.lang == Language::ObjectiveC && self.prev(i) == "&"
            }
            Language::Kotlin => {
                i >= 2
                    && self.w(i - 1) == ":"
                    && self.w(i - 2) == ":"
                    && matches!(self.prev(i - 2), "=" | "(" | "," | "return" | "{")
            }
            Language::Java => {
                i >= 3 && self.w(i - 1) == ":" && self.w(i - 2) == ":" && self.id(i - 3)
            }
            _ => false,
        }
    }

    fn kotlin_initializer_type(&self, i: usize) -> Option<usize> {
        if self.lang != Language::Kotlin {
            return None;
        }
        let mut scope = self.parent[i];
        while scope != ROOT {
            if self.prev(scope) == "init" {
                return self.types.get(&self.parent[scope]).copied();
            }
            scope = self.parent[scope];
        }
        None
    }

    fn java_super_matches(&self, call: usize, method: &Decl) -> bool {
        // A parent can inherit another overload. With arguments and an
        // uninspected ancestor, a same-name method in the parent is insufficient.
        if self.w(call + 2) != ")" {
            let after = if self.w(method.scope + 1) == "<" {
                self.angle_end(method.scope + 1).unwrap_or(method.scope) + 1
            } else {
                method.scope + 1
            };
            if self.w(after) != "{" {
                return false;
            }
        }
        let arity = |open| {
            let end = self.close(open)?;
            let mut count = usize::from(end > open + 1);
            let mut i = open + 1;
            while i < end {
                match self.w(i) {
                    "," => count += 1,
                    "(" | "[" | "{" => i = self.close(i)?,
                    "<" => i = self.angle_end(i)?,
                    _ => {}
                }
                i += 1;
            }
            Some(count)
        };
        method.body.is_some() && arity(call + 1).is_some_and(|n| arity(method.at + 1) == Some(n))
    }

    fn java_parent(
        &self,
        class: usize,
        lookup: &HashMap<(usize, &str), Vec<usize>>,
    ) -> Option<usize> {
        if class == ROOT {
            return None;
        }
        let mut i = class + 1;
        if self.w(i) == "<" {
            i = self.angle_end(i)? + 1;
        }
        if self.w(i) != "extends" || !self.id(i + 1) || self.w(i + 2) == "." {
            return None;
        }
        let scope = self
            .types
            .get(&self.parent[class])
            .copied()
            .unwrap_or(self.parent[class]);
        let [id] = lookup.get(&(scope, self.w(i + 1)))?.as_slice() else {
            return None;
        };
        (self.decls[*id].role == Role::Type).then_some(self.decls[*id].at)
    }

    fn message(&self, i: usize) -> Option<(String, &'a str)> {
        if i < 2 || self.w(i - 2) != "[" || !self.id(i - 1) {
            return None;
        }
        let end = self.close(i - 2)?;
        if self.w(i + 1) == "]" {
            return Some((self.name(i), self.w(i - 1)));
        }
        if self.w(i + 1) != ":" {
            return None;
        }
        let mut key = String::new();
        let mut j = i;
        while j < end {
            if self.id(j) && self.w(j + 1) == ":" {
                key.push_str(self.w(j));
                key.push(':');
            }
            if matches!(self.w(j), "(" | "[" | "{") {
                j = self.close(j)?;
            }
            j += 1;
        }
        Some((key, self.w(i - 1)))
    }
}
fn reserved(s: &str) -> bool {
    matches!(
        s,
        "if" | "else"
            | "for"
            | "foreach"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "catch"
            | "try"
            | "finally"
            | "return"
            | "throw"
            | "yield"
            | "new"
            | "super"
            | "this"
            | "self"
            | "class"
            | "interface"
            | "struct"
            | "record"
            | "type"
            | "func"
            | "fun"
            | "function"
            | "sizeof"
            | "typeof"
            | "synchronized"
            | "when"
            | "match"
            | "assert"
            | "with"
            | "using"
            | "defer"
            | "select"
            | "go"
            | "var"
            | "const"
            | "void"
            | "int"
            | "bool"
            | "string"
            | "double"
            | "float"
            | "long"
            | "short"
            | "char"
            | "boolean"
            | "byte"
            | "unsigned"
            | "signed"
            | "private"
            | "public"
            | "static"
            | "final"
            | "override"
            | "throws"
            | "async"
            | "await"
            | "echo"
            | "print"
            | "isset"
            | "empty"
            | "array"
            | "list"
            | "unset"
            | "require"
            | "include"
            | "decltype"
    )
}

pub(super) fn analyze(lang: Language, source: &str) -> Analysis {
    analyze_scored(lang, source, None)
}
pub(super) fn analyze_scored(
    lang: Language,
    source: &str,
    score: Option<&mut super::super::score::Saved>,
) -> Analysis {
    let lang = if lang == Language::Php && hack_header_len(source).is_some() {
        Language::Hack
    } else {
        lang
    };
    Syntax::new(lang, source).finish(source, score)
}

fn hack_header_len(source: &str) -> Option<usize> {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let rest = text.strip_prefix("<?hh")?;
    (rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_whitespace()))
        .then_some(source.len() - rest.len())
}
