//! File-local pytest and unittest discovery. Logical statements keep continuation
//! lines inside their declaration; class members are resolved before collection.
use super::{Role, Roles, Syntax, arguments, indentation};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatementKind {
    Class,
    Function,
    DisableScope,
    DisableName,
    Other,
}

impl StatementKind {
    fn read(s: &Syntax, first: usize, head: usize) -> Self {
        match s.w(head) {
            "class" => Self::Class,
            "def" => Self::Function,
            "__test__" if s.seq(first + 1, &["=", "False"]) => Self::DisableScope,
            _ if s.seq(first + 1, &[".", "__test__", "=", "False"]) => Self::DisableName,
            _ => Self::Other,
        }
    }
}

struct Statement {
    kind: StatementKind,
    first: usize,
    head: usize,
    decorator_start: usize,
    after: usize,
    indent: usize,
    owner: Option<usize>,
    // Exclusive statement index, including the complete indented suite.
    end: usize,
}

impl Statement {
    fn module_level(&self) -> bool {
        self.owner.is_none() && self.indent == 0
    }
}

struct LogicalStatements<'a, 's> {
    syntax: &'a Syntax<'s>,
    first: usize,
}

impl LogicalStatements<'_, '_> {
    fn after(&self) -> usize {
        let s = self.syntax;
        let mut token = self.first;
        loop {
            let last = s.close(token).unwrap_or(token);
            token = last + 1;
            let continues = s
                .tokens
                .get(token)
                .is_some_and(|next| next.line == s.tokens[last].line || s.w(last) == "\\");
            if !continues {
                return token;
            }
        }
    }
}

impl Iterator for LogicalStatements<'_, '_> {
    type Item = Statement;

    fn next(&mut self) -> Option<Self::Item> {
        let first = self.first;
        let s = self.syntax;
        let token = s.tokens.get(first)?;
        let after = self.after();
        self.first = after;
        let head = first + usize::from(s.w(first) == "async");
        Some(Statement {
            kind: StatementKind::read(s, first, head),
            first,
            head,
            after,
            indent: indentation(s.text, token.span.start),
            decorator_start: 0,
            end: 0,
            owner: None,
        })
    }
}

#[derive(Default)]
struct StatementTree {
    list: Vec<Statement>,
    blocks: Vec<usize>,
}

impl StatementTree {
    fn insert(&mut self, s: &Syntax, mut statement: Statement) {
        let index = self.list.len();
        while self
            .blocks
            .last()
            .is_some_and(|&i| self.list[i].indent >= statement.indent)
        {
            self.list[self.blocks.pop().unwrap()].end = index;
        }
        statement.owner = self.blocks.last().copied();
        statement.decorator_start = self
            .list
            .last()
            .filter(|previous| previous.indent == statement.indent && s.w(previous.first) == "@")
            .map_or(index, |previous| previous.decorator_start);
        if matches!(
            statement.kind,
            StatementKind::Class | StatementKind::Function
        ) {
            self.blocks.push(index);
        }
        self.list.push(statement);
    }

    fn finish(mut self) -> Vec<Statement> {
        let eof = self.list.len();
        for i in self.blocks {
            self.list[i].end = eof;
        }
        self.list
    }
}

fn statements(s: &Syntax) -> Vec<Statement> {
    let mut tree = StatementTree::default();
    let logical = LogicalStatements {
        syntax: s,
        first: 0,
    };
    for statement in logical {
        tree.insert(s, statement);
    }
    tree.finish()
}

#[derive(Clone, Copy)]
enum Binding {
    Pytest,
    Fixture,
    Unittest(UnittestModule),
    Case,
    Class(Traits),
    Other,
}

#[derive(Default)]
struct Imports<'a>(HashMap<&'a str, Binding>);

fn fixture_name(name: &str) -> bool {
    matches!(name, "fixture" | "yield_fixture")
}

#[derive(Clone, Copy)]
enum UnittestModule {
    Root,
    Case,
    AsyncCase,
}

impl UnittestModule {
    fn named(path: &str) -> Option<Self> {
        match path {
            "unittest" => Some(Self::Root),
            "unittest.case" => Some(Self::Case),
            "unittest.async_case" => Some(Self::AsyncCase),
            _ => None,
        }
    }

