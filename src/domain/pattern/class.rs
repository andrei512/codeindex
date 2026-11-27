//! Bracket operands and compiled scalar predicates. Only range sets share heap storage.
use super::{sets, unicode};
#[derive(Clone, Debug)]
pub(super) enum Member {
    Range(char, char),
    Word(bool),
    Digit(bool),
    Space(bool),
    Set(sets::Set),
}
#[derive(Clone, Copy, Debug)]
pub(super) enum PerlClass {
    Word,
    Digit,
    Space,
}
impl PerlClass {
    fn contains(self, ch: char) -> bool {
        match self {
            Self::Word => unicode::word(ch, false),
            Self::Digit => unicode::decimal(ch),
            Self::Space => unicode::space(ch),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum Class {
    Range {
        start: char,
        end: char,
        fold: bool,
    },
    Perl {
        class: PerlClass,
        positive: bool,
        fold: bool,
    },
    // The parser has already applied case folding and set operations.
    Set(sets::Set),
    Any,
    ExceptLf,
    ExceptCrlf,
}

impl Class {
    pub(super) fn singleton(&self) -> Option<char> {
        match self {
            Self::Range { start, end, fold } if start == end =>
                (!fold || unicode::folded(*start).is_empty()).then_some(*start),
            Self::Set(set) => {
                let &[(start, end)] = set.0.as_ref() else { return None };
                (start == end).then(|| char::from_u32(start)).flatten()
            }
            _ => None,
        }
    }

    pub(super) fn one(member: Member, fold: bool) -> Self {
        let perl = |class, positive| Self::Perl {
            class,
            positive,
            fold,
        };
        match member {
            Member::Range(start, end) => Self::Range { start, end, fold },
            Member::Word(positive) => perl(PerlClass::Word, positive),
            Member::Digit(positive) => perl(PerlClass::Digit, positive),
            Member::Space(positive) => perl(PerlClass::Space, positive),
            Member::Set(set) => Self::Set(set),
        }
    }

    pub(super) fn accepts(&self, ch: char) -> bool {
        match self {
            Self::Any => true,
            Self::ExceptLf => ch != '\n',
            Self::ExceptCrlf => !matches!(ch, '\r' | '\n'),
            Self::Range { start, end, fold } => {
                accepts_folded(ch, *fold, |c| (*start..=*end).contains(&c))
            }
            Self::Perl {
                class,
                positive,
                fold,
            } => accepts_folded(ch, *fold, |c| class.contains(c) == *positive),
            // Sets already include their case closure before intersection or negation.
            Self::Set(set) => set.contains(ch),
        }
    }
}
fn accepts_folded(ch: char, fold: bool, accepts: impl Fn(char) -> bool) -> bool {
    accepts(ch) || fold && unicode::folded(ch).iter().copied().any(accepts)
}
