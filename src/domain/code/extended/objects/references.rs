//! Bind file-local source uses after all declaration names and shadows are known.
use super::*;
use crate::domain::code::record_unique;
mod grammar;
use grammar::{MemberLookup, Receiver, ReferenceGrammar};

pub(super) struct Bindings<'tree, 'src> {
    syntax: &'tree Syntax<'src>,
    owners: owners::Owners,
    blocked: Vec<bool>,
    shadows: shadows::ShadowBindings<'src>,
    names: MemberNames<'tree>,
}

#[derive(PartialEq, Eq)]
enum UseKind {
    Call,
    Value,
    TypeOnly,
}
struct Use<'src> {
    at: usize,
    previous: &'src str,
    target: WrittenTarget,
    kind: UseKind,
}

enum WrittenTarget {
    Message(usize),
    Type,
    Member,
}

enum MemberAccess {
    Qualified,
    ClassValue,
    Lexical { method_value: bool },
}
enum LexicalRule {
    Function,
    Method,
    ShadowableMethod,
    MethodOrFunction,
}

impl LexicalRule {
    fn new(class: usize, method_value: bool, lookup: MemberLookup) -> Self {
        match (class, method_value, lookup) {
            (ROOT, _, _) | (_, false, MemberLookup::Explicit) => Self::Function,
            (_, true, _) => Self::Method,
            (_, false, MemberLookup::Shadowable) => Self::ShadowableMethod,
            (_, false, MemberLookup::Lexical) => Self::MethodOrFunction,
        }
    }
}

enum Qualifier {
    Receiver(usize),
    EnclosingClass,
    NamedClass(usize),
}

impl Use<'_> {
    fn qualifier(&self, syntax: &Syntax<'_>) -> Option<Qualifier> {
        let receiver = self.at.checked_sub(2)?;
        match (self.previous, syntax.w(receiver), syntax.prev(receiver)) {
            (".", _, ".") => None,
            (".", _, _) => Some(Qualifier::Receiver(receiver)),
            (">", "-", "$this") | (":", ":", "self") => Some(Qualifier::EnclosingClass),
            (":", ":", _) if self.kind == UseKind::Call => {
                Some(Qualifier::NamedClass(receiver.checked_sub(1)?))
            }
            _ => None,
        }
    }

    fn member_access(&self, supports_class_values: bool) -> MemberAccess {
        match (self.previous, self.kind == UseKind::Value) {
            (":", true) if supports_class_values => MemberAccess::ClassValue,
            (":", true) => MemberAccess::Lexical { method_value: true },
            ("." | ">" | ":" | "\\", _) => MemberAccess::Qualified,
            _ => MemberAccess::Lexical {
                method_value: false,
            },
        }
    }
}

impl<'src> Syntax<'src> {
    fn reference<G: ReferenceGrammar>(&self, i: usize) -> Option<Use<'src>> {
        let previous = self.prev(i);
        if matches!(previous, "@" | "#") || G::selector_literal(self, i) {
            return None;
        }
        let type_ref = matches!(
            previous,
            "new" | "extends" | "implements" | "instanceof" | "is" | "as"
        );
        let message_end = G::message_end(self, i);
        let kind = self.reference_kind::<G>(i, message_end.is_some(), type_ref)?;
        let target = match message_end {
            Some(end) => WrittenTarget::Message(end),
            None if type_ref => WrittenTarget::Type,
            None => WrittenTarget::Member,
        };
        Some(Use {
            at: i,
            previous,
            target,
            kind,
        })
    }

    fn reference_kind<G: ReferenceGrammar>(
        &self,
        i: usize,
        message: bool,
        type_ref: bool,
    ) -> Option<UseKind> {
        if self.w(i + 1) == "(" || message {
            return Some(UseKind::Call);
        }
        if G::value_at(self, i) && self.value_ends_at(i) {
            return Some(UseKind::Value);
        }
        type_ref.then_some(UseKind::TypeOnly)
    }
}

