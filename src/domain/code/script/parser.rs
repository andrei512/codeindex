//! One parser owns the lexical scope stack, pending declarations and references.
//! Named declarations advance the same token cursor as ordinary uses.
use super::*;
mod arrows;
pub(super) mod dialect;
use dialect::Grammar;
mod javascript;
use arrows::Arrow;
pub(super) fn async_modifier(t: &[Token], pairs: &[usize], at: usize) -> bool {
    let next = at + 1;
    t.get(next).is_some_and(|token| token.line == t[at].line)
        && (tok(t, next) == "function" || Arrow::parse(t, pairs, next).is_some())
}

pub(super) struct Parser<'t, 's, G: Grammar> {
    source: &'s str,
    tokens: &'t [Token<'s>],
    pairs: &'t [usize],
    grammar: std::marker::PhantomData<G>,
    analysis: Analysis,
    evidence: Evidence,
    active: ActiveScopes,
    expressions: ExpressionBodies,
    angles: angle_ends::AngleEnds<'t, 's>,
    extra_vars: HashSet<usize>,
    pending_shapes: HashMap<usize, usize>,
    namespace_scopes: HashSet<usize>,
    seen_returns: Vec<u64>,
    references: reference_chain::References<'t, 's>,
    signatures: overloads::Overloads<'s>,
}

#[derive(Clone, Copy)]
pub(super) struct ParserSite<'t, 's> {
    pub(super) index: usize,
    pub(super) scope: usize,
    pub(super) token: &'t Token<'s>,
    pub(super) previous: &'s str,
    pub(super) next: &'s str,
}

impl<'t, 's> ParserSite<'t, 's> {
    pub(super) fn new(tokens: &'t [Token<'s>], index: usize, scope: usize) -> Self {
        Self {
            index,
            scope,
            token: &tokens[index],
            previous: index.checked_sub(1).map_or("", |i| tok(tokens, i)),
            next: tok(tokens, index + 1),
        }
    }
}

struct Publication<'s> {
    keyword: usize,
    scope: usize,
    name: &'s str,
    id: usize,
}

/// A recognized declaration prefix, before its signature or body is consumed.
struct DeclarationHead {
    keyword: usize,
    name: usize,
    kind: &'static str,
}

impl DeclarationHead {
    fn parse(t: &[Token], site: ParserSite, extra: bool, python: bool) -> Option<Self> {
        let (i, prev) = (site.index, site.previous);
        let mut kind = if extra {
            "variable_declarator"
        } else {
            declaration_kind(python, t[i].text)?
        };
        if prev == "." {
            return None;
        }
        let mut name = i + 1;
        if kind == "function_declaration" {
            (name, kind) = Self::function_name(t, i, prev);
        }
        if !t.get(name).is_some_and(word) {
            return None;
        }
        if kind == "variable_declarator" && matches!(tok(t, name + 1), "of" | "in") {
            kind = "local_binding";
        }
        Some(Self {
            keyword: i,
            name,
            kind,
        })
    }

    fn function_name(t: &[Token], keyword: usize, previous: &str) -> (usize, &'static str) {
        let context = if previous == "async" && keyword >= 2 {
            tok(t, keyword - 2)
        } else {
            previous
        };
        let generator = tok(t, keyword + 1) == "*";
        let name = keyword + 1 + usize::from(generator);
        let kind = match context {
            "=" | "(" | "," | ":" | "return" | "[" | "?" | "!" => "function_expression",
            _ if generator => "generator_function_declaration",
            _ => "function_declaration",
        };
        (name, kind)
    }

    fn function(&self) -> bool {
        matches!(
            self.kind,
            "function_declaration"
                | "generator_function_declaration"
                | "function_expression"
                | "function_definition"
        )
    }

    fn type_context(&self, prev: &str, next: &str) -> bool {
        prev == "as"
            || match self.kind {
                "variable_declarator" => matches!(next, "extends" | "is"),
                "type_alias_declaration" => !matches!(next, "=" | "<"),
                _ => false,
            }
    }
}

struct CallableBody {
    paren: usize,
    close: usize,
    body: usize,
    boundary: usize,
}

struct NamedCallable {
    head: DeclarationHead,
    declaration: usize,
    enclosing: usize,
}