    fn member(self, name: &str) -> Option<Binding> {
        match (self, name) {
            (_, "TestCase")
            | (Self::Root | Self::Case, "FunctionTestCase")
            | (Self::Root | Self::AsyncCase, "IsolatedAsyncioTestCase") => Some(Binding::Case),
            (Self::Root, "case") => Some(Binding::Unittest(Self::Case)),
            (Self::Root, "async_case") => Some(Binding::Unittest(Self::AsyncCase)),
            _ => None,
        }
    }
}

/// Identify the imported spelling and the local spelling it binds to. A
/// submodule imported under an alias does not bind the framework root.
fn framework_binding<'a>(
    tokens: &[super::Token<'a>],
    module_import: bool,
) -> Option<(&'a str, &'a str)> {
    let mut cursor = usize::from(tokens.first().is_some_and(|token| token.text == "\\"));
    if !module_import {
        let (name, local) = super::script::python_import_member(tokens, &mut cursor)?;
        return Some((tokens[name].text, tokens[local].text));
    }
    let (written, local) = super::script::python_import_module(tokens, &mut cursor)?;
    let root = tokens[written.start].text;
    let local_name = local.map_or(root, |i| tokens[i].text);
    Some((imported_module(&tokens[written], local.is_some()), local_name))
}

// With no alias, importing a submodule binds only its top-level package.
fn imported_module<'a>(tokens: &[super::Token<'a>], aliased: bool) -> &'a str {
    let root = tokens[0].text;
    if !aliased || tokens.len() == 1 {
        return root;
    }
    let [_, _, submodule] = tokens else { return "" };
    match (root, submodule.text) {
        ("unittest", "case") => "unittest.case",
        ("unittest", "async_case") => "unittest.async_case",
        _ => "",
    }
}

fn import_clause(s: &Syntax, stmt: &Statement) -> Option<(String, usize)> {
    let first = stmt.first;
    match s.w(first) {
        "import" => Some((String::new(), first)),
        "from" => {
            let import = (first + 1..stmt.after).find(|&i| s.w(i) == "import")?;
            let path = s.tokens[first + 1..import].iter().map(|t| t.text).collect();
            Some((path, import))
        }
        _ => None,
    }
}

impl<'a> Imports<'a> {
    fn observe(&mut self, s: &'a Syntax<'a>, stmt: &Statement) {
        let first = stmt.first;
        if s.w(first + 1) == "=" && s.w(first + 2) != "=" {
            self.shadow(s.w(first));
            return;
        }
        let Some((module, import)) = import_clause(s, stmt) else { return };
        let start = import + 1;
        let (first, after) = s
            .close(start)
            .map_or((start, stmt.after), |end| (start + 1, end));
        for arg in arguments(s, first, after) {
            let tokens = &s.tokens[arg];
            let Some((name, local)) = framework_binding(tokens, module.is_empty()) else {
                continue;
            };
            self.register(&module, name, local);
        }
    }

    fn shadow(&mut self, name: &'a str) {
        if name == "pytest" {
            self.0.insert(name, Binding::Other);
        } else {
            self.0.remove(name);
        }
    }

    fn register(&mut self, module: &str, name: &str, local: &'a str) {
        let binding = match (module, name) {
            ("", "pytest") => Some(Binding::Pytest),
            ("pytest", name) if fixture_name(name) => Some(Binding::Fixture),
            ("", name) => UnittestModule::named(name).map(Binding::Unittest),
            _ => UnittestModule::named(module).and_then(|api| api.member(name)),
        };
        if let Some(binding) = binding {
            self.0.insert(local, binding);
        } else {
            self.shadow(local);
        }
    }

