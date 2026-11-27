//! Direct lexical bindings for Python and ECMAScript. A module edge must name a
//! captured module. Dynamic members and unresolved modules yield no target.
pub(in crate::domain::code) mod analyzer;
mod imports;
pub(super) use imports::js_named_specifiers;
pub(super) use imports::python::{member_name as python_import_member, module_name as python_import_module};
mod active_scopes;
use active_scopes::ActiveScopes;
mod wire;
mod jsx;
mod js_function;
mod expression_bodies;
pub(super) use expression_bodies::ExpressionBodies;
mod angle_ends;
mod parser;
mod overloads;
mod labels;
mod scoped_names;
pub(super) use labels::public_labels;
mod projects;
mod python_bindings;
mod resolution;
mod reference_chain;
mod strings;
mod syntax;
#[cfg(test)]
use super::Names;
use super::{Analysis, Call, Declaration, Kind, Language, ForwardNames, Site, Token, Unit};
pub(super) use resolution::{Modules,resolve_files};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};
pub(super) use angle_ends::angle_end;
use syntax::*;
pub(super) fn paired_for_score(t: &[Token]) -> Vec<usize> {
    paired(t)
}

#[derive(Default)]
struct Scope {
    shape: bool,
    receiver: Option<usize>,
    this_boundary: bool,
    parent: Option<usize>,
    owner: Option<usize>,
    bindings: HashMap<String, Binding>,
    directives: HashMap<String, bool>,
}
#[derive(Clone, Copy)]
enum Binding {
    Local(usize),
    Import(usize),
    Package,
    Conflicting,
    Namespace { import: usize, local: usize },
}
impl Binding {
    fn imported(self) -> Option<usize> {
        match self {
            Self::Import(id) | Self::Namespace { import: id, .. } => Some(id),
            _ => None,
        }
    }
}
enum Forward {
    Namespace(usize),
    Local(String),
    Import(usize),
}
struct Import {
    module: String,
    name: Option<String>,
}
enum Lookup {
    Name {
        scope: usize,
        receiver: Option<Range<usize>>,
    },
    Argument {
        scope: usize,
        callee: Range<usize>,
    },
    Known(usize),
    Super {
        scope: usize,
        base_scope: usize,
        base: Range<usize>,
    },
    Chained {
        prior: usize,
        scope: usize,
        qualification: Option<Range<usize>>,
    },
    Construct {
        scope: usize,
    },
    Prop {
        scope: usize,
        component: Range<usize>,
    },
    Imported(usize),
    PropOf(usize),
}
#[derive(Default)]
pub(super) struct Evidence {
    scopes: Vec<Scope>,
    imports: Vec<Import>,
    uses: Vec<Lookup>,
    exports: HashMap<String, usize>,
    forward: HashMap<String, Forward>,
    stars: Vec<String>,
    packages: scoped_names::ScopedNames,
    wildcards: Vec<(usize, usize)>,
    all: Option<Vec<String>>,
    dynamic_all: bool,
    families: HashMap<usize, Vec<usize>>,
    shapes: HashMap<usize, usize>,
    result_shapes: HashMap<usize, usize>,
    annotated_returns: HashMap<usize, (usize, Range<usize>)>,
    annotated_shapes: HashMap<usize, usize>,
    result_types: HashMap<usize, (usize, Range<usize>)>,
    element_types: HashMap<usize, (usize, Range<usize>)>,
    iterated: HashMap<usize, (usize, Range<usize>)>,
    props: HashMap<usize, usize>,
    members: scoped_names::ScopedNames,
    types: HashMap<usize, (usize, Range<usize>)>,
    functions: HashMap<usize, usize>,
    destructured_parameters: Vec<Range<usize>>,
    self_params: HashMap<usize, usize>,
}
enum LexicalBinding<'a> {
    Global,
    Nonlocal,
    Root,
    Local(&'a Binding),
    Outer,
}

impl Scope {
    fn binding_decision(&self, name: &str, root: bool) -> LexicalBinding<'_> {
        if root {
            return LexicalBinding::Root;
        }
        match self.directives.get(name) {
            Some(true) => LexicalBinding::Global,
            Some(false) => LexicalBinding::Nonlocal,
            None => self
                .bindings
                .get(name)
                .map_or(LexicalBinding::Outer, LexicalBinding::Local),
        }
    }
}

