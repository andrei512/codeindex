//! Written Python binding targets and keyword-argument uses.
use super::*;

/// match and case introduce a compound statement only at a statement head
/// with a top-level colon. Parenthesized expressions can span physical lines.
pub(super) fn pattern_header(t: &[Token], pairs: &[usize], at: usize) -> bool {
    if !starts_physical_line(t, at) {
        return false;
    }
    let mut i = at + 1;
    while t.get(i).is_some_and(|token| token.line == t[i - 1].line) {
        match t[i].text {
            ":" => return true,
            ";" | "=" => break,
            _ => i = next_pattern_token(t, pairs, i),
        }
    }
    false
}

fn starts_physical_line(tokens: &[Token], at: usize) -> bool {
    at == 0 || tokens[at - 1].line != tokens[at].line
}

fn next_pattern_token(tokens: &[Token], pairs: &[usize], at: usize) -> usize {
    if matches!(tokens[at].text, "(" | "[" | "{") && pairs[at] < tokens.len() {
        pairs[at] + 1
    } else {
        at + 1
    }
}

#[derive(Default)]
pub(super) struct KeywordArguments(HashMap<usize, usize>);

#[derive(Default)]
pub(super) struct ArgumentLists {
    pub(super) keywords: KeywordArguments,
    callee: Option<usize>,
    enclosing: Vec<Option<usize>>,
}

impl ArgumentLists {
    // A balanced group restores the call in effect before its opener. This
    // also preserves the previous recovery for unmatched closers and semicolons.
    pub(super) fn observe(
        &mut self,
        t: &[Token],
        i: usize,
        token: &str,
        event: syntax::DelimiterEvent,
    ) {
        use syntax::DelimiterEvent::*;
        match (event, token) {
            (Open, _) => self.open_group(t, i, token),
            (Close, _) => self.callee = self.enclosing.pop().flatten(),
            (Token, ";") => self.callee = None,
            (Token, "=") => self.observe_keyword(t, i),
            _ => {}
        }
    }

    fn open_group(&mut self, tokens: &[Token], i: usize, token: &str) {
        self.enclosing.push(self.callee);
        if token == "(" {
            self.callee = unqualified_callee(tokens, i);
        }
    }

    fn observe_keyword(&mut self, t: &[Token], equals: usize) {
        let Some(callee) = self.callee else { return };
        let Some(before) = equals.checked_sub(2) else {
            return;
        };
        if matches!(tok(t, before), "(" | ",") {
            self.keywords.0.insert(equals - 1, callee);
        }
    }
}

fn unqualified_callee(t: &[Token], open: usize) -> Option<usize> {
    let name = open.checked_sub(1)?;
    let prior = tok(t, name.wrapping_sub(1));
    (prior != "." && word(&t[name])).then_some(name)
}

/// Only a complete assignment or for-header proves that these written names
/// bind locally. A comma assignment is flat; a loop target also admits grouped
/// and starred names. Neither grammar treats an attribute as a local write.
fn local_targets(t: &[Token], i: usize, starts_line: bool) -> Option<Range<usize>> {
    if t[i].text == "for" {
        let start = i + 1;
        let offset = t[start..].iter().position(|token| {
            token.text == "in" || !word(token) && !matches!(token.text, "(" | ")" | "," | "*")
        })?;
        let end = start + offset;
        return (t[end].text == "in").then_some(start..end);
    }
    if !starts_line || tok(t, i + 1) != "," {
        return None;
    }
    let mut end = i;
    while t.get(end).is_some_and(word) {
        end += 1;
        if tok(t, end) != "," {
            break;
        }
        end += 1;
    }
    (tok(t, end) == "=").then_some(i..end)
}

enum Equality {
    Member,
    Keyword,
    Assignment,
}

impl Equality {
    fn at(t: &[Token], i: usize, previous: &str) -> Option<Self> {
        match previous {
            "." => Some(Self::Member),
            "," | "(" => Some(Self::Keyword),
            _ if statement_head(t, i, previous)
                && NameKind::of(Language::Python, t[i].text).identifier() =>
            {
                Some(Self::Assignment)
            }
            _ => None,
        }
    }
}

fn statement_head(t: &[Token], i: usize, previous: &str) -> bool {
    previous == ";" || i == 0 || t[i - 1].line != t[i].line
}

