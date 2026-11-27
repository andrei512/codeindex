//! Written declarations and lexically provable uses for C-family source files.
//! A scope indexes distinct type/tag, value and namespace bindings. A use follows
//! its lexical parents or an explicit receiver. Unproved dispatch remains unresolved.
pub(in crate::domain::code) mod analyzer;
pub(super) fn c_keyword(word: &str) -> bool {
    declarations::Grammar::select(Language::C).reserved(word)
}

use super::{Analysis, Call, Declaration, Kind, Language, Token};
use std::collections::HashMap;
mod binding_ids;
mod condition;
pub(super) mod directives;
mod functions;
mod number;
mod overloads;
mod owner_macro;
mod parameters;
mod pending_uses;
mod physical_lines;
pub(crate) mod projection;
mod receiver;
mod scopes;
pub(in crate::domain::code) mod translation;
mod type_prefix;
pub(super) mod written_body;
use binding_ids::{Names, Tables};
mod declarations;
mod standard;
use declarations::{ContainerKind, Dialect};
pub(super) mod csharp;
pub(crate) mod project;
pub(crate) mod sdk;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Type,
    Namespace,
    Value,
    Function,
}
enum TypeProof<'a> {
    Known(&'a str),
    Written,
    Incomplete,
}
struct DeclaredType<'a> {
    name: &'a str,
    proof: TypeProof<'a>,
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
    names: Names<'a>,
}
struct Analyzer<'a, 't> {
    grammar: &'static declarations::Grammar,
    digit_separators: bool,
    t: &'t [Token<'a>],
    out: Analysis,
    scopes: Vec<Scope<'a>>,
    bindings: Vec<Binding<'a>>,
    tables: Tables<'a>,
    external_owners: HashMap<usize, HashMap<std::borrow::Cow<'a, str>, usize>>,
    pending: pending_uses::PendingUses,
    pairs: Vec<usize>,
    angles: type_prefix::Angles,
    body: HashMap<usize, usize>,
    skip: Vec<bool>,
    declared: Vec<bool>,
    macro_owners: HashMap<usize, String>,
    csharp_usings: std::cell::OnceCell<csharp::Usings<'a>>,
}

#[derive(Clone, Copy)]
enum DeclarationEvent {
    Open,
    Close,
    Boundary,
    Name,
    Ignore,
}

/// Missing delimiter pairs carry the token count as their sentinel.
fn opening(pairs: &[usize], close: usize) -> Option<usize> {
    let open = pairs[close];
    (open < close).then_some(open)
}

fn unique<T>(mut items: impl Iterator<Item = T>) -> Option<T> {
    let first = items.next()?;
    items.next().is_none().then_some(first)
}

fn identifier_start(text: &str) -> bool {
    text.starts_with(|c: char| c == '_' || c.is_alphabetic())
}