impl Evidence {
    fn bind_declaration(
        &mut self,
        token: &Token,
        scope: usize,
        kind: &'static str,
        analysis: &mut Analysis,
    ) -> usize {
        let id = self.bind_token(analysis, scope, (token, kind));
        if self.scopes[scope].shape {
            self.members.insert(scope, token.text, id);
        }
        id
    }

    fn bind_token(
        &mut self,
        analysis: &mut Analysis,
        scope: usize,
        (token, kind): (&Token, &'static str),
    ) -> usize {
        let id = analysis.declare_at(token.span.clone(), kind);
        self.declare(scope, token.text, Binding::Local(id));
        id
    }

    fn declare_member(
        &mut self,
        analysis: &mut Analysis,
        shape: usize,
        key: &str,
        span: Range<usize>,
    ) -> usize {
        let id = analysis.declare_at(span, "local_binding");
        self.members.insert(shape, key, id);
        id
    }

    /// These spelling ranges are consumed after parsing, when binding uses
    /// against scopes and selecting names from inferred types.
    fn lookup_spans(&self) -> impl Iterator<Item = &Range<usize>> {
        let uses = self.uses.iter().filter_map(|lookup| match lookup {
            Lookup::Name { receiver, .. } => receiver.as_ref(),
            Lookup::Chained { qualification, .. } => qualification.as_ref(),
            Lookup::Super { base: span, .. }
            | Lookup::Argument { callee: span, .. }
            | Lookup::Prop {
                component: span, ..
            } => Some(span),
            Lookup::Construct { .. }
            | Lookup::PropOf(_)
            | Lookup::Known(_)
            | Lookup::Imported(_) => None,
        });
        let tables = [
            &self.types,
            &self.iterated,
            &self.element_types,
            &self.result_types,
            &self.annotated_returns,
        ];
        uses.chain(
            tables
                .into_iter()
                .flat_map(|table| table.values().map(|(_, span)| span)),
        )
    }

    fn public_wildcard(&self, name: &str) -> bool {
        !self.dynamic_all
            && self.all.as_ref().map_or_else(
                || !name.starts_with('_'),
                |names| names.iter().any(|public| public == name),
            )
    }

    fn field_type(&mut self, tokens: &[Token], start: usize, id: usize, scope: usize) {
        let Some(ty) = tokens.get(start).filter(|t| word(t)) else {
            return;
        };
        let result = ty.text == "ReturnType"
            && tok(tokens, start + 1) == "<"
            && tok(tokens, start + 2) == "typeof";
        if let Some(target) = tokens.get(start + 3).filter(|t| result && word(t)) {
            self.result_types.insert(id, (scope, target.span.clone()));
        } else {
            self.types.insert(id, (scope, ty.span.clone()));
        }
    }
    fn child_scope(&mut self, parent: usize, owner: Option<usize>) -> usize {
        let child = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            owner,
            ..Scope::default()
        });
        child
    }

    /// Python methods skip the enclosing class in lexical name lookup. Only
    /// an explicit self/cls parameter supplies member lookup on that class.
    fn receiver_parameter(name: &str) -> bool {
        matches!(name, "self" | "cls")
    }

    fn method_receiver(
        &mut self,
        scope: usize,
        child: usize,
        parameter: &str,
        python: bool,
    ) -> bool {
        if !python || !self.scopes[scope].shape {
            return false;
        }
        self.scopes[child].parent = self.scopes[scope].parent;
        let receiver = Self::receiver_parameter(parameter);
        self.scopes[child].receiver = receiver.then_some(scope);
        receiver
    }

    pub(super) fn has_method_receiver(&self, declaration: usize) -> bool {
        self.functions
            .get(&declaration)
            .is_some_and(|&scope| self.scopes[scope].receiver.is_some())
    }
    fn binding(&self, scope: usize, name: &str) -> Option<&Binding> {
        let mut nonlocal = false;
        let mut ancestry = std::iter::successors(Some(scope), |&id| self.scopes[id].parent);
        ancestry
            .find_map(|id| {
                let scope = &self.scopes[id];
                match scope.binding_decision(name, id == 0) {
                    LexicalBinding::Root => {
                        Some((!nonlocal).then(|| scope.bindings.get(name)).flatten())
                    }
                    LexicalBinding::Global => Some(self.scopes[0].bindings.get(name)),
                    LexicalBinding::Nonlocal => {
                        nonlocal = true;
                        None
                    }
                    LexicalBinding::Local(binding) => Some(Some(binding)),
                    LexicalBinding::Outer => None,
                }
            })
            .flatten()
    }
    fn declare(&mut self, scope: usize, name: &str, binding: Binding) {
        self.scopes[scope]
            .bindings
            .entry(name.to_owned())
            .and_modify(|b| *b = Binding::Conflicting)
            .or_insert(binding);
    }
}
/// A shorthand property declares a member and also reads its lexical name.
/// A typed or modified field consumes the name as a declaration.
struct Field {
    span: Range<usize>,
    annotation: Option<usize>,
    shorthand: bool,
}

