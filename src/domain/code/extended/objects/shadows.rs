use super::{Decl, HashMap, HashSet, ROOT, Syntax};
use std::ops::Range;

#[derive(Default)]
pub(super) struct ShadowBindings<'a> {
    locals: HashMap<(usize, &'a str), usize>,
    parameters: HashSet<(usize, &'a str)>,
}

impl<'a> ShadowBindings<'a> {
    pub(super) fn signature(
        &mut self,
        syntax: &Syntax<'a>,
        id: usize,
        decl: &Decl<'_>,
        blocked: &mut [bool],
    ) {
        let Some(range) = &decl.signature else { return };
        blocked[range.clone()].fill(true);
        // A prototype has no use that can refer to its formal parameters.
        if decl.body.is_none() {
            return;
        }
        let objc = syntax.lang == super::Language::ObjectiveC;
        self.parameters.extend(range.clone().filter_map(|i| {
            let tag = objc && matches!(syntax.prev(i), "struct" | "union" | "enum");
            (i != decl.at && syntax.id(i) && !tag).then(|| (id, syntax.w(i)))
        }));
    }

    pub(super) fn contains(
        &self,
        parents: &[usize],
        use_at: usize,
        name: &str,
        caller: Option<usize>,
    ) -> bool {
        if caller.is_some_and(|id| self.parameters.contains(&(id, name))) {
            return true;
        }
        if self.locals.is_empty() {
            return false;
        }
        let mut scope = parents[use_at];
        while scope != ROOT {
            if self
                .locals
                .get(&(scope, name))
                .is_some_and(|&start| start <= use_at)
            {
                return true;
            }
            scope = parents[scope];
        }
        false
    }
}

/// The source position of a shadow matters: earlier uses in the same scope
/// still refer to the outer declaration.
impl<'a> ShadowBindings<'a> {
    fn local(&mut self, scope: usize, name: &'a str, visible_from: usize) {
        self.locals.entry((scope, name)).or_insert(visible_from);
    }

    fn assignment(&mut self, syntax: &Syntax<'a>, operator: usize, names: Range<usize>) {
        let scope = syntax.parent[operator];
        for i in names.step_by(2) {
            self.local(scope, syntax.w(i), operator);
        }
    }

    fn callback(
        &mut self,
        s: &Syntax<'a>,
        scope: usize,
        tokens: Range<usize>,
        blocked: &mut [bool],
    ) {
        for i in tokens {
            if s.id(i) {
                self.local(scope, s.w(i), i);
                blocked[i] = true;
            }
        }
    }
}

/// Shadowing constructs that introduce multiple names at one source position.
pub(super) enum Introducer {
    /// All names become visible at the declaration operator.
    Assignment(Range<usize>),
    /// Braced callbacks carry the body scope; inline lambdas use the enclosing scope.
    Parameters {
        names: Range<usize>,
        body: Option<usize>,
    },
}

impl Introducer {
    fn record<'a>(
        self,
        syntax: &Syntax<'a>,
        i: usize,
        owner: &mut impl FnMut(usize) -> usize,
        bindings: (&mut [bool], &mut ShadowBindings<'a>),
    ) {
        let (blocked, shadows) = bindings;
        let local_scope = syntax.parent[i] != ROOT;
        match self {
            Self::Assignment(names) if local_scope => shadows.assignment(syntax, i, names),
            Self::Parameters { names, body } => {
                record_parameters(syntax, (i, body, names), owner, (blocked, shadows));
            }
            _ => {}
        }
    }
}

fn record_parameters<'a>(
    syntax: &Syntax<'a>,
    (i, body, names): (usize, Option<usize>, Range<usize>),
    owner: &mut impl FnMut(usize) -> usize,
    (blocked, shadows): (&mut [bool], &mut ShadowBindings<'a>),
) {
    let local_scope = syntax.parent[i] != ROOT;
    let scope = match body {
        None => i,
        Some(body) if local_scope && owner(i) != ROOT => body,
        _ => return,
    };
    shadows.callback(syntax, scope, names, blocked);
}

pub(super) trait LocalGrammar {
    fn binding(_: &Syntax<'_>, _: usize) -> bool {
        false
    }
    fn initializer(_: &Syntax<'_>, _: usize) -> bool {
        false
    }
    fn introducer(_: &Syntax<'_>, _: usize) -> Option<Introducer> {
        None
    }
}

use super::languages::{Dart, Go, Kotlin, ObjectiveC};

impl LocalGrammar for super::languages::Java {}
impl LocalGrammar for super::languages::Php {}
impl LocalGrammar for super::languages::Hack {}

impl<'a> Syntax<'a> {
    pub(super) fn observe_shadow<G: LocalGrammar>(
        &self,
        i: usize,
        declared: bool,
        owner: &mut impl FnMut(usize) -> usize,
        blocked: &mut [bool],
        shadows: &mut ShadowBindings<'a>,
    ) {
        let scope = self.parent[i];
        if self.admits_local::<G>(i, declared, scope, owner) {
            shadows.local(scope, self.w(i), i);
        }
        let Some(introducer) = G::introducer(self, i) else {
            return;
        };
        introducer.record(self, i, owner, (blocked, shadows));
    }

    fn admits_local<G: LocalGrammar>(
        &self,
        i: usize,
        declared: bool,
        scope: usize,
        owner: &mut impl FnMut(usize) -> usize,
    ) -> bool {
        if scope == ROOT {
            return false;
        }
        if !declared && !G::binding(self, i) {
            return false;
        }
        self.id(i) && (owner(i) != ROOT || G::initializer(self, i))
    }

    fn typed_local(&self, i: usize) -> bool {
        let prev = self.prev(i);
        (i > 0 && self.id(i - 1) && !matches!(prev, "return" | "throw" | "case")
            || matches!(prev, "*" | ">" | "?"))
            && matches!(self.w(i + 1), "=" | ";" | "," | ")")
    }

    fn arrow_at(&self, at: usize) -> bool {
        self.w(at) == "-" && self.w(at + 1) == ">"
    }

    fn callback_parameters(&self, i: usize) -> Option<Introducer> {
        let end = self.close(i)?;
        (self.w(end + 1) == "{").then_some(Introducer::Parameters {
            names: i + 1..end,
            body: Some(end + 1),
        })
    }
}

impl LocalGrammar for Go {
    fn binding(s: &Syntax<'_>, i: usize) -> bool {
        matches!(s.prev(i), "var" | "const")
    }
    fn introducer(s: &Syntax<'_>, i: usize) -> Option<Introducer> {
        match s.w(i) {
            ":" => go_assignment(s, i),
            "(" if s.prev(i) == "func" => s.callback_parameters(i),
            _ => None,
        }
    }
}

fn go_assignment(s: &Syntax<'_>, colon: usize) -> Option<Introducer> {
    let mut first = colon.checked_sub(1)?;
    if s.w(colon + 1) != "=" || !s.id(first) {
        return None;
    }
    while let Some(prior) = previous_assignment_name(s, first) {
        first = prior;
    }
    Some(Introducer::Assignment(first..colon))
}

fn previous_assignment_name(s: &Syntax<'_>, at: usize) -> Option<usize> {
    let previous = at.checked_sub(2)?;
    (s.w(previous + 1) == "," && s.id(previous)).then_some(previous)
}

impl LocalGrammar for Kotlin {
    fn initializer(s: &Syntax<'_>, i: usize) -> bool {
        s.kotlin_initializer_type(i).is_some()
    }
    fn introducer(s: &Syntax<'_>, i: usize) -> Option<Introducer> {
        if s.w(i) != "{" {
            return None;
        }
        // Observe the parameter list before its tokens can be mistaken for references.
        // A Kotlin when condition can also end with an arrow; keep the same
        // identifier/comma/colon restriction as the declaration grammar.
        let start = i + 1;
        let arrow = (start..s.t.len()).find(|&p| !s.id(p) && !matches!(s.w(p), "," | ":"))?;
        (s.arrow_at(arrow) && s.parent[arrow] == i).then_some(Introducer::Parameters {
            names: start..arrow,
            body: None,
        })
    }
}

impl LocalGrammar for Dart {
    fn binding(s: &Syntax<'_>, i: usize) -> bool {
        s.typed_local(i)
    }
    fn introducer(s: &Syntax<'_>, i: usize) -> Option<Introducer> {
        match s.w(i) {
            "(" if !matches!(s.prev(i), "if" | "while" | "for" | "switch" | "catch") => {
                s.callback_parameters(i)
            }
            _ => None,
        }
    }
}

impl LocalGrammar for ObjectiveC {
    fn binding(s: &Syntax<'_>, i: usize) -> bool {
        s.typed_local(i)
    }
    fn introducer(s: &Syntax<'_>, i: usize) -> Option<Introducer> {
        (s.w(i) == "(" && s.prev(i) == "^")
            .then(|| s.callback_parameters(i))
            .flatten()
    }
}
