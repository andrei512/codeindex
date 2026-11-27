//! Canonical code-point intervals for POSIX and byte-mode Perl classes.
use super::Set;

const DIGIT: (u32, u32) = (48, 57);
const UPPER: (u32, u32) = (65, 90);
const LOWER: (u32, u32) = (97, 122);
const SPACE: &[(u32, u32)] = &[(9, 13), (32, 32)];
const WORD: &[(u32, u32)] = &[DIGIT, UPPER, (95, 95), LOWER];

const POSIX: &[(&str, &[(u32, u32)])] = &[
    ("alnum", &[DIGIT, UPPER, LOWER]),
    ("alpha", &[UPPER, LOWER]),
    ("ascii", &[(0, 127)]),
    ("blank", &[(9, 9), (32, 32)]),
    ("cntrl", &[(0, 31), (127, 127)]),
    ("digit", &[DIGIT]),
    ("graph", &[(33, 126)]),
    ("lower", &[LOWER]),
    ("print", &[(32, 126)]),
    ("punct", &[(33, 47), (58, 64), (91, 96), (123, 126)]),
    ("space", SPACE),
    ("upper", &[UPPER]),
    ("word", WORD),
    ("xdigit", &[DIGIT, (65, 70), (97, 102)]),
];

pub(super) fn posix(name: &str) -> Option<Set> {
    let index = POSIX.binary_search_by_key(&name, |row| row.0).ok()?;
    Some(Set(POSIX[index].1.into()))
}

pub(super) fn perl(key: char) -> Set {
    let ranges = match key {
        'w' => WORD,
        's' => SPACE,
        'd' => &[DIGIT],
        _ => unreachable!("Perl character class"),
    };
    Set(ranges.into())
}

pub(super) fn literal(c: char) -> Set {
    let lower = c.to_ascii_lowercase() as u32;
    let upper = c.to_ascii_uppercase() as u32;
    if lower == upper {
        Set([(lower, lower)].into())
    } else {
        Set([(upper, upper), (lower, lower)].into())
    }
}

pub(super) fn fold(set: Set) -> Set {
    let mut pairs = set.0.to_vec();
    for &range in set.0.iter() {
        pairs.extend(paired_case(range, UPPER, LOWER.0));
        pairs.extend(paired_case(range, LOWER, UPPER.0));
    }
    Set::ranges(pairs)
}

fn paired_case(
    (start, end): (u32, u32),
    (case_start, case_end): (u32, u32),
    paired_start: u32,
) -> Option<(u32, u32)> {
    let lo = start.max(case_start);
    let hi = end.min(case_end);
    (lo <= hi).then(|| (paired_start + (lo - case_start), paired_start + (hi - case_start)))
}
