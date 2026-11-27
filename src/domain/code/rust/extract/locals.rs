//! Lexical bindings and their initializer projections.
use super::*;

impl Extractor<'_, '_> {
    fn pattern_declarations(
        &mut self,
        range: std::ops::Range<usize>,
        scope: usize,
        kind: &'static str,
        visibility: usize,
    ) -> std::ops::Range<usize> {
        let start = self.analysis.declarations.len();
        let pattern = patterns::Pattern::new(self.tokens, range);
        let mut alternatives = pattern.has_alternatives().then(HashMap::new);
        for (i, spelling) in pattern.declarations() {
            let prior = alternatives
                .as_ref()
                .and_then(|first| first.get(spelling))
                .copied();
            match prior {
                Some(id) => self.alt_pattern_use(i, scope, id),
                None => {
                    let id = self.declare(BindingSite {
                        token: i,
                        scope,
                        kind,
                        visibility,
                        namespace: Namespace::Value,
                    });
                    if let Some(first) = &mut alternatives {
                        first.insert(spelling, id);
                    }
                }
            }
            self.excluded[i] = true;
        }
        start..self.analysis.declarations.len()
    }

    fn alt_pattern_use(&mut self, i: usize, scope: usize, id: usize) {
        let local = self.analysis.calls.len();
        self.analysis.calls.push(Call {
            span: self.tokens[i].span.clone(),
            owner: self.evidence.scopes[scope].owner,
            is_call: false,
        });
        self.evidence.uses.push(Lookup {
            scope,
            ns: Namespace::Value,
            path: Vec::new(),
        });
        self.evidence.direct.insert(local, id);
    }

    pub(super) fn match_arms(&mut self, open: usize) {
        let scope = self.scope;
        for arm in patterns::arms(self.tokens, self.pairs, open) {
            let child = new_scope(&mut self.evidence, scope);
            self.pattern_declarations(arm.pattern, child, "local_binding", 0);
            self.transitions.enter(arm.first, child);
            self.transitions.leave_arm(arm.end, scope);
        }
    }

    pub(super) fn local_binding(&mut self, i: usize, s: &str, prev: &str) {
        let (t, pairs) = (self.tokens, self.pairs);
        let stop = until(t, pairs, i + 1, &["=", ";", ":", "in", "}"]);
        let conditional = conditional_binding(t, i, prev);
        match (s, word(t, stop)) {
            ("for", "in") => self.body_binding(i, stop, conditional, true),
            ("let", "=") if conditional => self.body_binding(i, stop, true, false),
            ("let", "=" | ";" | ":") => self.statement_binding(i, stop),
            _ => {}
        }
    }

    fn body_binding(&mut self, i: usize, stop: usize, conditional: bool, iteration: bool) {
        let (t, pairs) = (self.tokens, self.pairs);
        let (open, guard) = binding_body(t, pairs, stop + 1, conditional);
        if word(t, open) != "{" {
            return;
        }
        let child = *self
            .body_scopes
            .entry(open)
            .or_insert_with(|| new_scope(&mut self.evidence, self.scope));
        if let Some(guard) = guard {
            self.transitions.enter(guard, child);
        }
        let declarations = self.pattern_declarations(
            i + 1..stop,
            child,
            "local_binding",
            t[guard.unwrap_or(open)].span.start,
        );
        if declarations.len() != 1 {
            return;
        }
        let first_decl = declarations.start;
        let projection = if iteration {
            Projection::Element
        } else if matches!(word(t, i + 1), "Some" | "Ok") {
            Projection::Unwrap
        } else {
            return;
        };
        self.simple_initializer(
            first_decl,
            members::ungroup(t, pairs, stop + 1..open),
            projection,
        );
    }

    fn simple_initializer(
        &mut self,
        decl: usize,
        rhs: std::ops::Range<usize>,
        projection: Projection,
    ) {
        if let Some((at, projection)) =
            members::initializer(self.tokens, self.pairs, rhs, projection)
        {
            self.initializers.push((decl, at, projection));
        }
    }

    fn statement_binding(&mut self, i: usize, stop: usize) {
        let (t, pairs) = (self.tokens, self.pairs);
        let end = until(t, pairs, stop, &[";", "}"]);
        if word(t, end) != ";" {
            return;
        }
        let declarations =
            self.pattern_declarations(i + 1..stop, self.scope, "local_binding", t[end].span.end);
        match (word(t, stop), declarations.len()) {
            (":", 1) => self.evidence.members.record_type(declarations.start, self.scope, t, stop + 1),
            ("=", _) => self.binding_initializer(i + 1, stop, end, declarations),
            _ => {}
        }
    }