impl Field {
    fn read(tokens: &[Token], i: usize) -> Option<Self> {
        let previous = tok(tokens, i.wrapping_sub(1));
        let next = tok(tokens, i + 1);
        let colon = i + 1 + usize::from(next == "?");
        let annotation = (tok(tokens, colon) == ":").then_some(colon + 1);
        let shorthand = match (previous, next) {
            ("{" | ",", "," | "}") => true,
            ("private" | "readonly" | "public" | "protected" | "#", "=" | ";") => false,
            _ if annotation.is_some() => false,
            _ => return None,
        };
        let first = i - usize::from(previous == "#");
        Some(Self { span: tokens[first].span.start..tokens[i].span.end, annotation, shorthand })
    }
}

fn word(t: &Token) -> bool {
    t.kind == Kind::Word && !t.text.starts_with(|c: char| c.is_ascii_digit())
}
fn declaration_kind(python: bool, keyword: &str) -> Option<&'static str> {
    if python {
        return match keyword {
            "def" => Some("function_definition"),
            "class" => Some("class_definition"),
            _ => None,
        };
    }
    ecmascript_declaration(keyword)
}

fn ecmascript_declaration(keyword: &str) -> Option<&'static str> {
    Some(match keyword {
        "function" => "function_declaration",
        "class" => "class_declaration",
        "const" | "let" | "var" => "variable_declarator",
        "interface" => "interface_declaration",
        "type" => "type_alias_declaration",
        "enum" => "enum_declaration",
        _ => return None,
    })
}

#[derive(PartialEq, Eq)]
enum NameKind {
    Name,
    Reserved,
    Pattern,
    Accessor,
    Async,
    Of,
}
impl NameKind {
    fn of(lang: Language, word: &str) -> Self {
        // A spelling has a distinct role in each dialect. Only the calling
        // grammar decides whether a contextual role is an identifier here.
        let (python, javascript) = Self::roles(word);
        if lang == Language::Python {
            python
        } else {
            javascript
        }
    }

    fn roles(word: &str) -> (Self, Self) {
        use NameKind::*;
        match word {
            "match" => (Pattern, Name),
            "case" => (Pattern, Reserved),
            "get" | "set" => (Name, Accessor),
            "async" => (Reserved, Async),
            "of" => (Name, Of),
            "as" | "await" | "break" | "class" | "continue" | "else" | "finally" | "for" | "if"
            | "import" | "in" | "return" | "try" | "while" | "with" | "yield" => {
                (Reserved, Reserved)
            }
            _ => Self::reserved_word(word),
        }
    }

    fn reserved_word(word: &str) -> (Self, Self) {
        use NameKind::*;
        match word {
            "False" | "None" | "True" | "and" | "assert" | "def" | "del" | "elif" | "except"
            | "from" | "global" | "is" | "lambda" | "nonlocal" | "not" | "or" | "pass"
            | "raise" => (Reserved, Name),
            "abstract" | "any" | "boolean" | "catch" | "const" | "constructor" | "debugger"
            | "declare" | "default" | "delete" | "do" | "enum" | "export" | "extends" | "false"
            | "function" | "implements" | "instanceof" | "interface" | "let" | "module"
            | "namespace" | "new" | "null" | "number" | "private" | "protected" | "public"
            | "readonly" | "static" | "string" | "super" | "switch" | "this" | "throw" | "true"
            | "type" | "typeof" | "undefined" | "unknown" | "var" | "void" => (Name, Reserved),
            _ => (Name, Name),
        }
    }

