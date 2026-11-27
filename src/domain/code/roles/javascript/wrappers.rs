//! Prove which local functions forward test registration arguments.
use super::{Binding, HashMap, HashSet, Range, Resolved, Scopes, Syntax, arguments, ident};

struct Function<'a> {
    name: &'a str,
    params: Vec<Option<&'a str>>,
    body: Range<usize>,
}

fn function<'a>(s: &'a Syntax, name: &'a str, open: usize) -> Option<Function<'a>> {
    let close = s.close(open)?;
    let mut body = close + 1;
    if s.seq(body, &["=", ">"]) {
        body += 2;
    }
    if s.w(body) != "{" {
        return None;
    }
    let end = s.close(body)?;
    let params = arguments(s, open + 1, close)
        .map(|arg| super::declarations::parameter_name(s, &arg))
        .collect();
    Some(Function {
        name,
        params,
        body: body + 1..end,
    })
}

impl Function<'_> {
    fn formal(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|&param| param == Some(name))
    }

    fn forwarded(&self, syntax: &Syntax, arg: &Range<usize>) -> Option<usize> {
        if arg.len() != 1 {
            return None;
        }
        self.formal(syntax.w(arg.start))
    }

    /// Only calls executed by the wrapper itself are possible forwarding edges.
    fn calls<'a>(&self, syntax: &'a Syntax) -> impl Iterator<Item = (usize, usize)> + 'a {
        let mut body = self.body.clone();
        std::iter::from_fn(move || {
            while body.start < body.end {
                let at = body.start;
                body.start = syntax.close(at).unwrap_or(at) + 1;
                if let Some(close) =
                    super::CallHead::read(syntax, at).and_then(|head| head.direct_end(syntax))
                {
                    body.start = close + 1;
                    return Some((at, close));
                }
            }
            None
        })
    }
}

/// A registered case supplies evidence to every wrapper that forwards its
/// title. Follow outgoing edges once when that evidence first reaches a name.
#[derive(Default)]
pub(super) struct WrapperCalls<'a> {
    callers: HashMap<&'a str, Vec<&'a str>>,
    reached: HashSet<&'a str>,
    pending: Vec<&'a str>,
}

impl<'a> WrapperCalls<'a> {
    fn forwards(&mut self, resolved: Resolved, callee: &'a str, caller: &'a str) {
        if matches!(
            resolved.binding(),
            Some(Binding::Case | Binding::NodeDefault | Binding::PlaywrightCase)
        ) {
            self.reach(caller);
        } else if !matches!(resolved, Resolved::Local(_)) {
            self.callers.entry(callee).or_default().push(caller);
        }
    }

    fn reach(&mut self, caller: &'a str) {
        if self.reached.insert(caller) {
            self.pending.push(caller);
        }
    }

    fn reach_callers(&mut self, callee: &str) {
        for caller in self.callers.remove(callee).unwrap_or_default() {
            self.reach(caller);
        }
    }

    pub(super) fn reaches(&mut self, local: &str) -> bool {
        while !self.reached.contains(local) {
            let Some(callee) = self.pending.pop() else {
                return false;
            };
            self.reach_callers(callee);
        }
        true
    }
}

fn wrapper_alias<'a>(s: &'a Syntax, i: usize) -> Option<&'a str> {
    let target = if s.seq(i + 3, &["Object", ".", "assign", "("]) && s.w(i + 8) == "," {
        i + 7
    } else if matches!(s.w(i + 4), "" | ";") {
        i + 3
    } else {
        return None;
    };
    ident(s, target).then(|| s.w(target))
}

enum Declaration<'a> {
    Wrapper(Function<'a>),
    Alias(&'a str, &'a str),
}

impl<'a> Declaration<'a> {
    fn read(s: &'a Syntax, i: usize) -> Option<Self> {
        let keyword = s.w(i);
        if !matches!(keyword, "function" | "const") || !ident(s, i + 1) {
            return None;
        }
        let name = s.w(i + 1);
        let open = match (keyword, s.w(i + 2)) {
            ("function", "(") => i + 2,
            ("const", "=") if s.w(i + 3) == "(" => i + 3,
            ("const", "=") => return wrapper_alias(s, i).map(|target| Self::Alias(name, target)),
            _ => return None,
        };
        function(s, name, open)
            .filter(|f| !f.params.is_empty())
            .map(Self::Wrapper)
    }
}

fn declared_wrappers<'a>(s: &'a Syntax) -> (Vec<Function<'a>>, HashMap<&'a str, &'a str>) {
    let mut functions = Vec::new();
    let mut aliases = HashMap::new();
    let mut i = 0;
    while i < s.tokens.len() {
        match Declaration::read(s, i) {
            Some(Declaration::Wrapper(f)) => functions.push(f),
            Some(Declaration::Alias(name, target)) => {
                aliases.insert(name, target);
            }
            None => {}
        }
        i = s.close(i).unwrap_or(i) + 1;
    }
    (functions, aliases)
}

// A wrapper must forward a parameter as the test title. Passing an imported
// registrar into a helper counts only if that helper actually invokes the
// corresponding formal parameter as a registrar.
#[derive(Clone, Copy)]
struct Forwarding {
    registrar: usize,
    title: usize,
}