    // An initializer supplies one nominal type, tuple slots in source order,
    // or one deferred value. Tuple-pattern operands are never scalar fallback.
    fn binding_initializer(
        &mut self,
        pattern: usize,
        equals: usize,
        end: usize,
        declarations: std::ops::Range<usize>,
    ) {
        let (t, pairs) = (self.tokens, self.pairs);
        let decl = declarations.start;
        let single = declarations.len() == 1;
        let rhs = members::ungroup(t, pairs, equals + 1..end);
        if single && let Some(ty) = members::literal_type(t, pairs, rhs.clone()) {
            self.evidence.members.types.insert(decl, (self.scope, ty));
            return;
        }
        if word(t, pattern) == "(" {
            if let Some(at) = members::terminal_operand(t, pairs, rhs.start, rhs.end) {
                self.tuple_initializers(pattern, equals, decl, at);
            }
        } else if single {
            self.simple_initializer(decl, rhs, Projection::Value);
        }
    }

    fn tuple_initializers(&mut self, open: usize, stop: usize, first_decl: usize, rhs: usize) {
        let t = self.tokens;
        // Both the bindings and the tuple elements are in source order. Nested
        // patterns can add bindings without identifying the entire element.
        let mut decl = first_decl;
        let mut part = open + 1;
        let mut index = 0;
        while part < self.pairs[open].min(stop) {
            let end = until_before(t, self.pairs, part..stop, &[",", ")"]);
            let name = part + usize::from(word(t, part) == "mut");
            if end == name + 1 {
                let span = &t[name].span;
                let declarations = &self.analysis.declarations;
                while decl < declarations.len() && declarations[decl].span.start < span.start {
                    decl += 1;
                }
                if declarations.get(decl).is_some_and(|d| d.span == *span) {
                    self.initializers
                        .push((decl, rhs, Projection::Tuple(index)));
                    decl += 1;
                }
            }
            index += 1;
            part = end + 1;
        }
    }
    pub(super) fn formal_parameters(
        &mut self,
        open: usize,
        close: usize,
        child: usize,
        closure: bool,
    ) {
        let (t, pairs) = (self.tokens, self.pairs);
        let context = FormalScope {
            delimiters: open..close,
            child,
            closure,
        };
        for (param, target) in Formal::targets(t, pairs, open + 1..close, closure) {
            self.formal_parameter(param, target, &context);
        }
    }

    fn formal_parameter(&mut self, param: Formal, target: FormalTarget, context: &FormalScope) {
        match target {
            FormalTarget::Name(at) => self.named_parameter(at, param, context),
            FormalTarget::Pattern(span) => {
                self.pattern_declarations(span, context.child, "parameter", 0);
            }
        }
    }

    fn named_parameter(&mut self, at: usize, param: Formal, context: &FormalScope) {
        let (t, pairs) = (self.tokens, self.pairs);
        let FormalScope {
            delimiters,
            child,
            closure,
        } = context;
        let (child, closure) = (*child, *closure);
        let open = delimiters.start;
        let close = delimiters.end;
        let id = self.declare(BindingSite {
                token: at,
                scope: child,
                kind: "parameter",
                visibility: 0,
                namespace: Namespace::Value,
            });
            self.excluded[at] = true;
            match (closure, word(t, at), param.annotation) {
                (false, "self", _) => self.evidence.members.record_self(id, child, t, at),
                (_, _, Some(ty)) => {
                    self.evidence.members.record_type(id, child, t, ty);
                }
                (true, _, None) if at == open + 1 && param.head.end == close => {
                    self.initializers.extend(
                        members::closure_element(t, pairs, open)
                            .map(|at| (id, at, Projection::Element)),
                    );
                }
                _ => {}
            }
    }

    pub(super) fn closure(&mut self, i: usize) {
        let (t, pairs) = (self.tokens, self.pairs);
        let stop = until(t, pairs, i + 1, &["|", ";", ")", "}"]);
        if word(t, stop) != "|" {
            return;
        }
        let child = new_scope(&mut self.evidence, self.scope);
        self.formal_parameters(i, stop, child, true);
        if word(t, stop + 1) == "{" {
            self.body_scopes.insert(stop + 1, child);
        } else {
            let end = until(t, pairs, stop + 1, &[",", ";", ")", "]", "}"]);
            self.transitions.leave_closure(end, self.scope);
            self.scope = child;
        }
    }
}

fn conditional_binding(t: &[Token], i: usize, prev: &str) -> bool {
    matches!(prev, "while" | "if") || prev == "&" && i > 1 && word(t, i - 2) == "&"
}

