//! Resolve literal Starlark test lists passed to a loaded analysis test suite.
use super::{Role, Roles, Syntax, indentation};
use std::collections::{HashMap, HashSet};

fn string<'a>(s: &'a Syntax, i: usize) -> &'a str {
    s.w(i).trim_matches(['\'', '"'])
}

fn list<'a>(s: &'a Syntax, open: usize) -> Option<Vec<&'a str>> {
    let end = s.close(open)?;
    if s.w(open) != "[" {
        return None;
    }
    let mut names = Vec::new();
    let mut i = open + 1;
    while i < end {
        if s.tokens[i].kind != super::Kind::Word {
            return None;
        }
        names.push(s.w(i));
        i += 1;
        if i != end && s.w(i) != "," {
            return None;
        }
        i += 1;
    }
    Some(names)
}

pub(super) fn collect(s: &Syntax, res: &mut Roles) {
    let mut suite_names = HashSet::new();
    let mut makers = HashSet::new();
    let mut lists = HashMap::new();
    for (i, token) in s.tokens.iter().enumerate() {
        if s.seq(i, &["load", "("]) {
            let module = string(s, i + 2);
            let Some(end) = s.close(i + 1) else { continue };
            let mut j = i + 3;
            while j < end {
                if !s.string(j) {
                    j += 1;
                    continue;
                }
                let original = string(s, j);
                let local = if j >= 2 && s.w(j - 1) == "=" {
                    s.w(j - 2)
                } else {
                    original
                };
                if original == "test_suite"
                    && matches!(
                        module.split_once("//"),
                        Some((
                            "@rules_testing",
                            "lib:analysis_test.bzl" | "lib:test_suite.bzl"
                        ))
                    )
                {
                    suite_names.insert(local);
                }
                if matches!(original, "unittest" | "analysistest")
                    && module.ends_with("//lib:unittest.bzl")
                {
                    makers.insert(local);
                }
                j += 1;
            }
        }
        if token.kind != super::Kind::Word
            || i > 0 && token.line == s.tokens[i - 1].line
            || indentation(s.text, token.span.start) != 0
        {
            continue;
        }
        let name = token.text;
        if s.w(i + 1) == "=" {
            if let Some(values) = list(s, i + 2) {
                lists.insert(name, values);
            } else {
                lists.remove(name);
            }
        } else if s.seq(i + 1, &[".", "append", "("]) && s.w(i + 5) == ")" {
            if let Some(values) = lists.get_mut(name) {
                values.push(s.w(i + 4));
            }
        } else if s.seq(i + 1, &[".", "extend", "("]) {
            if let Some(values) = list(s, i + 4) {
                if let Some(entries) = lists.get_mut(name) {
                    entries.extend(values);
                }
            } else {
                lists.remove(name);
            }
        }
    }
    let mut registered = HashSet::new();
    for (i, t) in s.tokens.iter().enumerate() {
        let w = t.text;
        if makers.contains(w)
            && s.seq(i + 1, &[".", "make", "("])
            && matches!(s.w(i + 5), "," | ")")
        {
            registered.insert(s.w(i + 4));
        } else if suite_names.contains(w) && s.w(i + 1) == "(" {
            let Some(end) = s.close(i + 1) else { continue };
            let mut arg = i + 2;
            while arg < end {
                if s.seq(arg, &["basic_tests", "="]) {
                    if let Some(names) = list(s, arg + 2) {
                        registered.extend(names);
                    } else if matches!(s.w(arg + 3), "," | ")")
                        && let Some(names) = lists.get(s.w(arg + 2))
                    {
                        registered.extend(names.iter().copied());
                    }
                }
                while arg < end && s.w(arg) != "," {
                    arg = s.close(arg).unwrap_or(arg) + 1;
                }
                arg += 1;
            }
        }
        if w.ends_with("_test")
            && s.seq(i + 1, &["(", "name", "="])
            && (i == 0 || s.w(i - 1) != "def")
        {
            s.add(res, i, s.close(i + 1), Role::Test, "Bazel test target");
        }
    }
    for (i, t) in s.tokens.iter().enumerate() {
        if t.text == "def"
            && registered.contains(s.w(i + 1))
            && indentation(s.text, t.span.start) == 0
        {
            s.add(
                res,
                i,
                Some(s.indented(i)),
                Role::Test,
                "registered Starlark test implementation",
            );
        }
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
