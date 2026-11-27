use super::*;
mod fields;
mod items;
mod locals;
mod references;

use references::{LexicalUse, TokenUses};

pub(in crate::domain::code) fn analyze(t: &[Token], text_len: usize) -> Analysis {
    analyze_paired(t, text_len, &delimiters(t).mates)
}
pub(in crate::domain::code) fn analyze_paired(
    t: &[Token],
    text_len: usize,
    pairs: &[usize],
) -> Analysis {
    Extractor::new(t, pairs).analyze(text_len)
}

#[derive(Clone, Copy)]
struct GenericSignature {
    after_parameters: usize,
    terminator: usize,
}

struct BindingSite {
    token: usize,
    scope: usize,
    kind: &'static str,
    visibility: usize,
    namespace: Namespace,
}

struct Extractor<'t, 'src> {
    tokens: &'t [Token<'src>],
    pairs: &'t [usize],
    analysis: Analysis,
    evidence: Evidence,
    scope: usize,
    body_scopes: HashMap<usize, usize>,
    module_attribute: Option<(usize, ModulePaths)>,
    excluded: Vec<bool>,
    transitions: patterns::Transitions,
    match_blocks: HashSet<usize>,
    // Textual macro bindings extend from the definition to its enclosing brace.
    // Only names otherwise treated as transparent expression macros need entries.
    macro_ends: HashMap<&'src str, usize>,
    generic_scopes: Vec<generics::Scope>,
    delimiters: Vec<usize>,
    uses: TokenUses,
    initializers: Vec<(usize, usize, Projection)>,
}

