//! File-local pytest and unittest discovery. Logical statements keep continuation
//! lines inside their declaration; class members are resolved before collection.
use super::{Role, Roles, Syntax, indentation};
use std::collections::{HashMap, HashSet};

struct Statement {
    first: usize,
    after: usize,
    indent: usize,
    owner: Option<usize>,
    // Exclusive statement index, including the complete indented suite.
    end: usize,
}

fn statements(s: &Syntax) -> Vec<Statement> {
    let mut list: Vec<Statement> = Vec::new();
    let mut blocks: Vec<usize> = Vec::new();
    let mut first = 0;
    while first < s.tokens.len() {
        let mut last = first;
        while last + 1 < s.tokens.len() {
            if let Some(close) = s.close(last) {
                last = close;
            }
            if last + 1 == s.tokens.len()
                || (s.tokens[last + 1].line > s.tokens[last].line && s.w(last) != "\\")
            {
                break;
            }
            last += 1;
        }
        let indent = indentation(s.text, s.tokens[first].span.start);
        let index = list.len();
        while blocks.last().is_some_and(|&i| list[i].indent >= indent) {
            list[blocks.pop().unwrap()].end = index;
        }
        let head = first + usize::from(s.w(first) == "async");
        list.push(Statement {
            first,
            after: last + 1,
            indent,
            end: 0,
            owner: blocks.last().copied(),
        });
        if matches!(s.w(head), "def" | "class") {
            blocks.push(index);
        }
        first = last + 1;
    }
    let eof = list.len();
    for i in blocks {
        list[i].end = eof;
    }
    list
}

#[derive(Default)]
struct Imports<'a> {
    pytest: HashSet<&'a str>,
    fixtures: HashSet<&'a str>,
    unittest: HashSet<&'a str>,
    cases: HashSet<&'a str>,
}

impl<'a> Imports<'a> {
    fn observe(&mut self, s: &Syntax<'a>, stmt: &Statement) {
        let t = &s.tokens[stmt.first..stmt.after];
        if t.len() < 2 {
            return;
        }
        let from = t[0].text == "from";
        if !from && t[0].text != "import" {
            return;
        }
        let module = t[1].text;
        for (i, token) in t.iter().enumerate().skip(1) {
            if !matches!(t[i - 1].text, "import" | "(" | ",") {
                continue;
            }
            let alias = if t.get(i + 1).is_some_and(|t| t.text == "as") {
                t.get(i + 2).map_or(token.text, |t| t.text)
            } else {
                token.text
            };
            match (from, module, token.text) {
                (false, _, "pytest") => {
                    self.pytest.insert(alias);
                }
                (false, _, "unittest") => {
                    self.unittest.insert(alias);
                }
                (true, "pytest", "fixture" | "yield_fixture") => {
                    self.fixtures.insert(alias);
                }
                (true, "unittest", "TestCase" | "IsolatedAsyncioTestCase" | "FunctionTestCase") => {
                    self.cases.insert(alias);
                }
                _ => {}
            }
        }
    }

    fn fixture(&self, s: &Syntax, pos: usize) -> bool {
        self.fixtures.contains(s.w(pos))
            || ((s.w(pos) == "pytest" || self.pytest.contains(s.w(pos)))
                && s.w(pos + 1) == "."
                && matches!(s.w(pos + 2), "fixture" | "yield_fixture"))
    }
}

#[derive(Clone, Copy, Default)]
struct Class {
    unittest: bool,
    collected: bool,
    no_pytest: bool,
    custom_init: bool,
    named_tests: bool,
}

