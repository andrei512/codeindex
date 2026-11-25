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
    use Language::*;
    if !matches!(lang, ObjectiveC | Php) {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut objc_names = std::collections::HashSet::new();
    for (i, t) in s.tokens.iter().enumerate() {
        if lang == ObjectiveC {
            if t.text == "interface" && s.w(i + 2) == ":" && s.w(i + 3).ends_with("TestCase") {
                objc_names.insert(s.w(i + 1));
            }
            if t.text == "implementation"
                && (objc_names.contains(s.w(i + 1)) || test_file && s.w(i + 1).ends_with("Tests"))
            {
                let end = (i + 1..s.tokens.len())
                    .find(|&j| s.seq(j, &["@", "end"]))
                    .unwrap_or(s.tokens.len() - 1);
                result.push(Suite {
                    first: i,
                    last: end,
                });
            }
            continue;
        }
        if t.text != "class" {
            continue;
        }
        let delimiter = "{";
        let mut at = i + 2;
        while at < s.tokens.len() && s.w(at) != delimiter && !matches!(s.w(at), ";" | "}" | "class")
        {
            at += 1
        }
        if at >= s.tokens.len() || s.w(at) != delimiter {
            continue;
        }
        let test_base = s.tokens[i + 2..at]
            .iter()
            .any(|t| t.text == "TestCase" || test_file && t.text.ends_with("TestCase"));
        if !test_base {
            continue;
        }
        let end = s.close(at);
        if let Some(last) = end {
            result.push(Suite { first: i, last });
        }
    }
    result
}