impl<'t, 'src> Extractor<'t, 'src> {
    fn new(tokens: &'t [Token<'src>], pairs: &'t [usize]) -> Self {
        Self {
            tokens,
            pairs,
            scope: 0,
            analysis: Analysis::default(),
            evidence: Evidence {
                scopes: vec![Scope::default()],
                ..Evidence::default()
            },
            body_scopes: HashMap::new(),
            module_attribute: None,
            excluded: vec![false; tokens.len()],
            transitions: patterns::Transitions::default(),
            match_blocks: HashSet::new(),
            macro_ends: HashMap::new(),
            generic_scopes: Vec::new(),
            delimiters: Vec::new(),
            uses: TokenUses::new(tokens.len()),
            initializers: Vec::new(),
        }
    }
    fn declare(&mut self, site: BindingSite) -> usize {
        let token = &self.tokens[site.token];
        let id = self.analysis.declare_at(token.span.clone(), site.kind);
        self.evidence.scopes[site.scope]
            .names
            .entry(name(token.text).to_owned())
            .or_default()
            .push(Binding {
                decl: id,
                visibility: site.visibility,
                local: matches!(site.kind, "parameter" | "local_binding"),
                ns: site.namespace,
            });
        id
    }

    fn analyze(mut self, text_len: usize) -> Analysis {
        let t = self.tokens;
        let mut i = 0;
        while i < t.len() {
            i = self.token(i);
        }
        for (decl, token, projection) in self.initializers {
            if let Some(id) = self.uses.at(token) {
                self.evidence
                    .members
                    .initializers
                    .insert(decl, (id, projection));
            }
        }
        if text_len > 0 {
            self.analysis.declare_at(0..text_len, "source_file");
        }
        self.analysis.rust = Some(Box::new(self.evidence));
        self.analysis
    }
    fn token(&mut self, i: usize) -> usize {
        let t = self.tokens;
        self.enter_token(i);
        let s = t[i].text;
        let prev = i.checked_sub(1).map_or("", |p| word(t, p));
        let after = match s {
            "#" => self.attribute(i),
            "!" | "macro_rules" => self.macro_item(i, s, prev),
            "use" => self.import_item(i),
            "{" => {
                self.open_brace(i);
                Some(i + 1)
            }
            "}" => {
                self.scope = self.evidence.scopes[self.scope].parent.unwrap_or(0);
                Some(i + 1)
            }
            _ => {
                self.inspect_reference_token(i, s, prev);
                None
            }
        };
        after.unwrap_or_else(|| {
            self.reference(i, prev);
            i + 1
        })
    }

    fn inspect_reference_token(&mut self, i: usize, spelling: &str, previous: &str) {
        match spelling {
            "impl" => self.impl_scope(i, previous),
            "match" => self.expect_match(i),
            "let" | "for" => self.local_binding(i, spelling, previous),
            "|" => self.possible_closure(i, previous),
            _ => self.named_item(i, previous),
        }
    }

    fn possible_closure(&mut self, i: usize, previous: &str) {
        if matches!(
            previous,
            "(" | "," | "=" | "move" | "async" | ">" | "return" | "["
        ) {
            self.closure(i);
        }
    }

    fn attribute(&mut self, i: usize) -> Option<usize> {
        let t = self.tokens;
        match word(t, i + 1) {
            "[" => {
                let (after, module) =
                    config::attributes(t, self.pairs, i, &mut self.evidence.conditions);
                self.module_attribute = module;
                Some(after)
            }
            "!" if self.scope == 0 => {
                self.evidence.conditional_crate |= word(t, i + 2) == "[" && word(t, i + 3) == "cfg";
                None
            }
            _ => None,
        }
    }

    fn macro_item(&mut self, i: usize, s: &str, prev: &str) -> Option<usize> {
        let t = self.tokens;
        let next = word(t, i + 1);
        let open = match (s, next) {
            ("!", "(" | "[" | "{") if self.opaque_invocation(i, prev) => i + 1,
            ("macro_rules", "!") if ident(word(t, i + 2)) => {
                self.macro_definition(i + 2);
                i + 3
            }
            _ => return None,
        };
        Some(after_group(self.pairs, open).unwrap_or(t.len()))
    }

    fn import_item(&mut self, i: usize) -> Option<usize> {
        let t = self.tokens;
        // A precise capture bound (impl Trait + use<T>) has no import tree.
        if word(t, i + 1) == "<" {
            return None;
        }
        let end = until(t, self.pairs, i + 1, &[";", "}"]);
        imports(
            &t[i + 1..end],
            self.scope,
            &mut self.evidence,
            &mut self.analysis,
        );
        Some(end + usize::from(word(t, end) == ";"))
    }

    fn enter_token(&mut self, i: usize) {
        generics::leave(&mut self.generic_scopes, i);
        if let Some(next) = self.transitions.at(i) {
            self.scope = next;
        }
        let s = self.tokens[i].text;
        while self
            .delimiters
            .last()
            .is_some_and(|&open| self.pairs[open] <= i)
        {
            self.delimiters.pop();
        }
        if matches!(s, "{" | "(" | "[") && self.pairs[i] != usize::MAX {
            self.delimiters.push(i);
        }
    }

    fn opaque_invocation(&self, bang: usize, spelling: &str) -> bool {
        let qualified =
            bang >= 3 && self.tokens[bang - 2].text == ":" && self.tokens[bang - 3].text == ":";
        opaque_macro(spelling)
            || !qualified && self.macro_ends.get(spelling).is_some_and(|&end| bang < end)
    }

    fn enter_generics(&mut self, index: usize, open: usize) -> Option<GenericSignature> {
        let (parameters, after) = generics::Parameters::read(
            generics::ParameterInput {
                tokens: self.tokens,
                pairs: self.pairs,
                open,
            },
            generics::ParameterOutput {
                analysis: &mut self.analysis,
                excluded: &mut self.excluded,
            },
        )?;
        let body = until(self.tokens, self.pairs, after, &["{", ";", "}"]);
        let end = if word(self.tokens, body) == "{" && self.pairs[body] != usize::MAX {
            self.pairs[body]
        } else {
            body
        };
        if end > index {
            let range = self.tokens[index].span.start
                ..self.tokens.get(end).map_or(usize::MAX, |t| t.span.end);
            self.generic_scopes
                .push(parameters.scope(end, range, &mut self.evidence.members));
        }
        Some(GenericSignature {
            after_parameters: after,
            terminator: body,
        })
    }

    fn signature_end(&self, start: usize, generic: Option<GenericSignature>) -> usize {
        let stops = &["{", ";", "}"];
        match generic {
            Some(g) if start <= g.terminator => g.terminator,
            _ => until(self.tokens, self.pairs, start, stops),
        }
    }

    fn body_open(&self, start: usize, generic: Option<GenericSignature>) -> Option<usize> {
        let open = self.signature_end(start, generic);
        (word(self.tokens, open) == "{").then_some(open)
    }
    fn impl_scope(&mut self, keyword: usize, prev: &str) {
        // Return and argument positions use impl Trait without an item body.
        if !matches!(
            prev,
            "" | ";" | "{" | "}" | "]" | "unsafe" | "default" | "const"
        ) {
            return;
        }
        let generic = self.enter_generics(keyword, keyword + 1);
        if let Some(open) = self.body_open(keyword + 1, generic) {
            let child = self.associated_scope(open);
            self.evidence
                .members
                .record_impl(child, self.tokens, keyword + 1, open);
        }
    }
    fn associated_scope(&mut self, open: usize) -> usize {
        let child = new_scope(&mut self.evidence, self.scope);
        self.evidence.scopes[child].associated = true;
        self.body_scopes.insert(open, child);
        child
    }
    fn expect_match(&mut self, i: usize) {
        if let Some(open) = self.body_open(i + 1, None) {
            self.match_blocks.insert(open);
        }
    }
    fn open_brace(&mut self, i: usize) {
        let t = self.tokens;
        if let Some(child) = self.body_scopes.remove(&i) {
            self.scope = child;
        } else {
            let child = Scope {
                literal: members::before(t, i),
                parent: Some(self.scope),
                owner: self.evidence.scopes[self.scope].owner,
                module: self.evidence.scopes[self.scope].module,
                ..Default::default()
            };
            self.scope = self.evidence.scopes.len();
            self.evidence.scopes.push(child);
        }
        if self.match_blocks.remove(&i) {
            self.match_arms(i);
        }
    }
    fn reference(&mut self, i: usize, prev: &str) {
        let (t, pairs) = (self.tokens, self.pairs);
        if self.excluded[i] {
            return;
        }
        if !self.generic_scopes.is_empty()
            && !matches!(prev, "." | ":")
            && let Some((target, span)) = generics::target(t, i, &self.generic_scopes)
        {
            let lookup = Lookup {
                scope: self.scope,
                ns: Namespace::Type,
                path: Vec::new(),
            };
            let id = self.record_use(i, span, false, lookup);
            self.evidence.direct.insert(id, target);
            self.excluded[i] = true;
            return;
        }
        if t[i].kind != Kind::Word {
            return;
        }
        let literal = &self.evidence.scopes[self.scope].literal;
        if let Some((receiver, is_call)) = self.uses.receiver(t, pairs, i, literal) {
            let lookup = Lookup {
                scope: self.scope,
                ns: Namespace::Either,
                path: Vec::new(),
            };
            let id = self.record_use(i, t[i].span.clone(), is_call, lookup);
            self.evidence.members.uses.insert(id, receiver);
        }
        if let Some(lookup) = self.uses.lexical(LexicalUse {
            tokens: t,
            at: i,
            scope: self.scope,
            enclosing: &self.evidence.scopes[self.scope],
            delimiter: self.delimiters.last().copied(),
        }) {
            self.record_use(i, t[i].span.clone(), call_follows(t, self.pairs, i), lookup);
        }
    }

    fn record_use(
        &mut self,
        at: usize,
        span: std::ops::Range<usize>,
        called: bool,
        lookup: Lookup,
    ) -> usize {
        let id = self
            .evidence
            .record_use(&mut self.analysis, span, called, lookup);
        self.uses.remember(at, id);
        id
    }
}

// Arguments of recognized expression macros are visible to navigation.
fn opaque_macro(name: &str) -> bool {
    (name == "#" || ident(name))
        && !matches!(
            name,
            "format"
                | "matches"
                | "format_args"
                | "write"
                | "writeln"
                | "println"
                | "print"
                | "eprintln"
                | "assert"
                | "assert_eq"
                | "assert_ne"
                | "debug_assert"
                | "debug_assert_eq"
                | "vec"
                | "dbg"
                | "panic"
                | "warn"
                | "debug"
                | "info"
                | "error"
                | "trace"
                | "anyhow"
                | "bail"
                | "ensure"
        )
}

fn call_follows(t: &[Token], pairs: &[usize], i: usize) -> bool {
    word(t, expression_arguments(t, pairs, i + 1)) == "("
}