impl Forwarding {
    fn registrar_argument(
        self,
        s: &Syntax,
        caller: &Function,
        args: impl Iterator<Item = Range<usize>>,
    ) -> Option<usize> {
        let mut args = args.enumerate();
        let first = args.nth(self.registrar.min(self.title))?.1;
        let (registrar, title) = match self.registrar.cmp(&self.title) {
            std::cmp::Ordering::Less => (first, args.nth(self.title - self.registrar - 1)?.1),
            std::cmp::Ordering::Equal => (first.clone(), first),
            std::cmp::Ordering::Greater => (args.nth(self.registrar - self.title - 1)?.1, first),
        };
        (registrar.len() == 1 && caller.forwarded(s, &title).is_some()).then_some(registrar.start)
    }
}

pub(super) fn wrapper_calls<'a>(s: &'a Syntax, bindings: &Scopes) -> WrapperCalls<'a> {
    let (functions, aliases) = declared_wrappers(s);
    // Declaration slots are stable while forward calls discover their signatures.
    let invoked: HashMap<_, _> = functions
        .iter()
        .map(|function| (function.name, std::cell::Cell::new(None)))
        .collect();
    let mut forwards = Vec::new();
    let mut flow = WrapperCalls::default();
    let calls = functions.iter().flat_map(|caller| {
        caller.calls(s).filter_map(move |(call, close)| {
            let mut rest = arguments(s, call + 2, close);
            let first = rest.next()?;
            (first.end + 1 < close).then_some((caller, call, first, rest))
        })
    });
    for (caller, call, first, rest) in calls {
        let callee = Registrar::at(s, bindings, call);
        let signature = callee.observe_forwarding(s, &mut flow, caller, &first);
        let slot = &invoked[caller.name];
        slot.set(signature.or_else(|| slot.get()));
        // A later helper declaration can establish which arguments matter.
        // Save the cursor; only consume its tail if that proof exists.
        let target = invoked
            .get(callee.name)
            .filter(|_| !matches!(callee.resolved, Resolved::Local(_)));
        forwards.extend(target.map(|target| (caller, target, first, rest)));
    }
    let forwarded = forwards
        .into_iter()
        .filter_map(|(caller, slot, first, rest)| {
            let args = std::iter::once(first).chain(rest);
            let arg = slot.get()?.registrar_argument(s, caller, args)?;
            Some((caller.name, Registrar::at(s, bindings, arg)))
        });
    for (caller, callee) in forwarded {
        flow.forwards(callee.resolved, callee.name, caller);
    }
    for (alias, target) in aliases {
        flow.forwards(bindings.resolve(0, target), target, alias);
    }
    flow
}

struct Registrar<'a> {
    name: &'a str,
    resolved: Resolved,
}

impl<'a> Registrar<'a> {
    fn at(syntax: &'a Syntax, bindings: &Scopes, index: usize) -> Self {
        let name = syntax.w(index);
        Self {
            name,
            resolved: bindings.resolve(index, name),
        }
    }

    fn observe_forwarding(
        &self,
        syntax: &Syntax,
        flow: &mut WrapperCalls<'a>,
        caller: &Function<'a>,
        title: &Range<usize>,
    ) -> Option<Forwarding> {
        let title = caller.forwarded(syntax, title)?;
        flow.forwards(self.resolved, self.name, caller.name);
        let registrar = caller.formal(self.name)?;
        Some(Forwarding { registrar, title })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_bare_wrapper_and_forwarded_parameter_prove_registration() {
        let src = concat!(
            "import {test} from 'node:test';",
            "function live(name, cb) { test(name, cb); }",
            "function member(name, cb) { obj.live(name, cb); }",
            "function shadow(name, cb, live) { live(name, cb); }",
            "function computed(name, cb) { test(name.length, cb); }",
            "function forwarded(name, cb) { live(name, cb); }",
        );
        let tokens = crate::domain::code::lexer::tokens(src, crate::domain::code::Language::Js);
        let pairs = crate::domain::code::script::paired_for_score(&tokens);
        let syntax = Syntax {
            tokens: &tokens,
            pairs: &pairs,
            text: src,
        };
        let bindings = Scopes::from_names(&syntax, [], true).with_locals();
        let mut wrappers = wrapper_calls(&syntax, &bindings);
        assert!(wrappers.reaches("forwarded"));
        for name in ["member", "shadow", "computed"] {
            assert!(!wrappers.reaches(name));
        }
    }
}
#[cfg(test)]
#[test]
fn lazy_reachability_survives_repeated_queries_and_cycles() {
    let mut flow = WrapperCalls::default();
    flow.callers.insert("root", vec!["a", "b"]);
    flow.callers.insert("a", vec!["b"]);
    flow.callers.insert("b", vec!["a", "c"]);
    flow.callers.insert("isolated", vec!["z"]);
    flow.reach("root");
    for name in ["b", "a", "c", "root", "b"] {
        assert!(flow.reaches(name));
    }
    assert!(!flow.reaches("z"));
    assert!(flow.reaches("a"));
}