/// Only the grammar-dependent passes are specialized. Building the name index
/// and applying declaration precedence are identical for every language.
struct LanguagePasses {
    extract: fn(&mut Syntax<'_>),
    resolve: fn(&mut Bindings<'_, '_>, &mut Analysis),
}

impl LanguagePasses {
    const fn for_grammar<
        G: declarations::DeclarationGrammar + shadows::LocalGrammar + ReferenceGrammar,
    >() -> Self {
        Self {
            extract: G::extract,
            resolve: |bindings, out| bindings.resolve_for::<G>(out),
        }
    }
}

fn language_passes(language: Language) -> &'static LanguagePasses {
    use languages::*;
    const PASSES: &[(Language, LanguagePasses)] = &[
        (Language::Go, LanguagePasses::for_grammar::<Go>()),
        (Language::Kotlin, LanguagePasses::for_grammar::<Kotlin>()),
        (Language::Dart, LanguagePasses::for_grammar::<Dart>()),
        (Language::Java, LanguagePasses::for_grammar::<Java>()),
        (Language::Php, LanguagePasses::for_grammar::<Php>()),
        (Language::Hack, LanguagePasses::for_grammar::<Hack>()),
        (
            Language::ObjectiveC,
            LanguagePasses::for_grammar::<ObjectiveC>(),
        ),
    ];
    PASSES
        .iter()
        .find_map(|(lang, passes)| (*lang == language).then_some(passes))
        .expect("unsupported object language")
}

pub(super) fn analyze(s: &mut Syntax<'_>, out: &mut Analysis) {
    let passes = language_passes(s.lang);
    (passes.extract)(s);
    // A type wins when incomplete syntax gives two declarations the same name token.
    s.decls.sort_by_key(|d| (d.at, d.role != Role::Type));
    s.decls.dedup_by_key(|d| d.at);
    (passes.resolve)(&mut Bindings::new(s, out), out);
}

impl<'tree, 'src> Bindings<'tree, 'src> {
    fn new(syntax: &'tree Syntax<'src>, out: &mut Analysis) -> Self {
        let n = syntax.t.len();
        let mut owners = owners::Owners::default();
        let mut blocked = vec![false; n];
        let mut shadows = shadows::ShadowBindings::default();
        let mut lookup = MemberNames::new();
        for (id, d) in syntax.decls.iter().enumerate() {
            out.declare_at(syntax.t[d.at].span.clone(), d.kind);
            blocked[d.at] = true;
            record_unique(
                &mut lookup,
                (syntax.lookup_scope(d.scope, d.at), d.key.as_ref()),
                id,
            );
            shadows.signature(syntax, id, d, &mut blocked);
            if let Some(body) = &d.body {
                owners.add(body.clone(), id, n);
            }
        }
        Self {
            syntax,
            owners,
            blocked,
            shadows,
            names: lookup,
        }
    }

    fn resolve_for<G: ReferenceGrammar + shadows::LocalGrammar>(&mut self, out: &mut Analysis) {
        let s = self.syntax;
        let mut owner_at = self.owners.cursor();
        let mut declarations = s.decls.iter().peekable();
        let mut supers = java::SuperMethods::default();
        for i in 0..s.t.len() {
            let declared = declarations.next_if(|d| d.at == i).is_some();
            s.observe_shadow::<G>(
                i,
                declared,
                &mut owner_at,
                &mut self.blocked,
                &mut self.shadows,
            );
            let Some(reference) = self.admitted_reference::<G>(i) else {
                continue;
            };
            let owner = owner_at(i);
            let caller = (owner != ROOT).then_some(owner);
            self.bind_use::<G>(&reference, caller, &mut supers, out);
        }
    }

