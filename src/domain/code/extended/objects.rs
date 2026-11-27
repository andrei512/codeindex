//! File-local declarations and direct calls in brace-delimited languages.
//! A method key includes its declaring type. An Objective-C key additionally
//! includes the full selector and whether it is an instance or class method.
use super::super::{Analysis, Call, Kind, Language, Token};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ops::Range,
};

mod declarations;
mod java;
mod languages;
mod namespaces;
mod owners;
mod references;
mod scan;
pub(in crate::domain::code) use scan::tokens_with_comments;
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
// A present key still blocks outer lookup when several declarations share it.
// Only an unambiguous name needs to retain its declaration ID.
type MemberNames<'a> = HashMap<(usize, &'a str), Option<usize>>;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Binding {
    Dynamic,
    Direct,
    PublicStatic,
    ScopedStatic,
}

impl Binding {
    fn direct(known: bool) -> Self {
        if known { Self::Direct } else { Self::Dynamic }
    }
}

struct Decl<'a> {
    at: usize,
    kind: &'static str,
    role: Role,
    scope: usize,
    key: Cow<'a, str>,
    body: Option<Range<usize>>,
    signature: Option<Range<usize>>,
    binding: Binding,
    receiver: Option<usize>,
}

struct Syntax<'a> {
    lang: Language,
    t: Vec<Token<'a>>,
    pair: Vec<usize>,
    parent: Vec<usize>,
    types: HashMap<usize, usize>,
    in_parameters: Vec<bool>,
    namespaces: namespaces::Namespaces,
    unusual_lookup: bool,
    decls: Vec<Decl<'a>>,
    objc_type: Vec<usize>,
}

struct Groups {
    pair: Vec<usize>,
    parent: Vec<usize>,
    in_parameters: Vec<bool>,
    opens: Vec<usize>,
    brace: usize,
}

impl Groups {
    fn new(size: usize) -> Self {
        Self {
            pair: vec![size; size],
            parent: Vec::with_capacity(size),
            in_parameters: Vec::with_capacity(size),
            opens: Vec::new(),
            brace: ROOT,
        }
    }
    fn observe(&mut self, tokens: &[Token], i: usize) {
        let tok = &tokens[i];
        self.parent.push(self.brace);
        self.in_parameters
            .push(self.opens.last().is_some_and(|&j| j != self.brace));
        match tok.text {
            "(" | "[" | "{" => {
                self.opens.push(i);
                if tok.text == "{" {
                    self.brace = i;
                }
            }
            ")" | "]" | "}" => self.close(tokens, i),
            _ => {}
        }
    }
    fn close(&mut self, tokens: &[Token], close: usize) {
        let Some(open) = self.opens.pop_if(|&mut open| {
            matches!(
                (tokens[open].text, tokens[close].text),
                ("(", ")") | ("[", "]") | ("{", "}")
            )
        }) else {
            return;
        };
        self.pair[close] = open;
        self.pair[open] = close;
        if tokens[open].text == "{" {
            self.brace = self.parent[open];
        }
    }
}

#[derive(Default)]
struct FileNotes {
    unusual_lookup: bool,
    namespace_starts: Vec<usize>,
}

impl FileNotes {
    fn observe(&mut self, lang: Language, tokens: &[Token], at: usize, brace: usize) {
        let written = tokens[at].text;
        match lang {
            Language::Go if written == "." => {
                self.unusual_lookup |= tokens
                    .get(at + 1)
                    .is_some_and(|token| token.kind == Kind::String);
            }
            Language::Php | Language::Hack => self.php_word(written, at, brace),
            _ => {}
        }
    }

    fn php_word(&mut self, word: &str, at: usize, brace: usize) {
        if brace == ROOT && word.eq_ignore_ascii_case("namespace") {
            self.namespace_starts.push(at);
        }
        self.unusual_lookup |= word.eq_ignore_ascii_case("use");
    }
}

