//! Resolve a written use against the receiver or lexical candidates selected
//! by the parser. Only ambiguous calls need an owned overload list.
use super::{Analyzer, Binding, Role, csharp, unique};

/// Argument ranges and source spellings live only until the next overload
/// query in this file. Single-candidate sites require none of these buffers.
#[derive(Default)]
pub(super) struct Arguments<'a> {
    spans: Vec<(usize, usize)>,
    labels: Vec<&'a str>,
    types: Vec<&'a str>,
}

impl Arguments<'_> {
    fn accepts(&self, binding: &Binding) -> bool {
        binding.role == Role::Function
            && binding.signature.as_ref().is_some_and(|signature| {
                signature.len() == self.spans.len()
                    && signature
                        .iter()
                        .zip(&self.labels)
                        .all(|(&(label, _), written)| label == *written)
            })
    }
}

pub(super) struct UseRequest {
    pub token: usize,
    pub scope: usize,
    pub call: bool,
}

pub(super) struct SiteResolution {
    pub is_call: bool,
    pub target: Option<usize>,
    pub pending: Option<csharp::Pending>,
}

impl SiteResolution {
    fn call(target: Option<usize>) -> Self {
        Self {
            is_call: true,
            target,
            pending: None,
        }
    }
    fn reference(target: Option<usize>) -> Self {
        Self {
            is_call: false,
            target,
            pending: None,
        }
    }
}

use super::declarations::{C, Cpp, Csharp, Swift};

pub(super) trait CallGrammar {
    const ARGUMENT_LABELS: bool = false;
    fn receiver_const(_a: &Analyzer, _i: usize, _scope: usize) -> Option<bool> {
        None
    }
    fn prefer(_receiver: Option<bool>, _bindings: &mut Vec<&Binding>) {}
    fn deferred(
        _a: &Analyzer,
        _i: usize,
        _scope: usize,
        _bindings: &[&Binding],
    ) -> Option<csharp::Pending> {
        None
    }
}
impl CallGrammar for Swift {
    const ARGUMENT_LABELS: bool = true;
}
impl CallGrammar for Csharp {
    fn deferred(
        a: &Analyzer,
        i: usize,
        scope: usize,
        bindings: &[&Binding],
    ) -> Option<csharp::Pending> {
        a.csharp_deferred(i, scope, a.out.calls.len(), bindings)
    }
}
impl CallGrammar for C {
    fn prefer(_: Option<bool>, candidates: &mut Vec<&Binding>) {
        if candidates.len() > 1 {
            prefer_matching(candidates, |b| b.definition, |b| (&b.signature, b.is_const));
        }
    }
}
impl CallGrammar for Cpp {
    fn receiver_const(a: &Analyzer, i: usize, scope: usize) -> Option<bool> {
        a.receiver_constness(i, scope)
    }
    fn prefer(receiver: Option<bool>, candidates: &mut Vec<&Binding>) {
        C::prefer(receiver, candidates);
        if candidates.len() > 1 {
            match receiver {
                Some(true) => candidates.retain(|b| b.is_const),
                Some(false) => prefer_matching(candidates, |b| !b.is_const, |b| &b.signature),
                None => {}
            }
        }
    }
}

