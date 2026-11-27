use super::*;

pub(in crate::domain::code::script) trait Grammar: Sized {
    const LANGUAGE: Language;
    const INDENTED: bool;
    fn prepare(tokens: &[Token]) -> (Self, Vec<usize>);
    fn scope(&mut self, _p: &mut Parser<Self>, _i: usize, enclosing: usize) -> usize {
        enclosing
    }
    fn markup(&mut self, _p: &mut Parser<Self>, _i: usize, _scope: usize) -> bool {
        false
    }
    fn statement(&mut self, parser: &mut Parser<Self>, site: ParserSite) -> Option<usize>;
    fn binding(&mut self, parser: &mut Parser<Self>, i: usize, scope: usize) -> Option<usize>;
}

pub(in crate::domain::code::script) struct Python {
    comprehensions: ComprehensionScopes,
    arguments: python_bindings::KeywordArguments,
}
impl Parser<'_, '_, Python> {
    fn python_import(&mut self, index: usize, scope: usize) -> Option<usize> {
        let rows =
            imports::ImportRows::new(self.tokens, scope, &mut self.analysis, &mut self.evidence);
        imports::python::parse(rows, index)
    }
}

impl Grammar for Python {
    const LANGUAGE: Language = Language::Python;
    const INDENTED: bool = true;
    fn prepare(tokens: &[Token]) -> (Self, Vec<usize>) {
        let mut comprehensions = ComprehensionScopes::default();
        let mut lists = python_bindings::ArgumentLists::default();
        let pairs = pair_delimiters(tokens, |i, token, stack, event| {
            lists.observe(tokens, i, token, event);
            if token == "for"
                && let Some(&open) = stack.last()
            {
                comprehensions.openings.insert(open);
            }
        });
        (
            Self {
                comprehensions,
                arguments: lists.keywords,
            },
            pairs,
        )
    }
    fn scope(&mut self, p: &mut Parser<Self>, i: usize, enclosing: usize) -> usize {
        self.comprehensions
            .scope(i, p.pairs, enclosing, &mut p.evidence)
    }
    fn statement(&mut self, p: &mut Parser<Self>, site: ParserSite) -> Option<usize> {
        let ParserSite {
            index: i,
            scope,
            previous: prev,
            token,
            ..
        } = site;
        match token.text {
            "__all__" if scope == 0 => {
                imports::python_exports(p.tokens, p.pairs, i, &mut p.evidence);
                None
            }
            "import" | "from" if prev != "." => p.python_import(i, scope),
            _ => None,
        }
    }
    fn binding(&mut self, p: &mut Parser<Self>, i: usize, scope: usize) -> Option<usize> {
        python_bindings::PythonScope {
            scope,
            analysis: &mut p.analysis,
            evidence: &mut p.evidence,
        }
        .observe(p.tokens, i, &self.arguments)
    }
}

pub(in crate::domain::code::script) struct Javascript(jsx::Markup);
impl Grammar for Javascript {
    const LANGUAGE: Language = Language::Js;
    const INDENTED: bool = false;
    fn prepare(tokens: &[Token]) -> (Self, Vec<usize>) {
        let mut first_tag = None;
        let pairs = pair_delimiters(tokens, |i, token, _, _| {
            if token == "<" {
                first_tag.get_or_insert(i);
            }
        });
        let markup =
            first_tag.map_or_else(jsx::Markup::default, |i| jsx::classify(tokens, &pairs, i));
        (Self(markup), pairs)
    }
    fn markup(&mut self, p: &mut Parser<Self>, i: usize, scope: usize) -> bool {
        match self.0.sites.get(&i).copied() {
            Some(jsx::Site::Component(_)) => {
                let row = p.analysis.calls.len();
                p.record_reference(ParserSite::new(p.tokens, i, scope));
                self.0.sites.insert(
                    i,
                    jsx::Site::Component((p.analysis.calls.len() > row).then_some(row)),
                );
            }
            Some(jsx::Site::Property(token)) => {
                if let Some(jsx::Site::Component(Some(row))) = self.0.sites.get(&token) {
                    UseRequest::reference(p.tokens[i].span.clone(), scope, Lookup::PropOf(*row))
                        .publish(&mut p.analysis, &mut p.evidence);
                }
            }
            None => return self.0.ignored(i),
        }
        true
    }
    fn statement(&mut self, p: &mut Parser<Self>, site: ParserSite) -> Option<usize> {
        p.javascript_statement(site)
    }
    fn binding(&mut self, p: &mut Parser<Self>, i: usize, scope: usize) -> Option<usize> {
        if !p.evidence.scopes[scope].shape || !word(&p.tokens[i]) {
            return None;
        }
        let field = Field::read(p.tokens, i)?;
        let key = &p.source[field.span.clone()];
        let id = p.evidence.declare_member(&mut p.analysis, scope, key, field.span);
        if let Some(start) = field.annotation {
            p.evidence.field_type(p.tokens, start, id, scope);
        }
        (!field.shorthand).then_some(i + 1)
    }
}