fn ident(t: &Token) -> bool {
    t.kind == Kind::Word && identifier_start(t.text)
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
        let pairs = super::script::paired_for_score(t);
        Self {
            digit_separators: false,
            grammar: declarations::Grammar::select(lang),
            t,
            out: Analysis::default(),
            scopes: vec![Scope::default()],
            bindings: Vec::new(),
            tables: Tables::default(),
            external_owners: HashMap::new(),
            pending: Default::default(),
            pairs,
            angles: Default::default(),
            body: HashMap::new(),
            skip: vec![false; n],
            declared: vec![false; n],
            macro_owners: HashMap::new(),
            csharp_usings: Default::default(),
        }
    }
    fn delimit(&self, start: usize, tokens: &[&str]) -> usize {
        let mut i = start;
        while i < self.t.len() {
            let w = self.word(i);
            if tokens.contains(&w) {
                break;
            }
            i = self.delimiter_group_end(i, w) + 1;
        }
        i
    }
    fn delimiter_group_end(&self, open: usize, spelling: &str) -> usize {
        let close = self.pairs[open];
        if matches!(spelling, "(" | "[") && close < self.t.len() {
            close
        } else {
            open
        }
    }

    fn declared_type(&mut self, i: usize, scope: usize) -> Option<DeclaredType<'a>> {
        let (j, indirect) = type_prefix::Prefix::Declared {
            cv: self.grammar.c_qualifiers(),
        }
        .before(self.t, &self.pairs, i)?;
        let prefix = self.t.get(j)?;
        if prefix.text == ">" && self.grammar.generic_type_suffix {
            let name = self.angles.opening(self.t, j)?.checked_sub(1)?;
            return ident(&self.t[name]).then(|| DeclaredType {
                name: self.word(name),
                proof: TypeProof::Written,
            });
        }
        let ty = prefix.text;
        if prefix.kind != Kind::Word {
            return None;
        }
        if primitive(ty) {
            return Some(DeclaredType {
                name: ty,
                proof: TypeProof::Known(ty),
            });
        }
        let tag = self.lookup(scope, ty, Some(Role::Type), i).next();
        let proof = TypeProof::named(
            self.grammar,
            ty,
            declarations::TagEvidence {
                keyword: matches!(self.prev(j), "struct" | "class" | "union" | "enum"),
                binding: tag,
                native_pointer: indirect,
            },
        )?;
        Some(DeclaredType { name: ty, proof })
    }
    /// Reopened and qualified namespaces share their existing lexical scopes.
    fn namespace(&mut self, mut name: usize, mut parent: usize) {
        loop {
            let child = self.namespace_scope(parent, name);
            let Some(next) = self.namespace_suffix(name, child) else {
                return;
            };
            parent = child;
            name = next;
        }
    }
    fn namespace_suffix(&mut self, name: usize, child: usize) -> Option<usize> {
        let next = match self.word(name + 1) {
            ";" | "{" => {
                self.body.insert(name + 1, child);
                return None;
            }
            "." => name + 2,
            ":" if self.word(name + 2) == ":" => name + 3,
            _ => return None,
        };
        self.t.get(next).filter(|token| ident(token)).map(|_| next)
    }
    fn container<D: Dialect>(&mut self, i: usize, scope: usize, prev: &str) -> bool {
        match D::container(prev) {
            Some(ContainerKind::Namespace) => {
                self.namespace(i, scope);
                true
            }
            Some(container) => self.bind_container(i, scope, container),
            None => false,
        }
    }

    fn bind_container(&mut self, i: usize, scope: usize, container: ContainerKind) -> bool {
        let existing = self
            .lookup(scope, self.word(i), Some(Role::Type), i)
            .next()
            .and_then(|b| b.body);
        if matches!(container, ContainerKind::Extension) && existing.is_none() {
            return true;
        }
        match (container, self.type_body(i), existing) {
            (ContainerKind::Extension, Some(open), Some(scope)) => {
                self.body.insert(open, scope);
                self.declared[i] = true;
            }
            (ContainerKind::Type(kind), Some(open), scope_body) => {
                let child = scope_body.unwrap_or_else(|| self.container_scope(scope));
                self.body.insert(open, child);
                self.add(i, scope, kind, Role::Type).body = Some(child);
            }
            (ContainerKind::Type(kind), None, None) => {
                self.add(i, scope, kind, Role::Type);
            }
            _ => return false,
        }
        true
    }

    fn type_body(&mut self, name: usize) -> Option<usize> {
        let open = match self.word(name + 1) {
            "final" if self.grammar.cpp() && matches!(self.word(name + 2), "{" | ":") => {
                self.skip[name + 1] = true;
                self.delimit(name + 2, &["{", ";", "}"])
            }
            ":" | "<" | "where" => self.delimit(name + 2, &["{", ";", "}"]),
            _ => name + 1,
        };
        (self.word(open) == "{").then_some(open)
    }

    fn has_written_return(&self, name: usize) -> bool {
        let Some((mut j, _)) = type_prefix::Prefix::WrittenReturn.before(self.t, &self.pairs, name)
        else {
            return false;
        };
        if self.word(j) == ")" {
            // Function-like annotations often wrap the return type.
            let Some(annotation) = opening(&self.pairs, j).and_then(|open| open.checked_sub(1))
            else {
                return false;
            };
            j = annotation;
        }
        ident(&self.t[j]) && !self.grammar.reserved(self.word(j))
    }
    fn extract_names(&mut self) {
        (self.grammar.extract)(self);
    }

    fn extract_dialect<D: Dialect>(&mut self, dialect: D) {
        self.scan_declarations(dialect);
        self.uses::<D>();
    }
    fn scan_declarations<D: Dialect>(&mut self, mut dialect: D) {
        let mut scope = 0;
        let mut resume = vec![];
        for i in 0..self.t.len() {
            let spelling = self.word(i);
            dialect.observe(spelling);
            match self.declaration_event(i, spelling) {
                DeclarationEvent::Open => {
                    resume.push(scope);
                    scope = self.brace_scope(scope, i, self.prev(i));
                }
                DeclarationEvent::Close => scope = resume.pop().unwrap_or(0),
                DeclarationEvent::Boundary => scope = self.body.remove(&i).unwrap_or(scope),
                DeclarationEvent::Name => self.index_name(i, scope, &dialect),
                DeclarationEvent::Ignore => {}
            }
        }
    }

    fn declaration_event(&self, i: usize, spelling: &str) -> DeclarationEvent {
        if self.skip[i] {
            return DeclarationEvent::Ignore;
        }
        const BOUNDARIES: &[(&str, DeclarationEvent)] = &[
            ("{", DeclarationEvent::Open),
            ("}", DeclarationEvent::Close),
            (";", DeclarationEvent::Boundary),
        ];
        if let Some((_, event)) = BOUNDARIES.iter().find(|(word, _)| *word == spelling) {
            return *event;
        }
        if !self.declared[i] && ident(&self.t[i]) {
            return DeclarationEvent::Name;
        }
        DeclarationEvent::Ignore
    }

    fn index_name<D: Dialect>(&mut self, i: usize, scope: usize, dialect: &D) {
        let word = self.word(i);
        let prev = self.prev(i);
        if D::syntax_word(self, i) {
            return;
        }
        let reserved = self.grammar.reserved(word);
        if self.admits_declaration::<D>(i, scope, prev, reserved) {
            let next = self.word(i + 1);
            if next != "(" || !self.function::<D>(i, scope) {
                dialect.value(self, i, scope, next);
            }
        }
        if !self.declared[i] && !reserved {
            self.pending.push(i, scope);
        }
    }

    fn admits_declaration<D: Dialect>(
        &mut self,
        i: usize,
        scope: usize,
        prev: &str,
        reserved: bool,
    ) -> bool {
        let member = matches!(
            self.access(i, prev),
            Some(receiver::Access::Dot | receiver::Access::Arrow)
        );
        !member
            && !self.container::<D>(i, scope, prev)
            && (!reserved || D::special_name(prev, self.word(i)))
    }

    fn arg_type(&self, scope: usize, lo: usize, hi: usize) -> Option<&'a str> {
        let start = if self.grammar.argument_labels && self.word(lo + 1) == ":" {
            lo + 2
        } else {
            lo
        };
        let args = self.t.get(start..hi)?;
        if let Some(ty) = self.grammar.literal_type(args, self.digit_separators) {
            return Some(ty);
        }
        let [token] = args else {
            return None;
        };
        if !ident(token) {
            return None;
        }
        self.lookup(scope, token.text, Some(Role::Value), start)
            .next()?
            .ty
    }
    fn record_site(&mut self, i: usize, s: usize, site: overloads::SiteResolution) {
        if let Some(pending) = site.pending {
            csharp::record(&mut self.out, pending);
        }
        let span = self.t[i].span.clone();
        let owner = self.scopes[s].owner;
        if let Some(target) = site.target {
            self.out.direct_edge(target, span, owner, site.is_call);
        } else {
            self.out.calls.push(Call {
                span,
                owner,
                is_call: site.is_call,
            });
        }
    }

    fn use_candidates(
        &self,
        token: usize,
        scope: usize,
        prefix: &mut Option<receiver::Prefix>,
    ) -> std::iter::Peekable<scopes::Candidates<'_, 'a>> {
        let name = self.word(token);
        match self.chain_receiver(token, scope, prefix) {
            Some(owner) => self.members(owner, name),
            None => self.lookup(scope, name, None, token),
        }
    }

    fn uses<D: Dialect>(&mut self) {
        let mut prefix = None;
        let mut args = overloads::Arguments::default();
        for (i, s) in std::mem::take(&mut self.pending).scopes() {
            let call = self.word(i + 1) == "(";
            let candidates = self.use_candidates(i, s, &mut prefix);
            let request = overloads::UseRequest { token: i, scope: s, call };
            let resolution = self.resolve_use::<D>(request, candidates, &mut args);
            let Some(site) = resolution else { continue };
            self.record_site(i, s, site);
        }
    }
}
pub(super) fn analyze(lang: Language, text: &str, t: &[Token]) -> Analysis {
    analyze_with_pairs(lang, text, t).0
}

