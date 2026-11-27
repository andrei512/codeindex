//! ECMAScript scopes whose boundary is known before visiting their body.
//! Each successful entry returns the next token for the shared parser cursor.
use super::*;

impl Parser<'_, '_, dialect::Javascript> {
    /// Consume module exports, scope entries and destructuring before an
    /// ordinary identifier can be interpreted as a declaration or reference.
    pub(super) fn javascript_statement(&mut self, site: ParserSite) -> Option<usize> {
        if let Some(after) = self
            .module_statement(site)
            .or_else(|| self.scope_statement(site))
        {
            return Some(after);
        }
        let ParserSite {
            index: i,
            scope,
            next,
            token,
            ..
        } = site;
        let (tokens, pairs) = (self.tokens, self.pairs);
        if let Some(arrow) = Arrow::parse(tokens, pairs, i) {
            return Some(self.enter_arrow(arrow, scope, None));
        }
        if token.text == "return" {
            self.observe_return(scope, next);
            return None;
        }
        let function = js_function::JsFunction::parse(site, pairs, &mut self.angles)?;
        Some(self.enter_js_callable(function, scope))
    }

    fn enter_js_callable(&mut self, function: js_function::JsFunction, scope: usize) -> usize {
        let owner = function.name.map(|span| {
            let id = self.analysis.declarations.len();
            if self.evidence.scopes[scope].shape {
                self.evidence
                    .members
                    .insert(scope, &self.source[span.clone()], id);
            }
            self.analysis.declarations.push(Declaration {
                span,
                kind: "method_definition",
            });
            id
        });
        let child = self.evidence.child_scope(scope, owner);
        if owner.is_some() {
            self.evidence.scopes[child].receiver = Some(scope);
        } else {
            self.evidence.scopes[child].this_boundary = true;
        }
        let mut facts = ScopeFacts::new(self.source, child, &mut self.analysis, &mut self.evidence);
        let body = function.signature.body;
        let body_scope = facts.callable_signature(&mut self.angles, self.pairs, function.signature);
        self.active.enter(body_scope, self.pairs[body]);
        body + 1
    }

    fn module_statement(&mut self, site: ParserSite) -> Option<usize> {
        let ParserSite {
            index: i,
            scope,
            previous,
            token,
            ..
        } = site;
        let mut rows = imports::ImportRows::new(self.tokens, scope, &mut self.analysis, &mut self.evidence);
        match token.text {
            "import" if previous != "." => imports::javascript::parse(rows, i),
            "export" => rows.exports(i),
            _ => None,
        }
    }

    fn scope_statement(&mut self, site: ParserSite) -> Option<usize> {
        let ParserSite {
            index: i,
            scope,
            previous,
            token,
            ..
        } = site;
        match token.text {
            "namespace" | "module" => self
                .namespace_head(site)
                .then(|| self.namespace_scope(i, scope)),
            "{" => {
                (self.pairs[i] < self.tokens.len()).then(|| self.enter_brace(i, scope, previous))
            }
            _ => self.paired_scope(site),
        }
    }

    fn paired_scope(&mut self, site: ParserSite) -> Option<usize> {
        let ParserSite {
            index: i,
            scope,
            previous,
            next,
            token,
            ..
        } = site;
        match token.text {
            "const" | "let" | "var" => {
                matches!(next, "[" | "{").then(|| self.destructuring(i, scope))
            }
            "for" | "catch" => {
                let admitted = previous != "." && self.paired_head(i, next);
                admitted.then(|| self.control_scope(i, scope))
            }
            _ => None,
        }
    }

    fn destructuring(&mut self, i: usize, scope: usize) -> usize {
        let (tokens, pairs) = (self.tokens, self.pairs);
        ScopeFacts::new(self.source, scope, &mut self.analysis, &mut self.evidence).pattern(
            tokens,
            pairs,
            i + 1,
            "local_binding",
        );
        pairs[i + 1].min(tokens.len()) + 1
    }

    fn paired_head(&self, i: usize, next: &str) -> bool {
        next == "(" && self.pairs[i + 1] < self.tokens.len()
    }

    fn namespace_head(&self, site: ParserSite) -> bool {
        site.previous != "."
            && self.tokens.get(site.index + 1).is_some_and(word)
            && tok(self.tokens, site.index + 2) == "{"
    }

    fn observe_return(&mut self, scope: usize, next: &str) {
        if next != "{"
            && let Some(owner) = self.evidence.scopes[scope].owner
        {
            self.record_return(owner, None);
        }
    }

    fn control_scope(&mut self, i: usize, scope: usize) -> usize {
        let (tokens, pairs) = (self.tokens, self.pairs);
        let close = pairs[i + 1];
        let body = close + 1;
        let stop = if tok(tokens, body) == "{" {
            pairs[body]
        } else {
            self.expression_end(body)
        };
        let child = self
            .evidence
            .child_scope(scope, self.evidence.scopes[scope].owner);
        self.active.enter(child, stop);
        if tokens[i].text == "catch" {
            ScopeFacts::new(self.source, child, &mut self.analysis, &mut self.evidence).parameters(
                &mut self.angles,
                pairs,
                i + 2..close,
                false,
            );
            body
        } else {
            i + 2
        }
    }

    fn namespace_scope(&mut self, i: usize, scope: usize) -> usize {
        let name = &self.tokens[i + 1];
        let id = self
            .evidence
            .bind_token(&mut self.analysis, scope, (name, "module_declaration"));
        self.export_declaration(
            Publication {
                keyword: i,
                scope,
                name: name.text,
                id,
            },
            true,
        );
        let child = self.evidence.child_scope(scope, None);
        self.namespace_scopes.insert(child);
        self.evidence.shapes.insert(id, child);
        self.active.enter(child, self.pairs[i + 2]);
        i + 3
    }

    fn enter_brace(&mut self, i: usize, scope: usize, prev: &str) -> usize {
        let owner = self.evidence.scopes[scope].owner;
        let child = self.evidence.child_scope(scope, owner);
        if prev == "return"
            && let Some(owner) = owner
        {
            self.evidence.scopes[child].shape = true;
            self.record_return(owner, Some(child));
        }
        if let Some(decl) = self.pending_shapes.remove(&i) {
            self.evidence.scopes[child].shape = true;
            self.evidence.shapes.insert(decl, child);
        }
        self.active.enter(child, self.pairs[i]);
        i + 1
    }
}