    fn identifier(self) -> bool {
        self != Self::Reserved
    }

    fn expression(self, t: &[Token], pairs: &[usize], i: usize) -> bool {
        match self {
            Self::Name => true,
            Self::Reserved => false,
            Self::Pattern => !python_bindings::pattern_header(t, pairs, i),
            contextual => contextual.js_expression(t, pairs, i),
        }
    }

    fn js_expression(self, t: &[Token], pairs: &[usize], i: usize) -> bool {
        if tok(t, i.wrapping_sub(1)) == "." {
            return true;
        }
        match self {
            Self::Async if parser::async_modifier(t, pairs, i) => false,
            Self::Accessor | Self::Async => !js_function::method_modifier(t, pairs, i),
            Self::Of => !t.get(i.wrapping_sub(1)).is_some_and(js_iteration_operand),
            _ => unreachable!("contextual JavaScript keyword"),
        }
    }
}
fn js_iteration_operand(prior: &Token) -> bool {
    matches!(prior.text, "]" | "}") || word(prior) && NameKind::of(Language::Js, prior.text).identifier()
}

fn keyword(lang: Language, s: &str) -> bool {
    NameKind::of(lang, s) != NameKind::Name
}
fn reference_name(lang: Language, t: &[Token], pairs: &[usize], i: usize) -> bool {
    NameKind::of(lang, t[i].text).expression(t, pairs, i)
}
fn tok<'a>(t: &[Token<'a>], i: usize) -> &'a str {
    t.get(i).map_or("", |x| x.text)
}
struct UseRequest {
    span: std::ops::Range<usize>,
    scope: usize,
    is_call: bool,
    lookup: Lookup,
}

impl UseRequest {
    fn reference(span: std::ops::Range<usize>, scope: usize, lookup: Lookup) -> Self {
        Self {
            span,
            scope,
            lookup,
            is_call: false,
        }
    }

    fn publish(self, analysis: &mut Analysis, evidence: &mut Evidence) {
        let Self {
            span,
            scope,
            lookup,
            is_call,
        } = self;
        analysis.calls.push(Call {
            span,
            owner: evidence.scopes[scope].owner,
            is_call,
        });
        evidence.uses.push(lookup);
    }
}

#[derive(Default)]
struct ComprehensionScopes {
    openings: HashSet<usize>,
    active: Vec<(usize, usize)>,
}

impl ComprehensionScopes {
    fn scope(&mut self, i: usize, pairs: &[usize], enclosing: usize, evidence: &mut Evidence) -> usize {
        while self.active.last().is_some_and(|&(stop, _)| stop <= i) {
            self.active.pop();
        }
        let parent = self.active.last().map_or(enclosing, |&(_, scope)| scope);
        if pairs[i] == pairs.len() || !self.openings.contains(&i) {
            return parent;
        }
        let child = evidence.child_scope(parent, evidence.scopes[parent].owner);
        self.active.push((pairs[i], child));
        child
    }
}

#[cfg(test)]
mod tests;

/// Source bytes read by the script resolver and public-label writer after parsing.
/// Call only when both files have the same serialized parser evidence.
pub(super) fn same_binding_text(analysis: &Analysis, lang: Language, old: &str, new: &str) -> bool {
    let Some(e) = analysis.script.as_ref() else {
        return false;
    };
    let same = |r: &Range<usize>| {
        old.get(r.clone())
            .is_some_and(|a| Some(a) == new.get(r.clone()))
    };
    let names = analysis
        .declarations
        .iter()
        .map(|d| &d.span)
        .chain(analysis.calls.iter().map(|c| &c.span))
        .chain(e.lookup_spans());
    names.into_iter().all(same)
        && (lang != Language::Python
            || analysis.declarations.iter().all(|d| {
                super::indentation(old, d.span.start) == super::indentation(new, d.span.start)
            }))
}
