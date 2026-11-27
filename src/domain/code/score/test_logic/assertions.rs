//! Locate the two expressions an assertion compares. All ranges refer to the
//! original token stream and paired delimiters belong to the shared syntax.
use super::{Language, PairedSyntax, scan};
use std::ops::Range;

type Operands = (Range<usize>, Range<usize>);

pub(super) struct Expressions<'a, 'text> {
    syntax: PairedSyntax<'a, 'text>,
    end: usize,
}

impl<'a, 'text> Expressions<'a, 'text> {
    pub(super) fn new(syntax: PairedSyntax<'a, 'text>, end: usize) -> Self {
        Self { syntax, end }
    }

    pub(super) fn operands(&self, lang: Language, i: usize) -> Option<Operands> {
        let t = self.syntax.tokens;
        let name = t[i].text;
        if lang == Language::Python && name == "assert" {
            let stop = i + t[i..self.end].partition_point(|tok| tok.line == t[i].line);
            return self.comparison(self.first_argument(i + 1..stop));
        }
        let open = i + 1 + usize::from(scan::at(t, i + 1) == "!");
        let args = self.arguments(open)?;
        if name == "expect"
            && let Some(rhs) = self.matcher(args.end)
        {
            return Some((args, rhs));
        }
        let first = self.first_argument(args.clone());
        if pair_assertion(name) && first.end < args.end {
            let second = self.first_argument(first.end + 1..args.end);
            return Some((first, second));
        }
        self.comparison(first)
    }

    pub(super) fn alternative_loop_assertions(&self, header: Range<usize>) -> bool {
        let open = header.end;
        let close = self.syntax.pairs[open];
        let otherwise = close + 2;
        scan::at(self.syntax.tokens, close + 1) == "else"
            && scan::at(self.syntax.tokens, otherwise) == "{"
            && self.syntax.pairs[otherwise] < self.end
            && self.checks_same_element(
                header.start + 1..open,
                open + 1..close,
                otherwise + 1..self.syntax.pairs[otherwise],
            )
    }

    /// A condition reads base[index]. Both arms must assert on the same
    /// other collection at that index. Each arm is inspected once even when
    /// the condition mentions many indexes.
    fn checks_same_element(
        &self,
        condition: Range<usize>,
        left: Range<usize>,
        right: Range<usize>,
    ) -> bool {
        use std::collections::{HashMap, HashSet};
        let mut bases = HashMap::new();
        for w in self.syntax.tokens[condition].windows(4) {
            if w[1].text == "[" && w[3].text == "]" {
                let only = bases.entry(w[2].text).or_insert(Some(w[0].text));
                if *only != Some(w[0].text) {
                    *only = None;
                }
            }
        }
        if bases.is_empty() {
            return false;
        }
        let eligible = |&(index, name): &(&str, &str)| {
            bases.get(index).is_some_and(|base| *base != Some(name))
        };
        let left: HashSet<_> = self.asserted_elements(left).filter(eligible).collect();
        !left.is_empty() && self.asserted_elements(right).any(|key| left.contains(&key))
    }

    fn asserted_elements(
        &self,
        arm: Range<usize>,
    ) -> impl Iterator<Item = (&'text str, &'text str)> + '_ {
        let t = self.syntax.tokens;
        let mut until = arm.start;
        let (start, end) = (arm.start, arm.end);
        arm.filter_map(move |i| {
            if i >= start + 2 && assertion_argument(t, i) {
                until = until.max(self.syntax.pairs[i - 1].min(end));
            }
            (i + 4 <= until).then(|| element(&t[i..i + 4])).flatten()
        })
    }

    fn arguments(&self, open: usize) -> Option<Range<usize>> {
        if scan::at(self.syntax.tokens, open) != "(" {
            return None;
        }
        let close = self.syntax.pairs[open];
        (close < self.end).then_some(open + 1..close)
    }

    fn matcher(&self, close: usize) -> Option<Range<usize>> {
        if scan::at(self.syntax.tokens, close + 1) != "."
            || !matches!(
                scan::at(self.syntax.tokens, close + 2),
                "toBe" | "toEqual" | "toStrictEqual"
            )
        {
            return None;
        }
        self.arguments(close + 3)
    }

    fn first_argument(&self, args: Range<usize>) -> Range<usize> {
        args.start..self.delimiter(args.clone(), &[","]).unwrap_or(args.end)
    }

    pub(super) fn comparison(&self, span: Range<usize>) -> Option<Operands> {
        let op = self.delimiter(span.clone(), &["=", "!"])?;
        (op + 1 < span.end && self.syntax.tokens[op + 1].text == "=")
            .then_some((span.start..op, op + 2..span.end))
    }

    fn delimiter(&self, span: Range<usize>, words: &[&str]) -> Option<usize> {
        let mut i = span.start;
        while i < span.end {
            if words.contains(&self.syntax.tokens[i].text) {
                return Some(i);
            }
            i = self.syntax.next_top_level(i, span.end, &["(", "[", "{"]);
        }
        None
    }
}

fn pair_assertion(name: &str) -> bool {
    matches!(
        name,
        "assert_eq"
            | "assert_ne"
            | "ASSERT_EQ"
            | "ASSERT_NE"
            | "EXPECT_EQ"
            | "EXPECT_NE"
            | "assertEquals"
            | "assertNotEquals"
            | "XCTAssertEqual"
            | "XCTAssertNotEqual"
    )
}

fn assertion_argument(tokens: &[super::Token], i: usize) -> bool {
    tokens[i - 1].text == "(" && super::assertion(tokens[i - 2].text)
}

fn element<'text>(tokens: &[super::Token<'text>]) -> Option<(&'text str, &'text str)> {
    let subscript = tokens[1].text == "[" && tokens[3].text == "]";
    (tokens[0].kind == super::Kind::Word && subscript).then_some((tokens[2].text, tokens[0].text))
}
