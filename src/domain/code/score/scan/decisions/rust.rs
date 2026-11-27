use super::{PairedSyntax, at};
use crate::domain::code::rust::after_angles;
use std::collections::{HashMap, HashSet};

pub(in crate::domain::code::score::scan) struct MatchArms(HashSet<usize>);

impl MatchArms {
    pub fn read(syntax: PairedSyntax) -> Self {
        let mut arms = Self(HashSet::new());
        let mut completed = HashMap::new();
        // A match can itself be the scrutinee of another match. Visit the
        // inner keyword first so the outer header can skip that expression.
        for (i, token) in syntax.tokens.iter().enumerate().rev() {
            if token.text == "match"
                && at(syntax.tokens, i.wrapping_sub(1)) != "$"
                && let Some(close) = arms.read_match(syntax, i + 1, &completed)
            {
                completed.insert(i, close + 1);
            }
        }
        arms
    }

    pub fn contains(&self, i: usize) -> bool {
        self.0.contains(&i)
    }

    fn read_match(
        &mut self,
        syntax: PairedSyntax,
        mut i: usize,
        completed: &HashMap<usize, usize>,
    ) -> Option<usize> {
        let t = syntax.tokens;
        while i < t.len() && !header_end(t, i) {
            if at(t, i) == "{" && at(t, i - 1) != "!" {
                let close = syntax.pairs[i];
                if close >= t.len() {
                    break;
                }
                if self.is_body(syntax, i, close) {
                    return Some(close);
                }
            }
            i = completed
                .get(&i)
                .copied()
                .unwrap_or_else(|| next(syntax, i));
        }
        None
    }

    fn is_body(&mut self, syntax: PairedSyntax, open: usize, close: usize) -> bool {
        let mut found = false;
        let mut i = open + 1;
        while i < close {
            if fat_arrow(syntax.tokens, i) {
                self.0.insert(i);
                found = true;
            }
            i = syntax.next_top_level(i, close, &["(", "[", "{"]);
        }
        found || !continued(syntax.tokens, close + 1)
    }
}

fn header_end(t: &[super::Token], i: usize) -> bool {
    matches!(at(t, i), "," | ";" | ")" | "]" | "}") || fat_arrow(t, i)
}

fn fat_arrow(t: &[super::Token], i: usize) -> bool {
    at(t, i) == "=" && at(t, i + 1) == ">" && t[i].span.end == t[i + 1].span.start
}

fn next(syntax: PairedSyntax, i: usize) -> usize {
    let t = syntax.tokens;
    if at(t, i) == "<" && matches!(at(t, i - 1), ":" | "match" | "&" | "*") {
        return after_angles(t, syntax.pairs, i).unwrap_or(i + 1);
    }
    syntax.next_top_level(i, t.len(), &["(", "[", "{"])
}

fn continued(t: &[super::Token], next: usize) -> bool {
    matches!(
        at(t, next),
        "{" | "else"
            | "."
            | "?"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "&"
            | "|"
            | "^"
            | "as"
            | "="
            | "!"
            | "<"
            | ">"
    )
}

#[cfg(test)]
mod tests {
    use crate::domain::code::{Language, score::Rule, scored};

    fn complexity(source: &str, name: &str) -> Option<usize> {
        scored(Language::Rust, source, "example.rs", None)
            .1
            .unwrap()
            .findings
            .iter()
            .find(|finding| finding.rule == Rule::Complexity && finding.unit == name)
            .map(|finding| finding.value)
    }

    #[test]
    fn macro_rules_and_token_tree_maps_are_declarative() {
        let source = r#"
            macro_rules! describe {
                (alfa) => {pair(alfa)};
                (bravo) => {pair(bravo)};
                (charlie) => {pair(charlie)};
                (delta) => {pair(delta)};
                (echo) => {pair(echo)};
                (foxtrot) => {pair(foxtrot)};
                (golf) => {pair(golf)};
                (hotel) => {pair(hotel)};
            }
            formats::registry! {
                [0 => Alfa, 1 => Bravo, 2 => Charlie, 3 => Delta],
                4 => Echo,
                5 => Foxtrot,
                match => {0 => Golf, 1 => Hotel}
            }
        "#;
        assert_eq!(complexity(source, "<file>"), None);
        assert_eq!(complexity(source, "describe"), None);
    }

    #[test]
    fn executable_arms_survive_declarative_macro_tables_and_rules() {
        let arms = (0..10)
            .map(|tag| format!("{tag} => {tag},"))
            .collect::<String>();
        let source = format!(
            "macro_rules! generate {{
                () => {{ fn decode(value: u8) -> u8 {{
                    inspect!(0 => 1, match load!{{value}} {{ {arms} _ => 0 }})
                }} }};
            }}"
        );
        // Eleven executable arms plus the baseline path.
        assert_eq!(complexity(&source, "generate"), Some(12));
    }

    #[test]
    fn executable_arms_with_nested_or_braced_scrutinees_count() {
        let source = r#"
            fn decode(value: u8) -> u8 {
                match match { value } {
                    0 => 4, 1 => 3, 2 => 2, 3 => 1, _ => 0
                } {
                    0 => 4, 1 => 3, 2 => 2, 3 => 1, _ => 0
                }
            }
        "#;
        assert_eq!(complexity(source, "decode"), Some(11));
    }

    #[test]
    fn only_arms_count_after_an_if_else_scrutinee_and_an_empty_match() {
        let source = r#"
            fn decode(value: u8, flag: bool, never: Never) -> u8 {
                inspect!(match never {}, [0 => Alpha, 1 => Beta]);
                match if flag { value } else { 0 } {
                    0 => 0, 1 => 1, 2 => 2, 3 => 3, 4 => 4, 5 => 5, 6 => 6, _ => 0,
                }
            }
        "#;
        assert_eq!(complexity(source, "decode"), Some(10));
    }

    #[test]
    fn generic_commas_do_not_end_a_match_header() {
        let source = r#"
            fn decode() -> u8 {
                match <Format as Decode<u8, u16>>::value() {
                    0 => 0, 1 => 1, 2 => 2, 3 => 3, _ => 0,
                } + match decode::<u8, u16>() {
                    0 => 0, 1 => 1, 2 => 2, 3 => 3, _ => 0,
                }
            }
        "#;
        assert_eq!(complexity(source, "decode"), Some(11));
    }
}
