//! A type before a declarator can end in a pointer, array, or type argument.
//! The last angle probe is enough to skip a previously inspected prefix
//! in a comma-separated expression of comparisons.
use super::Token;

#[derive(Clone, Copy)]
pub(super) enum Prefix {
    Declared { cv: bool },
    WrittenReturn,
}
impl Prefix {
    pub(super) fn before(
        self,
        tokens: &[Token],
        pairs: &[usize],
        name: usize,
    ) -> Option<(usize, bool)> {
        let mut at = name.checked_sub(1)?;
        let mut pointer = false;
        loop {
            match (tokens[at].text, self) {
                ("*", _) => pointer = matches!(self, Self::Declared { cv: true }),
                ("&", _)
                | ("const" | "volatile", Self::WrittenReturn | Self::Declared { cv: true })
                | ("?", Self::Declared { .. }) => {}
                ("]", Self::Declared { .. }) => at = super::opening(pairs, at)?,
                _ => return Some((at, pointer)),
            }
            at = at.checked_sub(1)?;
        }
    }
}

#[derive(Default)]
pub(super) struct Angles {
    previous: Option<(usize, Option<usize>)>,
}

impl Angles {
    pub fn opening(&mut self, tokens: &[Token], close: usize) -> Option<usize> {
        if let Some((last, open)) = self.previous
            && last == close
        {
            return open;
        }
        let open = self.probe(tokens, close);
        self.previous = Some((close, open));
        open
    }

    fn probe(&self, tokens: &[Token], close: usize) -> Option<usize> {
        let mut depth = 1;
        let mut at = close;
        loop {
            at = at.checked_sub(1)?;
            match tokens[at].text {
                ">" => {
                    if let Some((last, open)) = self.previous
                        && last == at
                    {
                        at = open?;
                    } else {
                        depth += 1;
                    }
                }
                "<" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(at);
                    }
                }
                ";" | "{" | "}" => return None,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::code::{Language, lexer};
    #[test]
    fn cached_closers_match_fresh_probes_in_either_order() {
        let text = "< > > ; A<B<C>> > ; < < > ; > } a > b, c > d, e > f";
        let tokens = lexer::tokens(text, Language::Cpp);
        let closes: Vec<_> = (0..tokens.len())
            .filter(|&i| tokens[i].text == ">")
            .collect();
        let mut cache = Angles::default();
        for &i in closes.iter().chain(closes.iter().rev()) {
            assert_eq!(
                cache.opening(&tokens, i),
                Angles::default().probe(&tokens, i)
            );
        }
    }
    #[test]
    fn unmatched_array_before_name_has_no_written_type() {
        let tokens = lexer::tokens("] y; ]", Language::Cpp);
        let pairs = crate::domain::code::script::paired_for_score(&tokens);
        assert_eq!(
            Prefix::Declared { cv: true }.before(&tokens, &pairs, 1),
            None
        );
    }
}
