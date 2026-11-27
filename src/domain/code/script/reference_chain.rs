//! Bind each written member to its receiver's use row. Nested calls can leave
//! several receivers pending; a plain dotted member needs only the next row.
use super::*;

pub(super) struct References<'t, 's> {
    tokens: &'t [Token<'s>],
    pairs: &'t [usize],
    language: Language,
    adjacent: Option<(usize, usize)>,
    after_calls: HashMap<usize, usize>,
    prefix: DottedPrefix,
    bases: HashMap<usize, (usize, Range<usize>)>,
}

impl<'t, 's> References<'t, 's> {
    pub(super) fn new(tokens: &'t [Token<'s>], pairs: &'t [usize], language: Language) -> Self {
        Self {
            tokens,
            pairs,
            language,
            adjacent: None,
            after_calls: HashMap::new(),
            prefix: DottedPrefix::default(),
            bases: HashMap::new(),
        }
    }

    /// Record a complete written expression in its enclosing lexical scope.
    pub(super) fn expression(
        &mut self,
        range: Range<usize>,
        scope: usize,
        analysis: &mut Analysis,
        evidence: &mut Evidence,
    ) {
        let Some(hidden) = self.fragment_scopes(range.clone()) else {
            return;
        };
        let mut angles = angle_ends::AngleEnds::new(self.tokens);
        let mut start = range.start;
        for open in hidden.into_iter().chain(std::iter::once(range.end)) {
            for i in start..open {
                if !word(&self.tokens[i]) || !self.expression_name(i) {
                    continue;
                }
                let site = parser::ParserSite::new(self.tokens, i, scope);
                if let Some(request) =
                    self.request(site, &mut angles, evidence, analysis.calls.len())
                {
                    request.publish(analysis, evidence);
                }
            }
            start = self
                .pairs
                .get(open)
                .copied()
                .unwrap_or(open)
                .saturating_add(1);
        }
    }

    // Names before a comprehension's "for" can belong to the scope introduced
    // later. Keep the enclosing group opaque while retaining sibling expressions.
    fn fragment_scopes(&self, range: Range<usize>) -> Option<Vec<usize>> {
        let mut hidden = Vec::new();
        let mut cursor = range.clone();
        while let Some(marker) = cursor.find(|&at| self.scopes_fragment(at)) {
            let open = self.enclosing_group(range.start, marker)?;
            hidden.truncate(hidden.partition_point(|&at| at < open));
            hidden.push(open);
            cursor.start = self.pairs[open].saturating_add(1);
        }
        Some(hidden)
    }

    fn enclosing_group(&self, start: usize, mut cursor: usize) -> Option<usize> {
        while cursor > start {
            cursor -= 1;
            match self.tokens[cursor].text {
                "(" | "[" | "{" => return Some(cursor),
                ")" | "]" | "}" => cursor = cursor.min(self.pairs[cursor]),
                _ => {}
            }
        }
        None
    }

    fn scopes_fragment(&self, i: usize) -> bool {
        let previous = tok(self.tokens, i.wrapping_sub(1));
        match (self.language, self.tokens[i].text) {
            (Language::Python, "lambda" | "for") | (Language::Js, "class" | "function") => {
                previous != "."
            }
            (Language::Js, ">") => previous == "=",
            (Language::Js, _) if tok(self.tokens, i + 1) == "(" => {
                tok(self.tokens, self.pairs[i + 1] + 1) == "{"
            }
            _ => false,
        }
    }

    fn expression_name(&self, i: usize) -> bool {
        let site = (
            self.language,
            tok(self.tokens, i.wrapping_sub(1)),
            tok(self.tokens, i + 1),
        );
        if matches!(site, (Language::Js, "{" | ",", ":")) {
            return false;
        }
        reference_name(self.language, self.tokens, self.pairs, i)
    }
    pub(super) fn base(&mut self, class: usize, scope: usize, span: Range<usize>) {
        self.bases.insert(class, (scope, span));
    }

    fn remember(&mut self, i: usize, row: usize, prev: &str, next: &str) {
        if prev == "." && next == "." {
            self.adjacent = Some((i + 2, row));
        } else if next == "(" {
            let close = self.pairs[i + 1];
            if tok(self.tokens, close) == ")" && tok(self.tokens, close + 1) == "." {
                self.after_calls.insert(close + 2, row);
            }
        }
    }

    fn superclass(&self, i: usize, scope: usize, evidence: &Evidence) -> Option<Lookup> {
        let [before @ .., name, open, close, _] = &self.tokens[..i] else {
            return None;
        };
        if self.language != Language::Python
            || name.text != "super"
            || open.text != "("
            || close.text != ")"
            || before.last().is_some_and(|token| token.text == ".")
        {
            return None;
        }
        let class = evidence.scopes[scope].receiver?;
        let (base_scope, base) = self.bases.get(&class)?;
        Some(Lookup::Super {
            scope,
            base_scope: *base_scope,
            base: base.clone(),
        })
    }

    fn member(&mut self, i: usize, scope: usize, evidence: &Evidence) -> Option<Lookup> {
        if let Some(lookup) = self.superclass(i, scope, evidence) {
            return Some(lookup);
        }
        let prior = self
            .adjacent
            .take()
            .filter(|&(at, _)| at == i)
            .map(|(_, row)| row)
            .or_else(|| self.after_calls.remove(&i));
        if let Some(prior) = prior {
            return Some(Lookup::Chained {
                scope,
                prior,
                qualification: self.qualification(i),
            });
        }
        let token = super::syntax::simple_receiver(self.tokens, i)?;
        Some(Lookup::Name {
            scope,
            receiver: Some(token.span.clone()),
        })
    }

    fn qualification(&mut self, token: usize) -> Option<Range<usize>> {
        if self.language == Language::Python {
            self.prefix.qualifier(self.tokens, token)
        } else {
            None
        }
    }

    pub(super) fn request(
        &mut self,
        site: parser::ParserSite<'_, '_>,
        angles: &mut angle_ends::AngleEnds<'_, '_>,
        evidence: &Evidence,
        next_row: usize,
    ) -> Option<UseRequest> {
        let (i, scope, prev, next, token) =
            (site.index, site.scope, site.previous, site.next, site.token);
        let python = self.language == Language::Python;
        if self.constructor(site, angles) {
            return Some(UseRequest {
                span: token.span.clone(),
                scope,
                is_call: true,
                lookup: Lookup::Construct { scope },
            });
        }
        let lookup = self.lexical_lookup(site, evidence)?;
        let property_key = next == ":" && matches!(lookup, Lookup::Name { .. });
        if !python && property_key {
            return None;
        }
        self.remember(i, next_row, prev, next);
        Some(UseRequest {
            span: token.span.clone(),
            scope,
            is_call: next == "(",
            lookup,
        })
    }

    fn lexical_lookup(&mut self, site: parser::ParserSite, evidence: &Evidence) -> Option<Lookup> {
        let (i, scope) = (site.index, site.scope);
        match site.previous {
            "." => self.member(i, scope, evidence),
            "#" if self.private_member(i) => None,
            _ => Some(Lookup::Name {
                scope,
                receiver: None,
            }),
        }
    }

    fn constructor(&self, site: parser::ParserSite, angles: &mut angle_ends::AngleEnds) -> bool {
        let new_operand = self.language != Language::Python && site.previous == "new";
        new_operand && angles.arguments(site.index + 1).is_some()
    }

    fn private_member(&self, i: usize) -> bool {
        tok(self.tokens, i.wrapping_sub(2)) == "."
    }
}
