//! Executable test bodies use token coordinates. A full registration can
//! independently provide role evidence when the callback is a named function.
use super::{Language, Token, registrations};

pub(super) struct BodySyntax<'t, 's> {
    pub(super) tokens: &'t [Token<'s>],
    pub(super) pairs: &'t [usize],
}

impl<'s> BodySyntax<'_, 's> {
    pub(super) fn word(&self, i: usize) -> &'s str {
        self.tokens.get(i).map_or("", |token| token.text)
    }

    pub(super) fn unqualified(&self, i: usize) -> bool {
        i == 0
            || !matches!(
                self.tokens[i - 1].text,
                "." | "def" | "function" | "fun" | ">"
            )
    }

    fn collect(
        &self,
        mut body: impl FnMut(&Self, usize) -> Option<(usize, usize)>,
    ) -> Vec<ExecutableBody<'s>> {
        self.tokens
            .iter()
            .enumerate()
            .filter_map(|(i, token)| {
                let (open, close) = body(self, i)?;
                Some(ExecutableBody {
                    open,
                    end: close + 1,
                    line: token.line,
                    name: token.text,
                })
            })
            .collect()
    }
}

pub(in crate::domain::code) struct ExecutableBody<'a> {
    pub(in crate::domain::code) open: usize,
    pub(in crate::domain::code) end: usize,
    pub(in crate::domain::code) line: usize,
    pub(in crate::domain::code) name: &'a str,
}
/// Each framework supplies the executable range of every suite, case and hook.
/// The ends of Ruby keyword blocks are also used to measure their control depth.
pub(in crate::domain::code) fn suite_bodies<'a>(
    lang: Language,
    input: super::super::score::PairedSyntax<'_, 'a>,
    ruby_blocks: &registrations::RubyBlocks,
) -> Option<Vec<ExecutableBody<'a>>> {
    let syntax = BodySyntax {
        tokens: input.tokens,
        pairs: input.pairs,
    };
    let mut registrations = match lang {
        Language::Ruby => syntax.collect(|s, i| ruby_blocks.body(s, i)),
        Language::Dart | Language::Php => syntax.collect(registrations::callback_body),
        Language::Kotlin => syntax.collect(registrations::kotest_body),
        Language::Powershell => syntax.collect(registrations::pester_body),
        Language::Perl => syntax.collect(registrations::perl_body),
        _ => return None,
    };
    if registrations.is_empty() {
        return None;
    }
    registrations.sort_unstable_by_key(|entry| entry.open);
    Some(registrations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::code::{lexer, roles, script};

    #[test]
    fn named_callback_has_a_registration_but_only_inline_bodies_bound_depth() {
        let code = "group('suite', () {\n test.skip('named', callback);\n test('inline', () { if (x) work(); });\n});";
        let tokens = lexer::tokens(code, Language::Dart);
        let pairs = script::paired_for_score(&tokens);
        let roles = roles::classify("test/example_test.dart", code);
        assert!(
            roles
                .regions
                .iter()
                .any(|r| r.first == 2 && r.evidence.0 == roles::Role::Test)
        );
        let bodies = suite_bodies(Language::Dart, super::super::super::score::PairedSyntax {tokens: &tokens, pairs: &pairs}, &Default::default()).unwrap();
        assert_eq!(
            bodies.iter().map(|b| (b.name, b.line)).collect::<Vec<_>>(),
            [("group", 1), ("test", 3)]
        );
        assert!(bodies[0].open < bodies[1].open && bodies[1].end < bodies[0].end);
    }
}