// The first top-level && starts the region where an if-let binding is visible.
// Skip paired operands so a guard inside a call cannot change lexical scope.
fn binding_body(
    t: &[Token],
    pairs: &[usize],
    start: usize,
    conditional: bool,
) -> (usize, Option<usize>) {
    let first = top_level(t, pairs, start..t.len(), |i, token| {
        matches!(token, "{" | "}" | ";") || conditional && token == "&" && word(t, i + 1) == "&"
    });
    if word(t, first) == "&" {
        (until(t, pairs, first + 2, &["{", ";", "}"]), Some(first))
    } else {
        (first.max(start), None)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_declaration_without_initializer_does_not_consume_the_next_statement() {
        use crate::domain::code::{Language, analyze};
        let text = "fn main(){let pending; First {word: 1}; pending = Second {word: 2};}";
        let analysis = analyze(Language::Rust, text);
        let decl = analysis
            .declarations
            .iter()
            .position(|d| &text[d.span.clone()] == "pending")
            .unwrap();
        let members = &analysis.rust.as_ref().unwrap().members;
        assert!(!members.types.contains_key(&decl));
        assert!(!members.initializers.contains_key(&decl));
    }
}

struct FormalScope {
    delimiters: std::ops::Range<usize>,
    child: usize,
    closure: bool,
}

enum FormalTarget {
    Name(usize),
    Pattern(std::ops::Range<usize>),
}
struct Formal {
    head: std::ops::Range<usize>,
    name: Option<usize>,
    annotation: Option<usize>,
    next: usize,
}

impl Formal {
    fn targets<'a>(
        t: &'a [Token],
        pairs: &'a [usize],
        mut region: std::ops::Range<usize>,
        closure: bool,
    ) -> impl Iterator<Item = (Self, FormalTarget)> + 'a {
        std::iter::from_fn(move || {
            if region.start >= region.end { return None }
            let param = Self::read(t, pairs, region.start, region.end);
            region.start = param.next;
            Some(param)
        }).filter_map(move |param| {
            let target = param.target(t, pairs, closure)?;
            Some((param, target))
        })
    }

    fn read(t: &[Token], pairs: &[usize], start: usize, close: usize) -> Self {
        let end = formal_head_end(t, pairs, start..close);
        let name = formal_name(t, start..end);
        let annotation = (word(t, end) == ":").then_some(end + 1);
        let next = annotation.map_or(end, |ty| parameter_type_end(t, pairs, ty, close)) + 1;
        Self {
            head: start..end,
            name,
            annotation,
            next,
        }
    }
    fn target(&self, t: &[Token], pairs: &[usize], closure: bool) -> Option<FormalTarget> {
        match (self.name, closure) {
            (Some(at), true) => {
                return patterns::Pattern::new(t, self.head.clone())
                    .binds(at)
                    .then_some(FormalTarget::Name(at));
            }
            (Some(at), false) => {
                return (self.annotation.is_some() || word(t, at) == "self")
                    .then_some(FormalTarget::Name(at));
            }
            (None, true) => return Some(FormalTarget::Pattern(self.head.clone())),
            (None, false) => {}
        }
        self.annotation?;
        let last = self.head.end.checked_sub(1)?;
        let open = *pairs.get(last)?;
        // Walk the contents of a tuple, array or struct pattern. The path
        // introducing a constructor is a type use, not a parameter binding.
        (open >= self.head.start && open < last && matches!(word(t, last), ")" | "]" | "}"))
            .then_some(FormalTarget::Pattern(open..self.head.end))
    }
}

fn formal_head_end(t: &[Token], pairs: &[usize], region: std::ops::Range<usize>) -> usize {
    let mut path_colon = None;
    top_level(t, pairs, region, |i, token| match token {
        ":" if path_colon == Some(i) => false,
        ":" if word(t, i + 1) == ":" && t[i].span.end == t[i + 1].span.start => {
            path_colon = Some(i + 1);
            false
        }
        ":" | "," => true,
        _ => false,
    })
}

fn formal_name(t: &[Token], mut head: std::ops::Range<usize>) -> Option<usize> {
    while matches!(word(t, head.start), "&" | "mut" | "ref") {
        head.start += 1;
    }
    let spelling = word(t, head.start);
    (head.start + 1 == head.end && (spelling == "self" || ident(spelling))).then_some(head.start)
}

/// Only a comma outside the annotation's generic arguments separates parameters.
fn parameter_type_end(t: &[Token], pairs: &[usize], start: usize, limit: usize) -> usize {
    let mut depth = 0usize;
    top_level(t, pairs, start..limit, |i, token| match token {
        "<" => {
            depth += 1;
            false
        }
        ">" if generic_close(t, i) => {
            depth = depth.saturating_sub(1);
            false
        }
        "," => depth == 0,
        _ => false,
    })
}
