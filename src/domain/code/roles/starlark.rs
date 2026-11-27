//! Resolve literal Starlark test lists passed to a loaded analysis test suite.
use super::{Role, Roles, Syntax, indentation};
use std::collections::{BTreeMap, HashMap, HashSet};

fn string<'a>(s: &'a Syntax, i: usize) -> &'a str {
    s.w(i).trim_matches(['\'', '"'])
}

fn list<'a>(s: &'a Syntax, open: usize) -> Option<Vec<&'a str>> {
    let end = s.close(open)?;
    if s.w(open) != "[" {
        return None;
    }
    s.tokens[open + 1..end]
        .chunks(2)
        .map(|pair| {
            let name = &pair[0];
            (name.kind == super::Kind::Word && pair.get(1).is_none_or(|t| t.text == ","))
                .then_some(name.text)
        })
        .collect()
}

fn loaded_runners<'a>(
    s: &'a Syntax,
    i: usize,
    suite_names: &mut HashSet<&'a str>,
    makers: &mut HashSet<&'a str>,
) {
    let module = string(s, i + 2);
    let Some(end) = s.close(i + 1) else { return };
    let (locals, supported): (&mut HashSet<&'a str>, &[&str]) = match module {
        "@rules_testing//lib:analysis_test.bzl" | "@rules_testing//lib:test_suite.bzl" => {
            (suite_names, &["test_suite"])
        }
        name if name.ends_with("//lib:unittest.bzl") => (makers, &["unittest", "analysistest"]),
        _ => return,
    };
    let names = (i + 3..end).filter(|&j| s.string(j)).filter_map(|j| {
        let original = string(s, j);
        supported.contains(&original).then(|| match s.w(j - 1) {
            "=" => s.w(j - 2),
            _ => original,
        })
    });
    locals.extend(names);
}

struct Function {
    start: usize,
    last: usize,
}

#[derive(Default)]
struct Members<'a> {
    requests: BTreeMap<usize, &'a str>,
    variables: HashSet<&'a str>,
    registered: HashSet<&'a str>,
}

impl<'a> Members<'a> {
    fn add_suite(&mut self, s: &'a Syntax, i: usize) {
        let Some(end) = s.close(i + 1) else { return };
        let lists = super::arguments(s, i + 2, end)
            .map(|arg| arg.start)
            .filter(|&arg| s.seq(arg, &["basic_tests", "="]));
        for key in lists {
            self.add_members(s, key + 2);
        }
    }

    fn add_members(&mut self, s: &'a Syntax, value: usize) {
        if let Some(names) = list(s, value) {
            self.registered.extend(names);
        } else if matches!(s.w(value + 1), "," | ")") {
            self.requests.insert(value, s.w(value));
            self.variables.insert(s.w(value));
        }
    }

    fn resolve_function(&mut self, s: &'a Syntax, def: &Function, globals: &Lists<'a>) {
        if self.requests.range(def.start..=def.last).next().is_none() {
            return;
        }
        let Some(close) = s.close(def.start + 2) else {
            return;
        };
        let body = (close + 2).min(def.last);
        let mut local = ListScope::new(globals, indentation(s.text, s.tokens[body].span.start));
        for arg in super::arguments(s, def.start + 3, close) {
            local.lists.insert(s.w(arg.start), None);
        }
        local.observe_requests(s, body..def.last + 1, self);
    }

    fn resolve(mut self, s: &'a Syntax, functions: &[Function]) -> HashSet<&'a str> {
        if self.requests.is_empty() {
            return self.registered;
        }
        // Module expressions observe their lists in source order. Functions
        // execute with the module initialized, but own their parameters and locals.
        let empty = Lists::new();
        let mut module = ListScope::new(&empty, 0);
        let mut cursor = 0;
        for def in functions {
            module.read_module(s, cursor..def.start, &mut self);
            cursor = def.last + 1;
        }
        module.read_module(s, cursor..s.tokens.len(), &mut self);
        for def in functions {
            self.resolve_function(s, def, &module.lists);
        }
        self.registered
    }
}

type Lists<'a> = HashMap<&'a str, Option<Vec<&'a str>>>;

/// Read the first token of each physical line, including when a requested scope
/// starts or ends partway through a line.
fn line_heads<'a>(
    tokens: &'a [super::Token],
    span: std::ops::Range<usize>,
) -> impl Iterator<Item = usize> + 'a {
    let end = span.end;
    let next_line = move |i: usize| {
        i + 1 + crate::domain::prefix_len(&tokens[i + 1..end], |t| t.line == tokens[i].line)
    };
    let mut i = span.start;
    if i < end && continues_line(tokens, i) {
        i = next_line(i);
    }
    std::iter::from_fn(move || {
        if i == end {
            return None;
        }
        let head = i;
        i = next_line(i);
        Some(head)
    })
}

