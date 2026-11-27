//! Associate declared functions with their written parameter lists and body ranges.
use super::*;

impl<'src> RegionSource<'_, 'src> {
    pub(super) fn declared_regions(&self) -> Vec<Region<'src>> {
        let t = self.syntax.tokens;
        let mut marks = Vec::new();
        let mut previous = 0;
        let mut ordered = true;
        for (id, d) in self
            .facts
            .declarations
            .iter()
            .enumerate()
            .filter(|(_, d)| declaration(d.kind))
        {
            let start = token_at(t, previous, d.span.start);
            if start == t.len() {
                continue;
            }
            ordered &= marks.is_empty() || previous < start;
            previous = start;
            marks.push((start, id));
        }
        if !ordered {
            marks.sort_unstable_by_key(|x| x.0);
            marks.dedup_by_key(|x| x.0);
        }
        marks
            .iter()
            .enumerate()
            .filter_map(|(m, &(start, id))| {
                let next = marks.get(m + 1).map_or(t.len(), |m| m.0);
                self.region(start, next, id)
            })
            .collect()
    }
}

// Most declarations arrive in source order. Bound the search by the distance
// from the previous declaration, and allow parsers to emit older positions.
fn token_at(t: &[Token], previous: usize, offset: usize) -> usize {
    if previous != 0 && t[previous - 1].span.end > offset {
        return t[..previous].partition_point(|t| t.span.end <= offset);
    }
    previous + crate::domain::prefix_len(&t[previous..], |t| t.span.end <= offset)
}

impl<'src> RegionSource<'_, 'src> {
    fn region(&self, start: usize, next: usize, id: usize) -> Option<Region<'src>> {
        let name = &self.text[self.facts.declarations[id].span.clone()];
        let (p, args) = self.after_parameters(start, next, name, id);
        let parameters_end = p.min(next);
        let (body, end) = if matches!(self.lang, Python | Starlark) {
            self.indented(start, parameters_end, next)
        } else {
            self.braced(start, parameters_end, next)
        };
        (end > start).then_some(Region {
            name,
            start,
            body,
            end,
            args,
        })
    }

    fn after_parameters(&self, start: usize, next: usize, name: &str, id: usize) -> (usize, usize) {
        let lang = self.lang;
        let t = self.syntax.tokens;
        let p = self.header_end(
            start + 1..next,
            t[start].line + 2,
            &["(", "{", ";", ":", "="],
        );
        let Some(close) = self.syntax.close_paren(p) else {
            return (p, 0);
        };
        // C++ call operators spell the name with an empty pair of
        // parentheses followed by the actual parameter list.
        let call_operator = lang == Cpp && name == "operator" && close == p + 1;
        let extra = call_operator
            .then(|| self.syntax.close_paren(p + 2))
            .flatten();
        let (p, close) = extra.map_or((p, close), |next_close| (p + 2, next_close));
        let parameters = p + 1..close;
        let omit_wildcards = lang == Rust && foreign_abi(&t[..start]);
        let explicit = self
            .syntax
            .count_parameters(lang, parameters.clone(), omit_wildcards)
            .saturating_sub(usize::from(self.has_receiver(start, id, parameters)));
        (close + 1, explicit)
    }

    fn header_end(&self, span: std::ops::Range<usize>, last_line: usize, stops: &[&str]) -> usize {
        let end = span.end;
        let t = self.syntax.tokens;
        span.into_iter()
            .find(|&i| t[i].line > last_line || stops.contains(&t[i].text))
            .unwrap_or(end)
    }

    fn has_receiver(&self, start: usize, id: usize, parameters: std::ops::Range<usize>) -> bool {
        let t = self.syntax.tokens;
        match self.lang {
            Python => {
                self.facts
                    .script
                    .as_ref()
                    .is_some_and(|s| s.has_method_receiver(id))
                    && !static_method(t, start)
            }
            Rust => t[parameters]
                .iter()
                .take_while(|tok| !matches!(tok.text, "," | ":"))
                .any(|tok| tok.text == "self"),
            _ => false,
        }
    }

    fn indented(&self, start: usize, mut body: usize, next: usize) -> (usize, usize) {
        let (t, text) = (self.syntax.tokens, self.text);
        while body < next && t[body].text != ":" {
            body += 1;
        }
        let base = ind(text, t[start].span.start);
        let mut j = (body + 1).min(t.len());
        while j < t.len() {
            if t[j].line > t[j - 1].line && ind(text, t[j].span.start) <= base {
                break;
            }
            j = self.syntax.next_top_level(j, t.len(), &["(", "[", "{"]);
        }
        (body.min(j), j)
    }
    fn braced(&self, start: usize, mut body: usize, next: usize) -> (usize, usize) {
        let (t, pairs) = (self.syntax.tokens, self.syntax.pairs);
        body = self.header_end(body..next, t[start].line + 12, &["{", ";"]);
        let brace = body < next && t[body].text == "{";
        if brace && pairs[body] < t.len() {
            (body, pairs[body] + 1)
        } else {
            (start, next)
        }
    }
}

// A bare wildcard can occupy a slot required by the foreign caller without
// adding an input to the Rust implementation.
fn foreign_abi(prefix: &[Token]) -> bool {
    let [modifiers @ .., abi, function] = prefix else {
        return false;
    };
    if function.text != "fn" {
        return false;
    }
    abi.text == "extern"
        || (modifiers.last().is_some_and(|token| token.text == "extern")
            && abi.kind == Kind::String
            && abi.text.split('"').nth(1).is_some_and(foreign_abi_name))
}

fn foreign_abi_name(name: &str) -> bool {
    name != "Rust" && !name.contains('\\')
}

#[cfg(test)]
#[test]
fn repeated_and_out_of_order_declarations_keep_their_parameter_lists() {
    use crate::domain::code::{Declaration, lexer};
    let text = "fn first(x: X, y: Y) {}\nfn second() {}\n";
    let tokens = lexer::tokens(text, Rust);
    let pairs = script::paired_for_score(&tokens);
    let mut facts = Analysis::default();
    for spelling in ["second", "first", "first"] {
        let start = text.find(spelling).unwrap();
        facts.declarations.push(Declaration {
            span: start..start + spelling.len(),
            kind: "function_item",
        });
    }
    let regions = RegionSource {
        lang: Rust,
        text,
        syntax: PairedSyntax {
            tokens: &tokens,
            pairs: &pairs,
        },
        facts: &facts,
    }
    .declared_regions();
    let measured: Vec<_> = regions.iter().map(|r| (r.name, r.args)).collect();
    assert_eq!(measured, [("first", 2), ("second", 0)]);
}
