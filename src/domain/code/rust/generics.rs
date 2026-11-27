use super::*;

pub(super) struct Scope {
    end: usize,
    names: HashMap<String, usize>,
}

fn trait_bounds(t: &[Token], pairs: &[usize], after_name: usize) -> Vec<Vec<String>> {
    let mut traits = Vec::new();
    if word(t, after_name) == ":" {
        let mut start = after_name + 1;
        loop {
            let bound = members::path(t, start);
            if bound.is_empty() {
                break;
            }
            start += 1 + 3 * (bound.len() - 1);
            if word(t, start) == "<" {
                start = after_angles(t, pairs, start).unwrap_or(t.len());
            }
            traits.push(bound);
            if word(t, start) != "+" {
                break;
            }
            start += 1;
        }
    }
    traits
}

#[derive(Default)]
pub(super) struct Parameters {
    names: HashMap<String, usize>,
    bounds: Vec<(String, Vec<Vec<String>>)>,
}

pub(super) struct ParameterInput<'t, 's> {
    pub tokens: &'t [Token<'s>],
    pub pairs: &'t [usize],
    pub open: usize,
}

enum Header {
    Lifetime,
    Const,
    Type,
}

struct Parameter {
    header: Header,
    start: usize,
    name: usize,
}

impl Parameter {
    fn read(t: &[Token], start: usize) -> Option<Self> {
        let header = match word(t, start) {
            "'" => Header::Lifetime,
            "const" => Header::Const,
            _ => Header::Type,
        };
        let name = start + usize::from(!matches!(header, Header::Type));
        ident(word(t, name)).then_some(Self {
            header,
            start,
            name,
        })
    }
}

impl Parameters {
    fn declare(
        &mut self,
        t: &[Token],
        pairs: &[usize],
        parameter: Parameter,
        output: &mut ParameterOutput<'_>,
    ) {
        let Parameter {
            header,
            start,
            name: at,
        } = parameter;
        let token = word(t, at);
        let (key, start, kind) = match header {
            Header::Lifetime => (format!("'{token}"), start, "lifetime_parameter"),
            Header::Const => {
                output.excluded[start] = true;
                (name(token).to_owned(), at, "generic_parameter")
            }
            Header::Type => {
                let key = name(token).to_owned();
                self.bounds
                    .push((key.clone(), trait_bounds(t, pairs, at + 1)));
                (key, at, "generic_parameter")
            }
        };
        output.excluded[at] = true;
        self.names.insert(key, output.analysis.declarations.len());
        output.analysis.declarations.push(Declaration {
            span: t[start].span.start..t[at].span.end,
            kind,
        });
    }

    pub(super) fn read(
        input: ParameterInput<'_, '_>,
        mut output: ParameterOutput<'_>,
    ) -> Option<(Self, usize)> {
        let ParameterInput {
            tokens,
            pairs,
            open,
        } = input;
        if word(tokens, open) != "<" {
            return None;
        }
        let mut result = Self::default();
        let mut cursor = ParameterCursor {
            tokens,
            pairs,
            at: open + 1,
            depth: 1,
            first: true,
        };
        while let Some(transition) = cursor.next() {
            match transition {
                GenericEvent::Parameter(parameter) => {
                    result.declare(tokens, pairs, parameter, &mut output)
                }
                GenericEvent::Close(after) => {
                    return (!result.names.is_empty()).then_some((result, after));
                }
            }
        }
        None
    }

    pub(super) fn scope(
        self,
        end: usize,
        range: std::ops::Range<usize>,
        members: &mut members::Members,
    ) -> Scope {
        for (name, traits) in self.bounds {
            members
                .bounds
                .entry(name)
                .or_default()
                .push((range.clone(), traits));
        }
        Scope {
            end,
            names: self.names,
        }
    }
}

pub(super) struct ParameterOutput<'a> {
    pub analysis: &'a mut Analysis,
    pub excluded: &'a mut [bool],
}

struct ParameterCursor<'t, 's> {
    tokens: &'t [Token<'s>],
    pairs: &'t [usize],
    at: usize,
    depth: usize,
    first: bool,
}

enum GenericEvent {
    Parameter(Parameter),
    Close(usize),
}

impl ParameterCursor<'_, '_> {
    fn next(&mut self) -> Option<GenericEvent> {
        loop {
            let token = self.tokens.get(self.at)?;
            match self.step(token.text) {
                Step::Continue => {}
                Step::Emit(event) => return Some(event),
                Step::Stop => return None,
            }
        }
    }

    fn step(&mut self, spelling: &str) -> Step {
        let index = self.at;
        let parameter = self
            .first
            .then(|| Parameter::read(self.tokens, index))
            .flatten();
        self.first = self.depth == 1 && spelling == ",";
        let advance = self.advance(spelling, index);
        if let Some(parameter) = parameter {
            return Step::Emit(GenericEvent::Parameter(parameter));
        }
        match advance {
            Advance::Continue => Step::Continue,
            Advance::Close => Step::Emit(GenericEvent::Close(self.at)),
            Advance::Stop => Step::Stop,
        }
    }

    fn advance(&mut self, spelling: &str, index: usize) -> Advance {
        self.at += 1;
        match spelling {
            "<" => self.depth += 1,
            ">" if generic_close(self.tokens, index) => return self.close_angle(),
            "{" | "(" | "[" if self.pairs[index] != usize::MAX => self.at = self.pairs[index] + 1,
            ";" => return Advance::Stop,
            _ => {}
        }
        Advance::Continue
    }

    fn close_angle(&mut self) -> Advance {
        self.depth -= 1;
        if self.depth == 0 {
            Advance::Close
        } else {
            Advance::Continue
        }
    }
}

enum Step {
    Continue,
    Emit(GenericEvent),
    Stop,
}

enum Advance {
    Continue,
    Close,
    Stop,
}

pub(super) fn target(
    t: &[Token],
    i: usize,
    scopes: &[Scope],
) -> Option<(usize, std::ops::Range<usize>)> {
    let s = word(t, i);
    if !ident(s) {
        return None;
    }
    let life = i > 0 && word(t, i - 1) == "'";
    let key = if life {
        std::borrow::Cow::Owned(format!("'{s}"))
    } else {
        std::borrow::Cow::Borrowed(name(s))
    };
    for scope in scopes.iter().rev() {
        if let Some(&target) = scope.names.get(key.as_ref()) {
            return Some((target, t[i - usize::from(life)].span.start..t[i].span.end));
        }
    }
    None
}
pub(super) fn leave(scopes: &mut Vec<Scope>, i: usize) {
    while scopes.last().is_some_and(|s| s.end < i) {
        scopes.pop();
    }
}