fn continues_line(tokens: &[super::Token], i: usize) -> bool {
    i != 0 && tokens[i - 1].line == tokens[i].line
}

struct ListScope<'a, 'global> {
    lists: Lists<'a>,
    global: &'global Lists<'a>,
    indent: usize,
}

impl<'a, 'global> ListScope<'a, 'global> {
    fn new(global: &'global Lists<'a>, indent: usize) -> Self {
        Self {
            lists: Lists::new(),
            global,
            indent,
        }
    }

    fn read_module(
        &mut self,
        s: &'a Syntax,
        span: std::ops::Range<usize>,
        members: &mut Members<'a>,
    ) {
        // Module writes after the final suite can still supply a later function.
        let tail = self.observe_requests(s, span, members);
        self.mutations(s, tail, &members.variables);
    }

    fn observe_requests(
        &mut self,
        s: &'a Syntax,
        span: std::ops::Range<usize>,
        members: &mut Members<'a>,
    ) -> std::ops::Range<usize> {
        let mut cursor = span.start;
        for (&at, &name) in members.requests.range(span.clone()) {
            self.mutations(s, cursor..at, &members.variables);
            let values = self.lists.get(name).or_else(|| self.global.get(name));
            members
                .registered
                .extend(values.and_then(Option::as_deref).unwrap_or_default());
            cursor = at + 1;
        }
        cursor..span.end
    }

    fn mutations(
        &mut self,
        s: &'a Syntax,
        span: std::ops::Range<usize>,
        variables: &HashSet<&str>,
    ) {
        let changes = line_heads(s.tokens, span)
            .filter(|&i| variables.contains(s.w(i)))
            .filter_map(|i| Some((i, ListChange::read(s, i)?)));
        for (i, operation) in changes {
            self.mutate(s, i, operation);
        }
    }
    fn mutate(&mut self, s: &'a Syntax, at: usize, operation: ListChange) {
        let token = &s.tokens[at];
        if indentation(s.text, token.span.start) != self.indent {
            self.lists.insert(token.text, None);
            return;
        }
        operation.apply(s, at, &mut self.lists);
    }
}

enum ListChange {
    Assign,
    Append,
    Extend,
}

impl ListChange {
    fn read(s: &Syntax, at: usize) -> Option<Self> {
        match (s.w(at + 1), s.w(at + 2)) {
            ("=", _) => Some(Self::Assign),
            (".", "append") => Some(Self::Append),
            (".", "extend") => Some(Self::Extend),
            _ => None,
        }
    }

    fn apply<'a>(self, s: &'a Syntax, i: usize, lists: &mut Lists<'a>) {
        let name = s.w(i);
        if matches!(self, Self::Assign) {
            lists.insert(name, list(s, i + 2));
            return;
        }
        let Some(Some(values)) = lists.get_mut(name) else {
            return;
        };
        if s.w(i + 3) != "(" {
            return;
        }
        match self {
            Self::Append if s.w(i + 5) == ")" => values.push(s.w(i + 4)),
            Self::Extend => {
                if let Some(entries) = list(s, i + 4) {
                    values.extend(entries);
                } else {
                    lists.insert(name, None);
                }
            }
            _ => {}
        }
    }
}

fn native_test_target(s: &Syntax, i: usize, name: &str) -> bool {
    if i > 0 && s.w(i - 1) == "def" {
        return false;
    }
    name.ends_with("_test") && s.seq(i + 1, &["(", "name", "="])
}

#[derive(Default)]
struct Registrations<'a> {
    suites: HashSet<&'a str>,
    makers: HashSet<&'a str>,
    declarations: Vec<usize>,
}

impl<'a> Registrations<'a> {
    fn discover(s: &'a Syntax, res: &mut Roles) -> Self {
        let mut runners = Self::default();
        for i in 0..s.tokens.len() {
            runners.observe(s, i, res);
        }
        runners
    }