pub(super) struct PythonScope<'a> {
    pub(super) scope: usize,
    pub(super) analysis: &'a mut Analysis,
    pub(super) evidence: &'a mut Evidence,
}
impl PythonScope<'_> {
    fn local(&mut self, token: &Token) {
        if !self.evidence.scopes[self.scope]
            .bindings
            .contains_key(token.text)
        {
            self.evidence
                .bind_token(self.analysis, self.scope, (token, "local_binding"));
        }
    }

    fn lexical_use(&mut self, token: &Token) {
        let lookup = Lookup::Name {
            scope: self.scope,
            receiver: None,
        };
        UseRequest::reference(token.span.clone(), self.scope, lookup)
            .publish(self.analysis, self.evidence);
    }

    fn assignment(&mut self, t: &[Token], i: usize) {
        let token = &t[i];
        let bindings = &mut self.evidence.scopes[self.scope];
        let existing = match bindings.bindings.get_mut(token.text) {
            Some(binding) => {
                if matches!(binding, Binding::Import(_) | Binding::Package) {
                    *binding = Binding::Conflicting;
                }
                true
            }
            None => bindings.directives.contains_key(token.text),
        };
        if existing {
            self.lexical_use(token);
            return;
        }
        let id = self
            .evidence
            .bind_declaration(token, self.scope, "local_binding", self.analysis);
        if t.get(i + 2).is_some_and(word) && tok(t, i + 3) == "(" {
            self.evidence
                .types
                .insert(id, (self.scope, t[i + 2].span.clone()));
        }
        if self.scope == 0 {
            self.evidence.exports.insert(token.text.into(), id);
        }
    }

    fn scope_directives(&mut self, t: &[Token], i: usize) -> usize {
        let token = &t[i];
        let global = token.text == "global";
        let names = t[i + 1..]
            .iter()
            .take_while(|name| name.line == token.line && (word(name) || name.text == ","));
        let mut end = i + 1;
        for name in names {
            self.record_directive(name, global);
            end += 1;
        }
        end
    }

    fn record_directive(&mut self, name: &Token, global: bool) {
        if word(name) {
            self.evidence.scopes[self.scope]
                .directives
                .insert(name.text.to_owned(), global);
            self.lexical_use(name);
        }
    }

    fn bind_self_field(&mut self, t: &[Token], i: usize) -> bool {
        let Some(receiver) = i.checked_sub(2) else {
            return false;
        };
        let name = tok(t, receiver);
        if !Evidence::receiver_parameter(name) {
            return false;
        }
        let Some(Binding::Local(id)) = self.evidence.binding(self.scope, name) else {
            return false;
        };
        let Some(&shape) = self.evidence.self_params.get(id) else {
            return false;
        };
        let token = &t[i];
        if !self
            .evidence
            .members
            .declare(shape, token.text, self.analysis.declarations.len())
        {
            return false;
        }
        self.analysis
            .declare_at(token.span.clone(), "local_binding");
        true
    }

    fn equal_sign(
        &mut self,
        t: &[Token],
        i: usize,
        prev: &str,
        keywords: &KeywordArguments,
    ) -> Option<usize> {
        match Equality::at(t, i, prev)? {
            Equality::Member => self.bind_self_field(t, i).then_some(i + 1),
            Equality::Keyword => {
                if let Some(&callee) = keywords.0.get(&i) {
                    self.keyword_argument(&t[i], &t[callee]);
                }
                Some(i + 1)
            }
            Equality::Assignment => {
                self.assignment(t, i);
                Some(i + 1)
            }
        }
    }

    fn keyword_argument(&mut self, name: &Token, callee: &Token) {
        let lookup = Lookup::Argument {
            scope: self.scope,
            callee: callee.span.clone(),
        };
        UseRequest::reference(name.span.clone(), self.scope, lookup)
            .publish(self.analysis, self.evidence);
    }

    pub(super) fn observe(
        &mut self,
        t: &[Token],
        i: usize,
        keywords: &KeywordArguments,
    ) -> Option<usize> {
        let token = &t[i];
        let prev = i.checked_sub(1).map_or("", |j| tok(t, j));
        let next = tok(t, i + 1);
        if matches!(token.text, "global" | "nonlocal") {
            return Some(self.scope_directives(t, i));
        }
        if prev == "as" && word(token) {
            self.local(token);
            return Some(i + 1);
        }
        let writes = next == "=" && tok(t, i + 2) != "=";
        if writes && let Some(after) = self.equal_sign(t, i, prev, keywords) {
            return Some(after);
        }
        let names = local_targets(t, i, starts_physical_line(t, i))?;
        for name in t[names.clone()].iter().filter(|name| word(name)) {
            self.local(name);
        }
        Some(names.end + 1)
    }
}
