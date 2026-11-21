//! Written declarations and conservative navigation for direct, named calls.
//! This lexical profile has partial coverage. Matching names in unrelated files
//! are never added to the target set of a call.
mod cache;
mod cfamily;
mod direct;
mod lexer;
mod markup;
mod rust;
mod script;
use lexer::{Kind, Token};
use std::{collections::HashMap, ops::Range, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    Js,
    Python,
    Starlark,
    Shell,
    Powershell,
    Csharp,
    Swift,
    C,
    Cpp,
    Proto,
    Css,
    Html,
    Sql,
}

impl Language {
    pub fn for_path(path: &str) -> Option<Self> {
        let p = Path::new(path);
        Some(match p.extension().or_else(|| p.file_name())?.to_str()? {
            "rs" => Self::Rust,
            "ts" | "tsx" | "cts" | "mts" | "js" | "jsx" | "mjs" | "cjs" => Self::Js,
            "py" | "pyi" => Self::Python,
            "bzl" | "bazel" | "BUILD" | "WORKSPACE" => Self::Starlark,
            "sh" | "bash" => Self::Shell,
            "ps1" | "psm1" => Self::Powershell,
            "cs" => Self::Csharp,
            "swift" => Self::Swift,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "hpp" | "cxx" => Self::Cpp,
            "proto" => Self::Proto,
            "css" => Self::Css,
            "html" | "htm" => Self::Html,
            "sql" => Self::Sql,
            _ => return None,
        })
    }
    fn line_comment(self, t: &str) -> bool {
        match self {
            Self::Python | Self::Starlark | Self::Shell | Self::Powershell => t.starts_with('#'),
            Self::Sql => t.starts_with("--"),
            Self::Css | Self::Html => false,
            _ => t.starts_with("//"),
        }
    }
    fn block_comments(self) -> bool {
        matches!(
            self,
            Self::Js
                | Self::Rust
                | Self::Csharp
                | Self::Swift
                | Self::C
                | Self::Cpp
                | Self::Proto
                | Self::Css
                | Self::Sql
        )
    }