pub(super) fn collect(path: &str, s: &Syntax, res: &mut Roles) {
    let list = statements(s);
    let filename = path.rsplit('/').next().unwrap_or(path);
    let pytest_file = filename.ends_with(".py")
        && (filename.starts_with("test_") || filename.ends_with("_test.py"));
    let mut imports = Imports::default();
    let mut classes = HashMap::<usize, Class>::new();
    let mut hidden: HashSet<(Option<usize>, &str)> = HashSet::new();
    let mut disabled_module = false;
    // A constructor or opt-out may appear anywhere inside a class. Resolve
    // direct members first so an earlier test declaration observes it.
    for (i, st) in list.iter().enumerate() {
        let first = st.first;
        if st.owner.is_none() && st.indent == 0 {
            imports.observe(s, st);
        }
        if s.w(first) == "class" {
            classes.insert(i, Class::default());
        } else if s.seq(first, &["__test__", "=", "False"]) {
            if let Some(class) = st.owner.and_then(|n| classes.get_mut(&n)) {
                class.no_pytest = true;
            } else if st.owner.is_none() && st.indent == 0 {
                disabled_module = true;
            }
        } else if s.seq(first + 1, &[".", "__test__", "=", "False"]) {
            hidden.insert((st.owner, s.w(first)));
        } else if s.w(first) == "def"
            && matches!(s.w(first + 1), "__init__" | "__new__")
            && let Some(class) = st.owner.and_then(|n| classes.get_mut(&n))
        {
            class.custom_init = true;
        }
    }

    // unittest's runTest is a fallback only when no named tests are collected.
    for st in &list {
        let head = st.first + usize::from(s.w(st.first) == "async");
        let name = s.w(head + 1);
        if s.w(head) == "def"
            && name.starts_with("test")
            && !hidden.contains(&(st.owner, name))
            && let Some(class) = st.owner.and_then(|owner| classes.get_mut(&owner))
        {
            class.named_tests = true;
        }
    }
    let mut local_classes = HashMap::<&str, Class>::new();
    let mut active_functions = HashSet::<usize>::new();
    for (i, st) in list.iter().enumerate().filter(|(_, st)| st.end != 0) {
        let head = st.first + usize::from(s.w(st.first) == "async");
        let name = s.w(head + 1);
        let owner = st.owner.and_then(|n| classes.get(&n)).copied();
        let local = st.owner.is_some() && owner.is_none();
        let mut first = i;
        while first > 0 && list[first - 1].indent == st.indent && s.w(list[first - 1].first) == "@"
        {
            first -= 1;
        }
        let last = list[st.end - 1].after - 1;
        let enabled = !disabled_module && !hidden.contains(&(st.owner, name));
        if s.w(head) == "class" {
            let mut class = classes[&i];
            // Recognize verified unittest imports and locally declared bases.
            if s.w(head + 2) == "("
                && let Some(close) = s.close(head + 2)
            {
                for j in head + 3..close {
                    let word = s.w(j);
                    let qualified = matches!(
                        word,
                        "TestCase" | "IsolatedAsyncioTestCase" | "FunctionTestCase"
                    ) && j >= 2
                        && s.w(j - 1) == "."
                        && imports.unittest.contains(s.w(j - 2));
                    if imports.cases.contains(word) || qualified {
                        class.unittest = true;
                    }
                    if let Some(base) = local_classes.get(word) {
                        class.unittest |= base.unittest;
                        class.custom_init |= base.custom_init;
                        class.named_tests |= base.named_tests;
                        class.no_pytest |= base.no_pytest;
                    }
                }
            }
            class.collected = enabled
                && !local
                && owner.is_none_or(|o| o.collected)
                && !class.no_pytest
                && (class.unittest
                    || (pytest_file && name.starts_with("Test") && !class.custom_init));
            if st.owner.is_none() {
                local_classes.insert(name, class);
            }
            classes.insert(i, class);
            if class.collected {
                s.add(
                    res,
                    list[first].first,
                    Some(last),
                    Role::Support,
                    "test class",
                );
            } else if st.owner.is_some_and(|p| active_functions.contains(&p)) {
                s.add(
                    res,
                    list[first].first,
                    Some(last),
                    Role::Support,
                    "class local to a test",
                );
            }
            continue;
        }
        let fixture = (first..i).any(|j| imports.fixture(s, list[j].first + 1));
        let test = enabled
            && !local
            && !fixture
            && (name.starts_with("test")
                || (name == "runTest" && owner.is_some_and(|o| o.unittest && !o.named_tests)))
            && owner.map_or(pytest_file && st.indent == 0, |o| o.collected);
        let inside_test = st.owner.is_some_and(|p| active_functions.contains(&p));
        let hook = st.owner.is_none()
            && (pytest_file || filename == "conftest.py")
            && matches!(
                name,
                "setup_module"
                    | "teardown_module"
                    | "setup_function"
                    | "teardown_function"
                    | "setUpModule"
                    | "tearDownModule"
            );
        let (role, reason) = if test {
            active_functions.insert(i);
            (Role::Test, "test discovery function")
        } else if fixture {
            (Role::Support, "pytest fixture")
        } else if inside_test || hook || owner.is_some_and(|o| o.collected) {
            (Role::Support, "test helper or lifecycle method")
        } else {
            continue;
        };
        s.add(res, list[first].first, Some(last), role, reason);
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