pub(super) fn analyze_with_pairs(
    lang: Language,
    text: &str,
    t: &[Token],
) -> (Analysis, Vec<usize>) {
    let mut a = Analyzer::new(lang, t);
    a.directives(text, lang);
    a.extract_names();
    (a.out, a.pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn targets(lang: Language, text: &str, anchor: &str, suffix: &str) -> Vec<usize> {
        let t = super::super::lexer::tokens(text, lang);
        let a = analyze(lang, text, &t);
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
    fn conditional_defined_alternatives_select_only_the_first_true_arm() {
        let text = "#define ON 1\n#undef OFF\n#if 0\n#elifdef ON\nint yes(){}\n#else\nint no(){}\n#endif\n#if 0\n#elifndef OFF\nint ok(){}\n#endif\n#if 1\n#elifdef ON\nint late(){}\n#endif";
        let tokens = super::super::lexer::tokens(text, Language::C);
        let analysis = analyze(Language::C, text, &tokens);
        let functions: Vec<_> = analysis
            .declarations
            .iter()
            .filter(|d| d.kind.starts_with("function"))
            .map(|d| &text[d.span.clone()])
            .collect();
        assert_eq!(functions, ["yes", "ok"]);
    }
    #[test]
    fn prototype_parameters_have_sites_without_a_body_scope() {
        let text = "int f(int x, double y); int x; int run(){return x;}";
        let tokens = super::super::lexer::tokens(text, Language::C);
        let mut analyzer = Analyzer::new(Language::C, &tokens);
        analyzer.scan_declarations(declarations::C::default());
        assert_eq!(analyzer.scopes.len(), 2); // file and run
        let parameters: Vec<_> = analyzer
            .out
            .declarations
            .iter()
            .filter(|d| d.kind == "parameter")
            .map(|d| &text[d.span.clone()])
            .collect();
        assert_eq!(parameters, ["x", "y"]);
        assert_eq!(
            targets(Language::C, text, "return x", "x"),
            [text.find("x;").unwrap()]
        );
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
    fn member_chains_preserve_the_root_type_through_each_hop() {
        let src = "struct A { A* next; int read(){return 1;} }; struct B { B* next; int read(){return 2;} }; int run(A* a, B* b){ return a->next->next->read() + unknown->next->read() + b->next->read(); }";
        for (chain, declaration) in [
            ("a->next->next->read()", "read(){return 1"),
            ("b->next->read()", "read(){return 2"),
        ] {
            assert_eq!(
                targets(Language::Cpp, src, chain, "read"),
                [src.find(declaration).unwrap()]
            );
        }
        assert!(targets(Language::Cpp, src, "unknown->next->read()", "read").is_empty());
    }

    #[test]
    fn unmatched_receiver_parenthesis_cannot_bind_to_the_last_type_in_the_file() {
        let source = "struct Last { void picked() {} };\nvoid use(){ ).picked(); }\nLast";
        let analysis = analyze(
            Language::Cpp,
            source,
            &super::super::lexer::tokens(source, Language::Cpp),
        );
        assert!(analysis.direct.iter().all(|(call, _)| {
            analysis.calls[*call].span.start != source.rfind("picked").unwrap()
        }));
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
        let analysis = analyze(Language::C, src, &tokens);
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

#[cfg(test)]
mod physical_directive_tests {
    use super::*;
    #[test]
    fn translation_precedes_directive_and_line_comment_recognition() {
        for (source, expected) in [
            ("%:if 0\nint chosen(void){}\n%:endif", false),
            ("#i\\\nf 0\nint chosen(void){}\n#endif", false),
            ("// ignored \\\n#if 0\nint chosen(void){}", true),
            (
                "#define FLAG /* a\n b */ 1\n#if FLAG\nint chosen(void){}\n#endif",
                true,
            ),
            ("% : if 0\nint chosen(void){}", true),
        ] {
            let tokens = super::super::lexer::tokens(source, Language::C);
            let analysis = analyze(Language::C, source, &tokens);
            assert_eq!(
                analysis
                    .declarations
                    .iter()
                    .any(|d| &source[d.span.clone()] == "chosen"),
                expected,
                "{source}"
            );
        }
    }
}