    fn admitted_reference<G: ReferenceGrammar>(&self, at: usize) -> Option<Use<'src>> {
        if self.blocked[at] || !self.syntax.callable_name(at) {
            return None;
        }
        self.syntax.reference::<G>(at)
    }

    fn bind_use<G: ReferenceGrammar>(
        &self,
        reference: &Use,
        caller: Option<usize>,
        supers: &mut java::SuperMethods,
        out: &mut Analysis,
    ) {
        let parameter = self.parameter(reference, caller);
        let target = parameter.or_else(|| self.target::<G>(reference, caller, supers));
        let is_call = parameter.is_none() && reference.kind == UseKind::Call;
        if let Some(id) = target {
            out.direct_edge(
                id,
                self.syntax.t[reference.at].span.clone(),
                caller,
                is_call,
            );
        } else if reference.kind != UseKind::Value {
            out.calls.push(Call {
                is_call,
                span: self.syntax.t[reference.at].span.clone(),
                owner: caller,
            });
        }
    }

    // Parameters and implementations invoked through them are distinct.
    fn parameter(&self, r: &Use, caller: Option<usize>) -> Option<usize> {
        if r.kind == UseKind::TypeOnly
            || matches!(r.previous, "." | ":" | ">")
            || matches!(r.target, WrittenTarget::Message(_))
        {
            return None;
        }
        let s = self.syntax;
        let name = s.w(r.at);
        let id = self.unique(s.decls[caller?].at, name)?;
        (s.decls[id].kind == "parameter" && !self.shadows.contains(&s.parent, r.at, name, None))
            .then_some(id)
    }

    fn named_class(&self, receiver: usize) -> Option<&Decl<'_>> {
        let s = self.syntax;
        if s.unusual_lookup || !s.id(receiver) {
            return None;
        }
        if matches!(s.prev(receiver), "\\" | ":" | ">" | "$") {
            return None;
        }
        let owner = self.unique(s.namespaces.at(receiver), &s.name(receiver))?;
        let class = &s.decls[owner];
        if class.kind != "class_declaration" {
            return None;
        }
        let parent = s.parent[class.at];
        (parent == ROOT || s.namespaces.blocks.contains(&parent)).then_some(class)
    }

    fn named_static_method(
        &self,
        receiver: usize,
        at: usize,
        caller: Option<usize>,
    ) -> Option<usize> {
        let s = self.syntax;
        let class = self.named_class(receiver)?;
        let id = self.unique(class.at, &s.name(at))?;
        let method = &s.decls[id];
        let accessible = match method.binding {
            Binding::PublicStatic => true,
            Binding::ScopedStatic => caller.is_some_and(|owner| s.decls[owner].scope == class.at),
            _ => false,
        };
        (accessible && method.body.is_some()).then_some(id)
    }

    fn target<G: ReferenceGrammar>(
        &self,
        r: &Use,
        caller: Option<usize>,
        supers: &mut java::SuperMethods,
    ) -> Option<usize> {
        match r.target {
            WrittenTarget::Message(end) => self.message_target(caller?, r.at, end),
            WrittenTarget::Type => self.type_target(r.at),
            WrittenTarget::Member => self.member_target::<G>(r, caller, supers),
        }
    }

    fn type_target(&self, at: usize) -> Option<usize> {
        let s = self.syntax;
        if s.unusual_lookup {
            return None;
        }
        self.bound(s.namespaces.at(at), &s.name(at), Role::Type)
    }

    fn member_target<G: ReferenceGrammar>(
        &self,
        r: &Use,
        caller: Option<usize>,
        supers: &mut java::SuperMethods,
    ) -> Option<usize> {
        let s = self.syntax;
        let i = r.at;
        let class = caller
            .map(|id| s.decls[id].scope)
            .or_else(|| G::initializer(s, i))
            .unwrap_or(ROOT);
        match r.member_access(G::CLASS_METHOD_REFERENCE) {
            MemberAccess::Qualified => self.qualified_method::<G>(r, class, caller, supers),
            MemberAccess::ClassValue => self.class_method_value(i, class),
            MemberAccess::Lexical { method_value } if !s.unusual_lookup => {
                self.lexical_target::<G>(i, class, caller, method_value)
            }
            MemberAccess::Lexical { .. } => None,
        }
    }

    fn class_method_value(&self, i: usize, class: usize) -> Option<usize> {
        let s = self.syntax;
        let receiver = i.checked_sub(3)?;
        if class == ROOT || s.prev(receiver) == "." {
            return None;
        }
        if s.w(receiver) != s.w(class) {
            return None;
        }
        self.bound(class, &s.name(i), Role::Method)
    }

    fn message_target(&self, caller: usize, at: usize, end: usize) -> Option<usize> {
        let method = &self.syntax.decls[caller];
        if self.syntax.w(at - 1) != "self" || method.role != Role::Method {
            return None;
        }
        let key = self
            .syntax
            .message_key(at, end, method.key.get(..1).unwrap_or(""));
        self.bound(method.scope, &key, Role::Method)
    }

    fn lexical_target<G: ReferenceGrammar>(
        &self,
        i: usize,
        class: usize,
        caller: Option<usize>,
        method_value: bool,
    ) -> Option<usize> {
        let s = self.syntax;
        let key = s.name(i);
        // A present but ambiguous member blocks the file-level candidate.
        let method = || {
            self.names
                .get(&(class, key.as_ref()))
                .map(|id| id.and_then(|id| self.direct(id, Role::Method)))
        };
        let function = || self.bound(s.namespaces.at(i), &key, Role::Function);
        use LexicalRule::*;
        let target = match LexicalRule::new(class, method_value, G::MEMBERS) {
            Function => function()?,
            Method => return method().flatten(),
            ShadowableMethod => method().flatten()?,
            MethodOrFunction => {
                if let Some(target) = method() {
                    return target;
                }
                function()?
            }
        };
        (!self.shadows.contains(&s.parent, i, s.w(i), caller)).then_some(target)
    }

    fn unique(&self, scope: usize, key: &str) -> Option<usize> {
        self.names.get(&(scope, key)).copied().flatten()
    }

    fn direct(&self, id: usize, role: Role) -> Option<usize> {
        let decl = &self.syntax.decls[id];
        (decl.role == role && decl.binding != Binding::Dynamic).then_some(id)
    }

    fn bound(&self, scope: usize, key: &str, role: Role) -> Option<usize> {
        self.direct(self.unique(scope, key)?, role)
    }

    fn receiver_method<G: ReferenceGrammar>(
        &self,
        receiver: usize,
        i: usize,
        class: usize,
        caller: Option<usize>,
        supers: &mut java::SuperMethods,
    ) -> Option<usize> {
        let s = self.syntax;
        let word = s.w(receiver);
        match G::receiver(word)? {
            Receiver::This => {}
            Receiver::DeclaredReceiver => {
                if word != s.w(s.decls[caller?].receiver?)
                    || self.shadows.contains(&s.parent, i, word, None)
                {
                    return None;
                }
            }
            Receiver::Super => {
                let parent = supers.parent(s, class, &self.names)?;
                let id = self.unique(parent, s.w(i))?;
                return (s.decls[id].role == Role::Method && supers.matches(s, i, id))
                    .then_some(id);
            }
        }
        self.bound(class, &s.name(i), Role::Method)
    }

    fn qualified_method<G: ReferenceGrammar>(
        &self,
        r: &Use,
        class: usize,
        caller: Option<usize>,
        supers: &mut java::SuperMethods,
    ) -> Option<usize> {
        let s = self.syntax;
        let i = r.at;
        match r.qualifier(s)? {
            Qualifier::NamedClass(receiver) => G::named_class(s, caller)
                .then(|| self.named_static_method(receiver, i, caller))
                .flatten(),
            other if class != ROOT => self.enclosed_method::<G>(other, i, (class, caller), supers),
            _ => None,
        }
    }

    fn enclosed_method<G: ReferenceGrammar>(
        &self,
        qualifier: Qualifier,
        i: usize,
        (class, caller): (usize, Option<usize>),
        supers: &mut java::SuperMethods,
    ) -> Option<usize> {
        match qualifier {
            Qualifier::Receiver(receiver) => {
                self.receiver_method::<G>(receiver, i, class, caller, supers)
            }
            Qualifier::EnclosingClass => self.bound(class, &self.syntax.name(i), Role::Method),
            Qualifier::NamedClass(_) => None,
        }
    }
}