impl<'a> Analyzer<'a, '_> {
    pub(super) fn resolve_use<'s, G: CallGrammar>(
        &'s self,
        request: UseRequest,
        mut candidates: impl Iterator<Item = &'s Binding<'a>>,
        args: &mut Arguments<'a>,
    ) -> Option<SiteResolution> {
        let UseRequest { token: i, scope, call } = request;
        if !call {
            return unique(candidates.filter(|b| b.usable))
                .map(|b| SiteResolution::reference(Some(b.id)));
        }
        let Some(first) = candidates.next() else {
            return Some(SiteResolution::call(None));
        };
        let Some(second) = candidates.next() else {
            return Some(self.single_candidate::<G>(i, first));
        };
        args.spans.clear();
        args.spans.extend(self.parameter_spans(i + 1));
        if G::ARGUMENT_LABELS {
            args.labels.clear();
            args.labels.extend(
                args.spans
                    .iter()
                    .map(|&(lo, hi)| self.argument_label(lo, hi)),
            );
        }
        let viable = [first, second]
            .into_iter()
            .chain(candidates)
            .filter(|b| args.accepts(b))
            .collect();
        Some(self.select_overload::<G>(i, scope, args, viable))
    }

    fn single_candidate<G: CallGrammar>(&self, token: usize, binding: &Binding) -> SiteResolution {
        // Calling a parameter references its lexical binding. Dispatch is dynamic.
        if self.qualifier(token).is_none() && self.out.declarations[binding.id].kind == "parameter" {
            return SiteResolution::reference(binding.usable.then_some(binding.id));
        }
        let matches = binding.usable && self.accepts_call::<G>(binding, token + 1);
        SiteResolution::call(matches.then_some(binding.id))
    }

    /// A single candidate needs no saved ranges. Stop as soon as its arity
    /// or a written Swift label rules it out.
    fn accepts_call<G: CallGrammar>(&self, binding: &Binding, open: usize) -> bool {
        let Some(signature) = &binding.signature else {
            return false;
        };
        let mut args = self.parameter_spans(open);
        if !G::ARGUMENT_LABELS {
            return args.take(signature.len() + 1).count() == signature.len();
        }
        signature.iter().all(|&(label, _)| {
            args.next()
                .is_some_and(|(lo, hi)| self.argument_label(lo, hi) == label)
        }) && args.next().is_none()
    }

    fn argument_label(&self, lo: usize, hi: usize) -> &'a str {
        if lo + 1 < hi && self.word(lo + 1) == ":" {
            self.word(lo)
        } else {
            "_"
        }
    }

    fn receiver_constness(&self, i: usize, scope: usize) -> Option<bool> {
        match self.qualifier(i) {
            Some(q) if self.word(q) == "this" => Some(self.scopes[scope].const_receiver),
            Some(q) => {
                unique(self.lookup(scope, self.word(q), Some(Role::Value), i)).map(|b| b.is_const)
            }
            None => self.scopes[scope]
                .container
                .map(|_| self.scopes[scope].const_receiver),
        }
    }

    fn exact_overload<'b, G: CallGrammar>(
        &self,
        scope: usize,
        args: &mut Arguments<'a>,
        candidates: &[&'b Binding<'a>],
        receiver_const: Option<bool>,
    ) -> Option<&'b Binding<'a>> {
        // Arity has already selected only empty signatures at a zero-argument call.
        if args.spans.is_empty() {
            return None;
        }
        args.types.clear();
        for &(lo, hi) in &args.spans {
            args.types.push(self.arg_type(scope, lo, hi)?);
        }
        // Both preferences operate within a signature. Matching argument types
        // select complete groups, so preferences on other groups cannot change this choice.
        let mut exact = candidates.iter().copied().filter(|b| {
            (receiver_const != Some(true) || b.is_const)
                && b.signature
                    .as_ref()
                    .unwrap()
                    .iter()
                    .zip(&args.types)
                    .all(|((_, ty), arg)| *ty == Some(*arg))
        });
        let first = exact.next()?;
        let Some(second) = exact.next() else {
            return Some(first);
        };
        let mut group: Vec<_> = [first, second].into_iter().chain(exact).collect();
        // The caller applies preferences to the full group when all types match.
        if group.len() == candidates.len() {
            return None;
        }
        G::prefer(receiver_const, &mut group);
        unique(group.into_iter())
    }

    fn select_overload<G: CallGrammar>(
        &self,
        i: usize,
        s: usize,
        args: &mut Arguments<'a>,
        mut candidates: Vec<&Binding<'a>>,
    ) -> SiteResolution {
        if candidates.len() > 1 {
            let receiver_const = G::receiver_const(self, i, s);
            if let Some(binding) = self.exact_overload::<G>(s, args, &candidates, receiver_const) {
                return SiteResolution::call(binding.usable.then_some(binding.id));
            }
            G::prefer(receiver_const, &mut candidates);
        }
        candidates.retain(|b| b.usable);
        let mut result = SiteResolution::call(match candidates.as_slice() {
            [b] => Some(b.id),
            _ => None,
        });
        if candidates.len() > 1 {
            result.pending = G::deferred(self, i, s, &candidates);
        }
        result
    }
}

/// Preserve candidate order. A preference eliminates an alternative only
/// when the preferred declaration has the same signature key.
fn prefer_matching<T: Copy, K: Eq + std::hash::Hash>(
    candidates: &mut Vec<T>,
    preferred: impl Fn(T) -> bool,
    key: impl Fn(T) -> K,
) {
    if candidates.iter().all(|&item| preferred(item)) {
        return;
    }
    let mut keys = candidates
        .iter()
        .copied()
        .filter(|&item| preferred(item))
        .map(&key);
    let Some(first) = keys.next() else { return };
    if let Some(second) = keys.next() {
        let distinct: std::collections::HashSet<_> =
            [first, second].into_iter().chain(keys).collect();
        candidates.retain(|&item| preferred(item) || !distinct.contains(&key(item)));
    } else {
        candidates.retain(|&item| preferred(item) || key(item) != first);
    }
}