impl<'t, 's, G: Grammar> Parser<'t, 's, G> {
    pub fn new(source: &'s str, tokens: &'t [Token<'s>], pairs: &'t [usize]) -> Self {
        let mut evidence = Evidence::default();
        evidence.scopes.push(Scope::default());
        Self {
            source,
            tokens,
            pairs,
            grammar: std::marker::PhantomData,
            evidence,
            analysis: Analysis::default(),
            active: Default::default(),
            expressions: Default::default(),
            angles: angle_ends::AngleEnds::new(tokens),
            extra_vars: Default::default(),
            pending_shapes: Default::default(),
            namespace_scopes: Default::default(),
            seen_returns: Vec::new(),
            signatures: Default::default(),
            references: reference_chain::References::new(tokens, pairs, G::LANGUAGE),
        }
    }
    fn export(t: &[Token<'s>], i: usize) -> Option<&'s str> {
        t[..i]
            .iter()
            .rev()
            .map(|x| x.text)
            .take_while(|s| matches!(*s, "default" | "async" | "export" | "declare" | "abstract"))
            .find(|s| matches!(*s, "default" | "export"))
    }

    /// An inferred result shape is sound only for one written object return.
    /// Record the first return and invalidate its shape as soon as another appears.
    fn record_return(&mut self, owner: usize, shape: Option<usize>) {
        let word = owner / 64;
        if self.seen_returns.len() <= word {
            self.seen_returns.resize(word + 1, 0);
        }
        let bit = 1 << (owner % 64);
        if self.seen_returns[word] & bit != 0 {
            self.evidence.result_shapes.remove(&owner);
        } else {
            self.seen_returns[word] |= bit;
            if let Some(shape) = shape {
                self.evidence.result_shapes.insert(owner, shape);
            }
        }
    }

    fn export_declaration(&mut self, decl: Publication<'_>, namespace: bool) {
        let Publication {
            keyword,
            scope,
            name,
            id,
        } = decl;
        if scope != 0 && !self.namespace_scopes.contains(&scope) {
            return;
        }
        let Some(tag) = self.export_tag(keyword) else {
            return;
        };
        if scope != 0 {
            self.evidence.members.insert(scope, name, id);
            return;
        }
        let key = if !namespace && tag == "default" {
            "default"
        } else {
            name
        };
        self.evidence.exports.insert(key.into(), id);
    }

    fn export_tag(&self, keyword: usize) -> Option<&str> {
        if G::INDENTED {
            Some("export")
        } else {
            Self::export(self.tokens, keyword)
        }
    }

    fn follow_variables(&mut self, mut start: usize) {
        let t = self.tokens;
        while start < t.len() {
            let comma = self.expression_end(start);
            if tok(t, comma) != ","
                || !t.get(comma + 1).is_some_and(word)
                || !matches!(tok(t, comma + 2), "=" | ":" | "," | ";" | "!")
            {
                break;
            }
            self.extra_vars.insert(comma);
            start = comma + 2;
        }
    }

    fn declaration(&mut self, site: ParserSite, extra: bool) -> Option<usize> {
        let ParserSite {
            index,
            scope,
            previous,
            ..
        } = site;
        let head = DeclarationHead::parse(self.tokens, site, extra, G::INDENTED)?;
        if head.function() || head.kind == "class_definition" {
            return Some(self.callable_declaration(head, scope, previous));
        }
        let name = head.name;
        if !G::INDENTED && head.type_context(previous, tok(self.tokens, name + 1)) {
            return Some(index + 1);
        }
        let id = self.register_declaration(&head, scope);
        if !G::INDENTED && head.kind == "variable_declarator" {
            return Some(self.variable_declaration(name, scope, id, extra));
        }
        Some(name + 1)
    }

    fn variable_declaration(&mut self, name: usize, scope: usize, id: usize, extra: bool) -> usize {
        if !extra {
            self.follow_variables(name + 1);
        }
        self.arrow_initializer(name)
            .map_or(name + 1, |arrow| self.enter_arrow(arrow, scope, Some(id)))
    }

    fn callable_declaration(&mut self, head: DeclarationHead, scope: usize, prev: &str) -> usize {
        let (t, end) = (self.tokens, self.pairs);
        let name = head.name;
        let parentheses = self
            .angles
            .arguments(name + 1)
            .map(|open| (open, end[open]));
        let tail = parentheses
            .filter(|_| !G::INDENTED)
            .map(|(_, close)| SignatureEnd::parse(&mut self.angles, end, close));
        if let (Some((paren, close)), Some(SignatureEnd::Declaration(start))) = (parentheses, &tail)
        {
            if close >= t.len() {
                return name + 1;
            }
            return self.record_overload(
                head,
                scope,
                CallableSignature {
                    head: name,
                    parentheses: (paren, close),
                    body: *start,
                },
            );
        }
        if (!G::INDENTED && head.type_context(prev, tok(t, name + 1)))
            || head.function() && parentheses.is_none()
        {
            return head.keyword + 1;
        }
        let declaration = NamedCallable {
            declaration: self.register_declaration(&head, scope),
            enclosing: scope,
            head,
        };
        self.enter_callable(declaration, parentheses, tail.as_ref())
            .unwrap_or(name + 1)
    }

    fn record_overload(
        &mut self,
        head: DeclarationHead,
        scope: usize,
        mut signature: CallableSignature,
    ) -> usize {
        let tokens = self.tokens;
        let stop = self.overload_end(signature.body);
        signature.body = stop;
        let token = &tokens[signature.head];
        let id = self
            .analysis
            .declare_at(token.span.clone(), "function_overload");
        self.export_overload(scope, head.keyword, token.text);
        self.signatures.remember(scope, token.text, id);
        let child = self.evidence.child_scope(scope, Some(id));
        let mut facts = ScopeFacts::new(self.source, child, &mut self.analysis, &mut self.evidence);
        let (open, close) = signature.parentheses;
        facts.parameters(&mut self.angles, self.pairs, open + 1..close, true);
        facts.signature(&mut self.angles, self.pairs, signature);
        stop + usize::from(tok(tokens, stop) == ";")
    }

    fn overload_end(&self, mut stop: usize) -> usize {
        let tokens = self.tokens;
        while stop < tokens.len()
            && !matches!(tok(tokens, stop), ";" | "}" | "export" | "declare" | "function")
        {
            stop = match tok(tokens, stop) {
                "(" | "{" | "[" => self.pairs[stop] + 1,
                _ => stop + 1,
            };
        }
        stop
    }

    fn export_overload(&mut self, scope: usize, keyword: usize, name: &str) {
        if scope != 0 {
            return;
        }
        let Some(tag) = Self::export(self.tokens, keyword) else {
            return;
        };
        let key = if tag == "default" { "default" } else { name };
        self.evidence
            .forward
            .insert(key.into(), Forward::Local(name.into()));
    }

    fn arrow_initializer(&self, name: usize) -> Option<Arrow> {
        if tok(self.tokens, name + 1) != "=" {
            return None;
        }
        let first = name + 2;
        let first = first + usize::from(tok(self.tokens, first) == "async");
        Arrow::parse(self.tokens, self.pairs, first).filter(|arrow| arrow.block)
    }

    fn register_declaration(&mut self, head: &DeclarationHead, scope: usize) -> usize {
        let t = self.tokens;
        let (name, kind) = (head.name, head.kind);
        let token = &t[name];
        let id = if kind == "function_expression" {
            self.analysis.declare_at(token.span.clone(), kind)
        } else {
            self.evidence
                .bind_declaration(token, scope, kind, &mut self.analysis)
        };
        if !G::INDENTED {
            self.observe_declaration_shape(head, scope, id);
        }

        self.export_declaration(
            Publication {
                keyword: head.keyword,
                scope,
                name: t[name].text,
                id,
            },
            false,
        );
        id
    }

    fn observe_declaration_shape(&mut self, head: &DeclarationHead, scope: usize, id: usize) {
        let (name, kind) = (head.name, head.kind);
        let t = self.tokens;
        if matches!(
            kind,
            "class_declaration" | "interface_declaration" | "type_alias_declaration"
        ) && let Some(body) = type_body(&mut self.angles, name + 1)
        {
            self.pending_shapes.insert(body, id);
        }
        let variable = kind == "variable_declarator";
        if let Some(body) =
            ScopeFacts::new(self.source, scope, &mut self.analysis, &mut self.evidence)
                .initializer(t, name, id, variable)
        {
            self.pending_shapes.insert(body, id);
        }
    }

    fn enter_callable(
        &mut self,
        decl: NamedCallable,
        parentheses: Option<(usize, usize)>,
        tail: Option<&SignatureEnd>,
    ) -> Option<usize> {
        let (t, end) = (self.tokens, self.pairs);
        let name = decl.head.name;
        let (paren, close) = parentheses.unwrap_or((name + 1, name));
        if close >= t.len() {
            return None;
        }
        let token = &t[decl.head.keyword];
        let (body, boundary) = if G::INDENTED {
            (
                python_body(t, close)?,
                self.active.indentation(self.source, token),
            )
        } else {
            let body = tail.and_then(SignatureEnd::body)?;
            (body, end[body])
        };
        let child = self
            .evidence
            .child_scope(decl.enclosing, Some(decl.declaration));
        let outline = CallableBody {
            paren,
            close,
            body,
            boundary,
        };
        if token.text == "class" {
            self.enter_class(decl, child, outline);
        } else {
            self.enter_function(decl, child, outline);
        }
        Some(body + 1)
    }

    fn enter_class(&mut self, decl: NamedCallable, child: usize, outline: CallableBody) {
        let CallableBody {
            paren,
            close,
            boundary,
            ..
        } = outline;
        self.active.enter(child, boundary);
        self.evidence.scopes[child].shape = true;
        self.evidence.shapes.insert(decl.declaration, child);
        if close == paren + 2 && word(&self.tokens[paren + 1]) {
            self.references
                .base(child, decl.enclosing, self.tokens[paren + 1].span.clone());
        }
    }

    fn enter_function(&mut self, decl: NamedCallable, child: usize, outline: CallableBody) {
        let NamedCallable {
            head,
            declaration: id,
            enclosing: scope,
        } = decl;
        let CallableBody {
            paren,
            close,
            body,
            boundary,
        } = outline;
        let (t, end) = (self.tokens, self.pairs);
        let first_param = tok(t, paren + 1);
        let receiver = self
            .evidence
            .method_receiver(scope, child, first_param, G::INDENTED);
        self.evidence.scopes[child].this_boundary = true;
        if head.kind == "function_expression" {
            self.evidence
                .declare(child, t[head.name].text, Binding::Local(id));
        }
        self.evidence.functions.insert(id, child);
        let mut facts = ScopeFacts::new(self.source, child, &mut self.analysis, &mut self.evidence);
        let body_scope = if G::INDENTED {
            facts.parameters(&mut self.angles, end, paren + 1..close, false)
        } else {
            facts.callable_signature(
                &mut self.angles,
                end,
                CallableSignature {
                    head: head.name,
                    parentheses: (paren, close),
                    body,
                },
            )
        };
        self.active.enter(body_scope, boundary);
        if receiver
            && let Some(Binding::Local(param)) =
                self.evidence.scopes[child].bindings.get(first_param)
        {
            self.evidence.self_params.insert(*param, scope);
        }
    }

    pub fn analyze(mut self, mut grammar: G) -> Analysis {
        let mut i = 0;
        while i < self.tokens.len() {
            let enclosing = self
                .active
                .enclosing(G::INDENTED, self.source, self.tokens, i);
            let scope = grammar.scope(&mut self, i, enclosing);
            i = self.consume(i, scope, &mut grammar);
        }
        self.signatures.bind(&mut self.analysis, &mut self.evidence);
        self.analysis.script = Some(Box::new(self.evidence));
        self.analysis
    }

    fn record_reference(&mut self, site: ParserSite) {
        if let Some(request) = self.references.request(
            site,
            &mut self.angles,
            &self.evidence,
            self.analysis.calls.len(),
        ) {
            request.publish(&mut self.analysis, &mut self.evidence);
        }
    }

    fn consume(&mut self, i: usize, scope: usize, grammar: &mut G) -> usize {
        if grammar.markup(self, i, scope) {
            return i + 1;
        }
        let site = ParserSite::new(self.tokens, i, scope);
        if site.token.kind == Kind::String {
            self.string(site);
            return i + 1;
        }
        if let Some(after) = grammar.statement(self, site) {
            return after;
        }
        self.consume_name(site, grammar)
    }

    fn string(&mut self, site: ParserSite) {
        let ParserSite {
            index,
            token,
            previous,
            ..
        } = site;
        let prefix = if index > 0 && self.tokens[index - 1].span.end == token.span.start {
            previous
        } else {
            ""
        };
        strings::visit(
            G::LANGUAGE,
            site,
            prefix,
            (&mut self.analysis, &mut self.evidence),
        );
    }

    fn consume_name(&mut self, site: ParserSite, grammar: &mut G) -> usize {
        let ParserSite {
            index,
            scope,
            token,
            ..
        } = site;
        let extra = !G::INDENTED && token.text == "," && self.extra_vars.remove(&index);
        if !extra && !word(token) {
            return index + 1;
        }
        if let Some(after) = self
            .declaration(site, extra)
            .or_else(|| grammar.binding(self, index, scope))
        {
            return after;
        }
        if reference_name(G::LANGUAGE, self.tokens, self.pairs, index) {
            self.record_reference(site);
        }
        index + 1
    }
}
