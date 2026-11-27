//! Lexical scopes that qualify convention-based test methods. Imports alone
//! do not make unrelated classes in the same file into test suites.
use super::{Language, Syntax};

pub(super) struct Suite {
    first: usize,
    last: usize,
}
impl Suite {
    pub fn contains(&self, pos: usize) -> bool {
        self.first < pos && pos <= self.last
    }
}
pub(super) fn suites(lang: Language, s: &Syntax, test_file: bool) -> Vec<Suite> {
    match lang {
        Language::ObjectiveC => objc_suites(s, test_file),
        Language::Php => php_suites(s, test_file),
        _ => Vec::new(),
    }
}

fn objc_suites(s: &Syntax, test_file: bool) -> Vec<Suite> {
    let mut names = std::collections::HashSet::new();
    let mut result = Vec::new();
    for (i, t) in s.tokens.iter().enumerate() {
        let name = s.w(i + 1);
        match t.text {
            "interface" if objc_test_base(s, i) => {
                names.insert(name);
            }
            "implementation" if names.contains(name) || objc_test_name(test_file, name) => {
                result.push(objc_suite(s, i));
            }
            _ => {}
        }
    }
    result
}

fn objc_test_base(s: &Syntax, i: usize) -> bool {
    s.w(i + 2) == ":" && s.w(i + 3).ends_with("TestCase")
}

fn objc_test_name(test_file: bool, name: &str) -> bool {
    test_file && name.ends_with("Tests")
}

fn objc_suite(s: &Syntax, first: usize) -> Suite {
    let last = (first + 1..s.tokens.len())
        .find(|&j| s.seq(j, &["@", "end"]))
        .unwrap_or(s.tokens.len() - 1);
    Suite { first, last }
}

fn php_suites(s: &Syntax, test_file: bool) -> Vec<Suite> {
    s.tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.text == "class")
        .filter_map(|(first, _)| php_suite(s, first, test_file))
        .collect()
}

fn php_suite(s: &Syntax, first: usize, test_file: bool) -> Option<Suite> {
    let header = first + 2..s.tokens.len();
    let at = header
        .clone()
        .find(|&j| matches!(s.w(j), "{" | ";" | "}" | "class"))?;
    if s.w(at) != "{" {
        return None;
    }
    let test_base = s.tokens[header.start..at]
        .iter()
        .any(|t| t.text == "TestCase" || test_file && t.text.ends_with("TestCase"));
    test_base
        .then(|| s.close(at))
        .flatten()
        .map(|last| Suite { first, last })
}