    fn decl(self, kw: &str) -> Option<&'static str> {
        Some(match (self, kw) {
            (Self::Rust, "fn") => "function_item",
            (Self::Rust, "struct") => "struct_item",
            (Self::Rust, "enum") => "enum_item",
            (Self::Rust, "trait") => "trait_item",
            (Self::Rust, "mod") => "mod_item",
            (Self::Rust, "const") => "const_item",
            (Self::Rust, "type") => "type_item",
            (Self::Rust, "static") => "static_item",
            (Self::Js, "function") => "function_declaration",
            (Self::Js, "class") => "class_declaration",
            (Self::Js, "const" | "let" | "var") => "variable_declarator",
            (Self::Js, "interface") => "interface_declaration",
            (Self::Js, "type") => "type_alias_declaration",
            (Self::Js, "enum") => "enum_declaration",
            (Self::Python | Self::Starlark, "def") => "function_definition",
            (Self::Python, "class") => "class_definition",
            (Self::Swift, "func") => "function_declaration",
            (Self::Swift, "class" | "struct" | "enum") => "class_declaration",
            (Self::Csharp, "class") => "class_declaration",
            (Self::Csharp, "interface") => "interface_declaration",
            (Self::Csharp, "struct") => "struct_declaration",
            (Self::Csharp, "enum") => "enum_declaration",
            (Self::Cpp, "class") => "class_specifier",
            (Self::C | Self::Cpp, "struct") => "struct_specifier",
            (Self::C | Self::Cpp, "enum") => "enum_specifier",
            (Self::Proto, "message") => "message_name",
            (Self::Proto, "service") => "service_name",
            (Self::Proto, "rpc") => "rpc_name",
            (Self::Proto, "enum") => "enum_name",
            (Self::Powershell, "function") => "function_name",
            (Self::Shell, "function") => "function_definition",
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub struct Declaration {
    pub span: Range<usize>,
    pub kind: &'static str,
}
#[derive(Debug)]
pub struct Call {
    pub is_call: bool,
    pub span: Range<usize>,
    pub owner: Option<usize>,
}
#[derive(Default)]
pub struct Analysis {
    direct: Vec<(usize, Vec<usize>)>,
    direct_evidence: Option<Box<direct::Evidence>>,
    rust: Option<rust::Evidence>,
    pub declarations: Vec<Declaration>,
    pub calls: Vec<Call>,
    script: Option<script::Evidence>,
}

fn indentation(text: &str, start: usize) -> usize {
    let begin = text[..start].rfind('\n').map_or(0, |i| i + 1);
    text[begin..]
        .bytes()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count()
}

pub fn analyze(lang: Language, text: &str) -> Analysis {
    if lang == Language::Html {
        return markup::html(text);
    }
    if lang == Language::Shell {
        return direct::analyze_shell(text);
    }
    let t = lexer::tokens(text, lang);
    if lang == Language::Proto {
        return direct::analyze_proto(&t);
    }
    if matches!(
        lang,
        Language::C | Language::Cpp | Language::Csharp | Language::Swift
    ) {
        return cfamily::analyze(lang, &t);
    }
    if lang == Language::Css {
        return markup::css(&t);
    }
    if lang == Language::Sql {
        return markup::sql(&t);
    }
    if lang == Language::Rust {
        return rust::analyze(&t, text.len());
    }
    if matches!(lang, Language::Python | Language::Js) {
        return script::analyze(lang, text, &t);
    }
    let mut a = Analysis::default();
    let mut brace_depth = 0usize;
    let mut pending = None;
    let mut scopes: Vec<(usize, usize)> = Vec::new();
    let mut py_scopes: Vec<(usize, usize)> = Vec::new();
    let offside = matches!(lang, Language::Python | Language::Starlark);
    for (i, tok) in t.iter().enumerate() {
        let prev = i.checked_sub(1).map_or("", |j| t[j].text);
        let next = t.get(i + 1).map_or("", |x| x.text);
        if offside && (i == 0 || t[i - 1].line != tok.line) {
            let indent = indentation(text, tok.span.start);
            while py_scopes.last().is_some_and(|&(v, _)| v >= indent) {
                py_scopes.pop();
            }
        }
        if tok.text == "{" {
            brace_depth += 1;
            if let Some(id) = pending.take() {
                scopes.push((brace_depth, id));
            }
        }
        if tok.text == "}" {
            if scopes.last().is_some_and(|s| s.0 == brace_depth) {
                scopes.pop();
            }
            brace_depth = brace_depth.saturating_sub(1);
            pending = None;
        }
        if tok.text == ";" {
            pending = None;
        }
        let span = tok.span.clone();
        let kind = if tok.kind == Kind::Word {
            lang.decl(prev).or_else(|| match lang {
                Language::Shell
                    if next == "("
                        && t.get(i + 2).is_some_and(|x| x.text == ")")
                        && t.get(i + 3).is_some_and(|x| x.text == "{") =>
                {
                    Some("function_definition")
                }
                Language::C | Language::Cpp
                    if next == "("
                        && matches!(
                            prev,
                            "int" | "long" | "float" | "double" | "char" | "void" | "bool"
                        ) =>
                {
                    Some("function_definition")
                }
                _ => None,
            })
        } else {
            None
        };
        if let Some(kind) = kind {
            let id = a.declarations.len();
            a.declarations.push(Declaration { span, kind });
            if kind.starts_with("function") {
                if offside {
                    py_scopes.push((indentation(text, tok.span.start), id));
                } else {
                    pending = Some(id);
                }
            }
        } else if tok.kind == Kind::Word
            && next == "("
            && !matches!(prev, "." | ":" | "#")
            && !matches!(
                tok.text,
                "if" | "for"
                    | "while"
                    | "match"
                    | "switch"
                    | "catch"
                    | "return"
                    | "sizeof"
                    | "function"
                    | "fn"
            )
        {
            let owner = if offside {
                py_scopes.last().map(|s| s.1)
            } else {
                scopes.last().map(|s| s.1)
            };
            a.calls.push(Call {
                span,
                owner,
                is_call: true,
            });
        }
    }

    a
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct Site {
    pub file: usize,
    pub local: usize,
}
#[derive(Default)]
pub struct Names {
    pub public_labels: HashMap<Site, String>,
    pub by_name: HashMap<String, Vec<Site>>,
    pub incoming: HashMap<Site, Vec<Site>>,
    pub targets: HashMap<Site, Vec<Site>>,
}

pub struct Unit<'a> {
    pub path: &'a str,
    pub text: &'a str,
    pub analysis: &'a Analysis,
}
impl Declaration {
    pub fn searchable(&self) -> bool {
        !matches!(
            self.kind,
            "embedded_declaration"
                | "css_variable"
                | "shell_assignment"
                | "parameter"
                | "function_overload"
                | "function_expression"
                | "local_binding"
                | "source_file"
                | "field_declaration"
                | "enum_variant"
                | "generic_parameter"
                | "lifetime_parameter"
                | "associated_type"
        )
    }
}

impl Names {
    /// With identical path membership, these are the units whose bytes can
    /// affect binding or exported labels. All other units contribute only paths.
    pub(crate) fn consumes_text(path: &str) -> bool {
        let p = Path::new(path);
        Language::for_path(path).is_some() || path == "pyproject.toml" ||
            match p.file_name().and_then(|name| name.to_str()) {
                Some("Cargo.toml" | "package.json" | "tsconfig.json") => true,
                Some("config" | "config.toml") => p.parent().and_then(Path::file_name)
                    .is_some_and(|name| name == ".cargo"),
                _ => false,
            }
    }

    fn bind(&mut self, site: Site, mut targets: Vec<Site>) {
        if targets.is_empty() {
            return;
        }
        targets.sort_unstable();
        targets.dedup();
        match self.targets.entry(site) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(targets);
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let previous = e.get_mut();
                previous.extend(targets);
                previous.sort_unstable();
                previous.dedup();
            }
        }
    }

    pub fn build(units: &[Unit<'_>]) -> Self {
        let mut names = Self::default();
        let paths: HashMap<_, _> = units.iter().enumerate().map(|(i, u)| (u.path, i)).collect();
        for (file, u) in units.iter().enumerate() {
            for (local, d) in u.analysis.declarations.iter().enumerate() {
                if !d.searchable() {
                    continue;
                }
                let name = &u.text[d.span.clone()];
                let site = Site { file, local };
                if let Some(sites) = names.by_name.get_mut(name) {
                    sites.push(site);
                } else {
                    names.by_name.insert(name.to_owned(), vec![site]);
                }
            }
        }
        direct::resolve(units, &paths, &mut names);
        rust::resolve(units, &paths, &mut names);
        script::resolve(units, &paths, &mut names);
        script::public_labels(units, &mut names.public_labels);
        for (file, u) in units.iter().enumerate() {
            let Some(lang) = Language::for_path(u.path) else {
                continue;
            };
            for (local, targets) in &u.analysis.direct {
                names.bind(
                    Site {
                        file,
                        local: *local,
                    },
                    targets.iter().map(|&local| Site { file, local }).collect(),
                );
            }
            if matches!(
                lang,
                Language::Rust
                    | Language::Python
                    | Language::Js
                    | Language::Css
                    | Language::Html
                    | Language::Shell
                    | Language::Proto
                    | Language::C
                    | Language::Cpp
                    | Language::Csharp
                    | Language::Swift
            ) {
                continue;
            }
            for (local, c) in u.analysis.calls.iter().enumerate() {
                let call = Site { file, local };
                let key = &u.text[c.span.clone()];
                let targets: Vec<_> = names
                    .by_name
                    .get(key)
                    .into_iter()
                    .flatten()
                    .filter(|s| s.file == file)
                    .copied()
                    .collect();
                names.bind(call, targets);
            }
        }
        // Derive the reverse directory from the complete forward relation.
        // Site order is canonical across worker orders and randomized hash seeds.
        for (file, u) in units.iter().enumerate() {
            for local in 0..u.analysis.calls.len() {
                let site = Site { file, local };
                if let Some(targets) = names.targets.get(&site) {
                    for &target in targets {
                        names.incoming.entry(target).or_default().push(site);
                    }
                }
            }
        }
        names
    }
}

crate::domain::code::cache::record!(Analysis { direct, direct_evidence, rust, declarations, calls, script });

crate::domain::code::cache::record!(Call { is_call, span, owner });

crate::domain::code::cache::record!(Declaration { span, kind });

impl Analysis{
    pub(crate) fn same_binding_text(&self, lang: Language, old: &str, new: &str) -> bool {
        matches!(lang, Language::Python | Language::Js) && script::same_binding_text(self, lang, old, new)
    }

pub(crate) fn encode(&self,w:&mut Vec<u8>){ use cache::Wire;self.save(w); }
pub(crate) fn decode(r:&mut &[u8])->std::io::Result<Self>{use cache::Wire; Self::load(r)}
}