impl<'a> Syntax<'a> {
    fn new(lang: Language, source: &'a str) -> Self {
        let t = scan::scan(source, lang);
        let mut groups = Groups::new(t.len());
        let mut notes = FileNotes::default();
        for i in 0..t.len() {
            notes.observe(lang, &t, i, groups.brace);
            groups.observe(&t, i);
        }
        let Groups {
            pair,
            parent,
            in_parameters,
            ..
        } = groups;
        let objc_type = if lang == Language::ObjectiveC {
            vec![ROOT; t.len()]
        } else {
            Vec::new()
        };
        let namespaces = namespaces::Namespaces::new(&t, &pair, notes.namespace_starts);
        Self {
            lang,
            t,
            pair,
            parent,
            types: HashMap::new(),
            in_parameters,
            namespaces,
            unusual_lookup: notes.unusual_lookup,
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
    fn callable_name(&self, i: usize) -> bool {
        self.id(i) && !reserved(self.lang, self.w(i))
    }
    fn id(&self, i: usize) -> bool {
        self.t.get(i).is_some_and(|t| {
            t.kind == Kind::Word && t.text.starts_with(|c: char| c == '_' || c.is_alphabetic())
        })
    }
    fn close(&self, i: usize) -> Option<usize> {
        self.pair
            .get(i)
            .copied()
            .filter(|&paired| paired != self.t.len())
    }
    fn name(&self, i: usize) -> Cow<'a, str> {
        let text = self.w(i);
        if matches!(self.lang, Language::Php | Language::Hack)
            && text.bytes().any(|b| b.is_ascii_uppercase())
        {
            Cow::Owned(text.to_ascii_lowercase())
        } else {
            Cow::Borrowed(text)
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
            binding: Binding::Direct,
            receiver: None,
        });
        n
    }
    // Skip balanced subexpressions while looking for a declaration's delimiters.
    fn seek(&self, i: usize, stops: &[&str]) -> Option<usize> {
        self.seek_if(i, |word| stops.contains(&word))
    }
    fn seek_if(&self, mut i: usize, stop: impl Fn(&str) -> bool) -> Option<usize> {
        while i < self.t.len() {
            let s = self.w(i);
            if stop(s) {
                return Some(i);
            }
            i = self.next_seek_token(i, s)?;
        }
        None
    }
    fn next_seek_token(&self, at: usize, word: &str) -> Option<usize> {
        match word {
            ";" | "{" | "}" => None,
            "(" | "[" => self.close(at).map(|close| close + 1),
            _ => Some(at + 1),
        }
    }

    fn finish(mut self, source: &str, score: Option<&mut super::super::score::Saved>) -> Analysis {
        let mut out = Analysis::default();
        references::analyze(&mut self, &mut out);
        if let Some(dest) = score {
            *dest = super::super::score::with_pairs(
                self.lang,
                source,
                super::super::score::PairedSyntax {
                    tokens: &self.t,
                    pairs: &self.pair,
                },
                &out,
            );
        }
        out
    }
    fn lookup_scope(&self, scope: usize, at: usize) -> usize {
        // Namespace declarations and type declarations have distinct token IDs.
        if scope == ROOT {
            self.namespaces.at(at)
        } else {
            scope
        }
    }

    fn value_ends_at(&self, i: usize) -> bool {
        matches!(self.w(i + 1), "" | ";" | "," | ")" | "}")
            || self.t.get(i + 1).is_some_and(|t| t.line > self.t[i].line)
    }

    fn kotlin_initializer_type(&self, i: usize) -> Option<usize> {
        let mut scope = self.parent[i];
        while scope != ROOT {
            if self.prev(scope) == "init" {
                return self.types.get(&self.parent[scope]).copied();
            }
            scope = self.parent[scope];
        }
        None
    }

    /// Successfully pairing the outer brackets also validates nested argument groups.
    fn message_end(&self, i: usize) -> Option<usize> {
        let open = i.checked_sub(2)?;
        match (self.w(open), self.id(open + 1), self.w(i + 1)) {
            ("[", true, "]" | ":") => self.close(open),
            _ => None,
        }
    }

    fn message_key(&self, i: usize, end: usize, prefix: &str) -> String {
        let mut key = prefix.to_owned();
        if self.w(i + 1) == "]" {
            key.push_str(&self.name(i));
            return key;
        }
        let mut j = i;
        while j < end {
            if self.id(j) && self.w(j + 1) == ":" {
                key.push_str(self.w(j));
                key.push(':');
            }
            j = self.after_selector_component(j);
        }
        key
    }
    fn after_selector_component(&self, at: usize) -> usize {
        match self.w(at) {
            "(" | "[" | "{" => self.pair[at] + 1,
            _ => at + 1,
        }
    }
}
// Keep this list in lexical order for admission without allocation.
const RESERVED: &[&str] = &[
    "array",
    "assert",
    "async",
    "await",
    "bool",
    "boolean",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "decltype",
    "defer",
    "do",
    "double",
    "echo",
    "else",
    "empty",
    "final",
    "finally",
    "float",
    "for",
    "foreach",
    "fun",
    "func",
    "function",
    "go",
    "if",
    "include",
    "int",
    "interface",
    "isset",
    "list",
    "long",
    "match",
    "new",
    "override",
    "print",
    "private",
    "public",
    "record",
    "require",
    "return",
    "select",
    "self",
    "short",
    "signed",
    "sizeof",
    "static",
    "string",
    "struct",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "try",
    "type",
    "typeof",
    "unset",
    "unsigned",
    "using",
    "var",
    "void",
    "when",
    "while",
    "with",
    "yield",
];

fn reserved(lang: Language, s: &str) -> bool {
    // Bare Objective-C callables follow C identifier grammar. @-directives
    // and message selectors are recognized at their written source sites.
    if lang == Language::ObjectiveC {
        return crate::domain::code::cfamily::c_keyword(s) || matches!(s, "self" | "super");
    }
    RESERVED.binary_search(&s).is_ok()
}

pub(in crate::domain::code) fn analyze(lang: Language, source: &str) -> Analysis {
    analyze_scored(lang, source, None)
}
pub(in crate::domain::code) fn analyze_scored(
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