    fn observe(&mut self, s: &'a Syntax, i: usize, res: &mut Roles) {
        let token = &s.tokens[i];
        if token.text == "def" && indentation(s.text, token.span.start) == 0 {
            self.declarations.push(i);
            return;
        }
        if s.seq(i, &["load", "("]) {
            loaded_runners(s, i, &mut self.suites, &mut self.makers);
            return;
        }
        if native_test_target(s, i, token.text) {
            s.add(
                res,
                i,
                s.close(i + 1),
                Role::Test.evidence("Bazel test target"),
            );
        }
    }

    fn record_call(&self, s: &'a Syntax, members: &mut Members<'a>, call: usize) {
        let name = s.w(call);
        if s.w(call.wrapping_sub(1)) != "." {
            if self.suites.contains(name) {
                members.add_suite(s, call);
            }
            return;
        }
        // A maker is a member call on a bare, loaded receiver.
        if name != "make" || !matches!(s.w(call + 3), "," | ")") {
            return;
        }
        if s.w(call.wrapping_sub(3)) != "." && self.makers.contains(s.w(call.wrapping_sub(2))) {
            members.registered.insert(s.w(call + 2));
        }
    }

    fn implementations(&self, s: &'a Syntax, res: &mut Roles) {
        let mut members = Members::default();
        for call in (0..s.tokens.len()).filter(|&i| s.w(i + 1) == "(") {
            self.record_call(s, &mut members, call);
        }
        // A suite may occur before its list is populated. Only its requested
        // names need file-wide assignment, append, extend and invalidation.
        let functions: Vec<_> = self
            .declarations
            .iter()
            .copied()
            .filter(|&start| {
                !members.requests.is_empty() || members.registered.contains(s.w(start + 1))
            })
            .map(|start| Function {
                start,
                last: s.indented(start),
            })
            .collect();
        let registered = members.resolve(s, &functions);
        for def in functions {
            if registered.contains(s.w(def.start + 1)) {
                s.add(
                    res,
                    def.start,
                    Some(def.last),
                    Role::Test.evidence("registered Starlark test implementation"),
                );
            }
        }
    }
}

pub(super) fn collect(s: &Syntax, res: &mut Roles) {
    let runners = Registrations::discover(s, res);
    if runners.declarations.is_empty() {
        return;
    }
    if !runners.suites.is_empty() || !runners.makers.is_empty() {
        runners.implementations(s, res);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_suite_module_registers_appended_callbacks() {
        let src = r#"load("@rules_testing//lib:test_suite.bzl", suite = "test_suite")
cases = []
def callback(env):
    pass
cases.append(callback)
def collect(name):
    suite(name=name, basic_tests=cases)
"#;
        let roles = crate::domain::code::roles::classify("checks.bzl", src);
        assert!(
            roles
                .regions
                .iter()
                .any(|r| r.first == 3 && r.evidence.0 == Role::Test)
        );
        let unrelated = src.replace("@rules_testing", "@my_service");
        assert!(
            crate::domain::code::roles::classify("checks.bzl", &unrelated)
                .regions
                .is_empty()
        );
    }

    #[test]
    fn a_suite_observes_its_local_list_at_the_call() {
        let src = r#"load("@rules_testing//lib:analysis_test.bzl", suite = "test_suite")
def first(env): pass
def second(env): pass
items = [second]
def make(name):
    items = [first]
    suite(name=name, basic_tests=items)
    items.append(second)
"#;
        let actual = crate::domain::code::roles::classify("checks.bzl", src);
        assert_eq!(
            actual.regions.iter().map(|r| r.first).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn list_membership_requires_registration() {
        let src = r#"load("@rules_testing//lib:analysis_test.bzl", suite = "test_suite")
def first(env):
    env.expect.that_int(1).equals(1)
def second(env):
    pass
def decoy(env):
    pass
ignored = [decoy]
items = [first]
items.append(second)
def make_suite(name):
    suite(name=name, basic_tests=items)
"#;
        let actual = crate::domain::code::roles::classify("checks.bzl", src);
        let roles: Vec<_> = actual
            .regions
            .iter()
            .map(|r| (r.first, r.evidence.0))
            .collect();
        assert_eq!(roles, vec![(2, Role::Test), (4, Role::Test)]);
    }
}