    fn fixture(&self, s: &Syntax, pos: usize) -> bool {
        match self.0.get(s.w(pos)) {
            Some(Binding::Fixture) => true,
            binding => {
                let module = matches!(binding, Some(Binding::Pytest))
                    || binding.is_none() && s.w(pos) == "pytest";
                module && s.w(pos + 1) == "." && fixture_name(s.w(pos + 2))
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Traits {
    unittest: bool,
    no_pytest: bool,
    custom_init: bool,
    named_tests: bool,
}

#[derive(Clone, Copy, Default)]
struct Class {
    traits: Traits,
    collected: bool,
}

impl Traits {
    fn merge(&mut self, base: Self) {
        self.unittest |= base.unittest;
        self.custom_init |= base.custom_init;
        self.no_pytest |= base.no_pytest;
        self.named_tests |= base.named_tests;
    }

    fn discoverable(&self, pytest_file: bool, name: &str) -> bool {
        !self.no_pytest
            && (self.unittest || (pytest_file && name.starts_with("Test") && !self.custom_init))
    }

    fn inherit(&mut self, s: &Syntax, head: usize, imports: &Imports) {
        // Recognize verified unittest imports and locally declared bases.
        let open = head + 2;
        if s.w(open) != "(" {
            return;
        }
        let Some(close) = s.close(open) else { return };
        for base in arguments(s, open + 1, close) {
            let mut base = base;
            while s.w(base.start) == "(" && s.close(base.start) == Some(base.end - 1) {
                base = base.start + 1..base.end - 1;
            }
            self.merge(inherited_traits(&s.tokens[base], imports));
        }
    }
}

fn inherited_traits(base: &[super::Token], imports: &Imports) -> Traits {
    let Some((name, suffix)) = base.split_first() else {
        return Traits::default();
    };
    let binding = match (imports.0.get(name.text), suffix) {
        (binding, []) => binding.copied(),
        (Some(Binding::Unittest(api)), [dot, member]) if dot.text == "." => api.member(member.text),
        _ => None,
    };
    match binding {
        Some(Binding::Class(traits)) => traits,
        Some(Binding::Case) => Traits {
            unittest: true,
            ..Traits::default()
        },
        _ => Traits::default(),
    }
}

struct Discovery<'a> {
    imports: Imports<'a>,
    classes: HashMap<usize, Class>,
    hidden: HashSet<(Option<usize>, &'a str)>,
    disabled_module: bool,
    active_tests: HashSet<usize>,
    pytest_file: bool,
    module_hooks: bool,
}

impl<'a> Discovery<'a> {
    fn class(&mut self, s: &'a Syntax<'a>, st: &Statement, i: usize, enabled: bool) -> bool {
        let name = s.w(st.head + 1);
        let class = self
            .classes
            .get_mut(&i)
            .expect("class header recorded by discovery");
        class.traits.inherit(s, st.head, &self.imports);
        class.collected = enabled && class.traits.discoverable(self.pytest_file, name);
        if st.owner.is_none() {
            self.imports.0.insert(name, Binding::Class(class.traits));
        }
        class.collected
    }

    // An opt-out or constructor may follow the test declarations it affects.
    fn observe_statement(&mut self, s: &'a Syntax, st: &Statement, i: usize) {
        match st.kind {
            StatementKind::Class => {
                self.classes.insert(i, Class::default());
            }
            StatementKind::DisableScope => self.disable_scope(st),
            StatementKind::DisableName => {
                self.hidden.insert((st.owner, s.w(st.first)));
            }
            _ => {}
        }
    }

    fn disable_scope(&mut self, st: &Statement) {
        if let Some(class) = st.owner.and_then(|n| self.classes.get_mut(&n)) {
            class.traits.no_pytest = true;
        } else if st.module_level() {
            self.disabled_module = true;
        }
    }

    fn observe_method(
        &mut self,
        s: &'a Syntax,
        st: &Statement,
        pending: &mut Vec<(usize, &'a str)>,
    ) {
        let Some(owner) = st.owner else { return };
        let name = s.w(st.head + 1);
        if st.first == st.head && matches!(name, "__init__" | "__new__") {
            if let Some(class) = self.classes.get_mut(&owner) {
                class.traits.custom_init = true;
            }
        } else if name.starts_with("test") && self.classes.contains_key(&owner) {
            pending.push((owner, name));
        }
    }

    fn read(s: &'a Syntax<'a>, list: &[Statement], filename: &str) -> Self {
        let pytest_file = filename.ends_with(".py")
            && (filename.starts_with("test_") || filename.ends_with("_test.py"));
        let mut discovery = Self {
            imports: Imports::default(),
            classes: HashMap::new(),
            hidden: HashSet::new(),
            disabled_module: false,
            active_tests: HashSet::new(),
            pytest_file,
            module_hooks: pytest_file || filename == "conftest.py",
        };
        let mut methods = Vec::new();
        for (i, st) in list.iter().enumerate() {
            discovery.observe_statement(s, st, i);
            if st.kind == StatementKind::Function {
                discovery.observe_method(s, st, &mut methods);
            }
        }
        // A later opt-out affects whether runTest is the class's only test.
        for (owner, _) in methods
            .into_iter()
            .filter(|&(owner, name)| !discovery.hidden.contains(&(Some(owner), name)))
        {
            discovery
                .classes
                .get_mut(&owner)
                .expect("method of a recorded class")
                .traits
                .named_tests = true;
        }
        discovery
    }
}

enum Parent {
    Module,
    Class(Class),
    Function,
}

impl Parent {
    fn from_statement(st: &Statement, classes: &HashMap<usize, Class>) -> Self {
        match st.owner {
            None => Self::Module,
            Some(i) => classes.get(&i).copied().map_or(Self::Function, Self::Class),
        }
    }

    fn permits_collection(&self, module: bool) -> bool {
        match self {
            Self::Module => module,
            Self::Class(c) => c.collected,
            Self::Function => false,
        }
    }

    fn test_method(&self, name: &str, module: bool) -> bool {
        if !self.permits_collection(module) {
            return false;
        }
        name.starts_with("test")
            || (name == "runTest"
                && matches!(self, Self::Class(c) if c.traits.unittest && !c.traits.named_tests))
    }
}

fn module_hook(name: &str) -> bool {
    matches!(
        name,
        "setup_module"
            | "teardown_module"
            | "setup_function"
            | "teardown_function"
            | "setUpModule"
            | "tearDownModule"
    )
}

struct RoleCandidate<'a> {
    name: &'a str,
    parent: Parent,
    enabled: bool,
    inside_test: bool,
}

impl<'a> Discovery<'a> {
    fn candidate(&self, s: &'a Syntax<'a>, statement: &Statement) -> RoleCandidate<'a> {
        let name = s.w(statement.head + 1);
        RoleCandidate {
            name,
            parent: Parent::from_statement(statement, &self.classes),
            enabled: !self.disabled_module && !self.hidden.contains(&(statement.owner, name)),
            inside_test: statement
                .owner
                .is_some_and(|owner| self.active_tests.contains(&owner)),
        }
    }

    fn class_role(
        &mut self,
        s: &'a Syntax<'a>,
        st: &Statement,
        i: usize,
        candidate: RoleCandidate,
    ) -> Option<super::Evidence> {
        if self.class(
            s,
            st,
            i,
            candidate.enabled && candidate.parent.permits_collection(true),
        ) {
            return Some(Role::Support.evidence("test class"));
        }
        candidate
            .inside_test
            .then_some(Role::Support.evidence("class local to a test"))
    }

    fn role(&mut self, s: &'a Syntax<'a>, list: &[Statement], i: usize) -> Option<super::Evidence> {
        let st = &list[i];
        let candidate = self.candidate(s, st);
        if st.kind == StatementKind::Class {
            return self.class_role(s, st, i, candidate);
        }
        if (st.decorator_start..i).any(|j| self.imports.fixture(s, list[j].first + 1)) {
            return Some(Role::Support.evidence("pytest fixture"));
        }
        self.function_role(st, i, candidate)
    }

    fn function_role(
        &mut self,
        st: &Statement,
        i: usize,
        candidate: RoleCandidate,
    ) -> Option<super::Evidence> {
        let RoleCandidate {
            name,
            parent,
            enabled,
            inside_test,
        } = candidate;
        let module_test = self.pytest_file && st.indent == 0;
        if enabled && parent.test_method(name, module_test) {
            self.active_tests.insert(i);
            return Some(Role::Test.evidence("test discovery function"));
        }
        let hook = self.module_hooks && st.owner.is_none() && module_hook(name);
        (inside_test || hook || parent.permits_collection(false))
            .then_some(Role::Support.evidence("test helper or lifecycle method"))
    }

    fn visit(&mut self, s: &'a Syntax<'a>, list: &[Statement], i: usize, res: &mut Roles) {
        let st = &list[i];
        let module = st.module_level();
        if st.end == 0 {
            if module {
                self.imports.observe(s, st);
            }
            return;
        }
        if let Some(evidence) = self.role(s, list, i) {
            let first = list[st.decorator_start].first;
            let last = list[st.end - 1].after - 1;
            s.add(res, first, Some(last), evidence);
        }
        if module && st.kind == StatementKind::Function {
            self.imports.shadow(s.w(st.head + 1));
        }
    }
}

pub(super) fn collect(path: &str, s: &Syntax, res: &mut Roles) {
    let list = statements(s);
    let filename = path.rsplit('/').next().unwrap_or(path);
    let mut discovery = Discovery::read(s, &list, filename);
    for i in 0..list.len() {
        discovery.visit(s, &list, i, res);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Role, classify};

    fn roles(path: &str, text: &str, markers: &[(&str, Role)]) {
        let result = classify(path, text);
        for &(marker, role) in markers {
            let pos = text.find(marker).unwrap();
            let got = result
                .regions
                .iter()
                .rev()
                .find(|r| r.start <= pos && pos < r.end)
                .map_or(result.base.0, |r| r.evidence.0);
            assert_eq!(got, role, "{marker}");
        }
    }

    #[test]
    fn module_class_and_local_discovery() {
        use Role::*;
        roles(
            "tests/test_logic.py",
            "class TestMath:\n    def test_add(self):\n        def test_nested(): pass\n        assert 1\nclass TestCtor:\n    def test_before(self): pass\n    def __init__(self): pass\nclass TestDisabled:\n    def test_also(self): pass\n    __test__ = False\ndef test_gone(): pass\ntest_gone.__test__ = False\n",
            &[
                ("test_add", Test),
                ("test_nested", Support),
                ("assert 1", Test),
                ("test_before", Support),
                ("test_also", Support),
                ("def test_gone", Support),
            ],
        );
    }

    #[test]
    fn class_bases_use_the_binding_present_at_the_declaration() {
        use Role::*;
        let text = "class Base: pass\nclass Earlier(Base):\n    def test_old(self): pass\nfrom unittest import TestCase as Base\nclass Current(Base):\n    def test_real(self): pass\nBase = object\nclass Later(Base):\n    def test_new(self): pass\n";
        roles(
            "src/logic.py",
            text,
            &[
                ("test_old", Production),
                ("test_real", Test),
                ("test_new", Production),
            ],
        );
    }

    #[test]
    fn unittest_runs_fallback_only_without_named_tests() {
        use Role::*;
        roles(
            "src/logic.py",
            "import unittest as ut\nclass Fallback(ut.TestCase):\n    def runTest(self): expected()\nclass Named(ut.TestCase):\n    def test_active(self): pass\n    def runTest(self): helper()\n",
            &[
                ("expected()", Test),
                ("test_active", Test),
                ("helper()", Support),
            ],
        );
    }

    #[test]
    fn require_the_imported_framework_module() {
        use Role::*;
        roles(
            "src/client.py",
            "from pytest.tools import fixture as other\nfrom unittest.mock import TestCase as Fake\nfrom unittest.async_case import IsolatedAsyncioTestCase as Async\n@other\ndef ordinary(): pass\nclass TestFake(Fake):\n    def test_fake(self): pass\nclass Live(Async):\n    def test_live(self): pass\n",
            &[
                ("ordinary", Production),
                ("test_fake", Production),
                ("test_live", Test),
            ],
        );
    }

    #[test]
    fn unittest_submodules_export_distinct_base_classes() {
        use Role::*;
        let source = concat!(
            "import unittest.async_case as api\n",
            "class First(api.IsolatedAsyncioTestCase):\n    def test_async(self): pass\n",
            "from unittest import case as api\n",
            "class Second(api.FunctionTestCase):\n    def test_function(self): pass\n",
            "class Mistake(api.IsolatedAsyncioTestCase):\n    def test_unavailable(self): pass\n",
        );
        roles(
            "src/checks.py",
            source,
            &[
                ("test_async", Test),
                ("test_function", Test),
                ("test_unavailable", Production),
            ],
        );
    }

    #[test]
    fn only_complete_positional_bases_inherit_test_roles() {
        use Role::*;
        roles(
            "library.py",
            concat!(
                "from unittest import TestCase as Case\n",
                "class Real(object, Case):\n    def test_real(self): pass\n",
                "class Foreign(Ordinary.Case):\n    def test_foreign(self): pass\n",
                "class Generic(list[Case]):\n    def test_generic(self): pass\n",
                "class Meta(metaclass=type(Case)):\n    def test_meta(self): pass\n",
            ),
            &[
                ("test_real", Test),
                ("test_foreign", Production),
                ("test_generic", Production),
                ("test_meta", Production),
            ],
        );
    }

    #[test]
    fn aliases_and_fixtures_in_mixed_production_source() {
        use Role::*;
        roles(
            "src/client.py",
            "from unittest import TestCase as Base\nfrom pytest import fixture as provide\nclass ClientTests(Base):\n    def setUp(self): pass\n    async def test_user(self): pass\n@provide(scope='module')\ndef make_client(): pass\nclass TestProduction:\n    def test_mode(self): pass\n",
            &[
                ("setUp", Support),
                ("test_user", Test),
                ("make_client", Support),
                ("test_mode", Production),
            ],
        );
    }
}
