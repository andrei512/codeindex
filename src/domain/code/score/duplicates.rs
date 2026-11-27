//! Within-file Jaccard similarity over multisets of consecutive two-token shingles.
//! Prefix filtering avoids comparing unrelated bodies. At 87.5% Jaccard, each sorted multiset
//! must share a shingle among its first n - ceil(7n/8) + 1 sorted elements.
use super::{Rule, Score, Token};
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
};

pub(super) struct Body<'a> {
    pub name: &'a str,
    pub line: usize,
    pub lines: usize,
    pub tokens: Range<usize>,
}
fn signature(tokens: &[Token]) -> Vec<u64> {
    let mut hashes = Vec::with_capacity(tokens.len().saturating_sub(1));
    let mut token_hashes = tokens.iter().map(|token| {
        let mut hasher = DefaultHasher::new();
        token.text.hash(&mut hasher);
        hasher.finish()
    });
    let Some(mut previous) = token_hashes.next() else {
        return hashes;
    };
    for current in token_hashes {
        hashes.push(previous.rotate_left(13) ^ current);
        previous = current;
    }
    hashes.sort_unstable();
    hashes
}
fn similar(a: &[u64], b: &[u64]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let need = (7 * (a.len() + b.len())).div_ceil(15);
    let (mut i, mut j, mut common) = (0, 0, 0);
    while common < need && common + (a.len() - i).min(b.len() - j) >= need {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    common >= need
}
/// The first posting for each distinct prefix shingle is enough to propose a pair.
fn prefix_keys(signature: &[u64]) -> impl Iterator<Item = u64> + '_ {
    let length = (signature.len() / 8 + 1).min(signature.len());
    signature[..length]
        .chunk_by(|a, b| a == b)
        .map(|group| group[0])
}

/// IDs before `settled` already have a witness. A new body needs at most
/// one of those witnesses, but every remaining body still deserves a check.
#[derive(Default)]
struct Posting {
    ids: Vec<usize>,
    settled: usize,
}

impl Posting {
    fn compare(
        &mut self,
        current: usize,
        matched: &mut [bool],
        mut similar: impl FnMut(usize) -> bool,
    ) {
        if !matched[current] {
            matched[current] = self.ids[..self.settled].iter().copied().any(&mut similar);
        }
        for index in self.settled..self.ids.len() {
            let id = self.ids[index];
            compare_unsettled(current, id, matched, &mut similar);
            if matched[id] {
                self.ids.swap(index, self.settled);
                self.settled += 1;
            }
        }
        self.ids.push(current);
    }
}

fn compare_unsettled(
    current: usize,
    id: usize,
    matched: &mut [bool],
    similar: impl FnOnce(usize) -> bool,
) {
    let known = matched[current] && matched[id];
    if !known && similar(id) {
        matched[current] = true;
        matched[id] = true;
    }
}

/// A body is eligible only if the earliest end or latest start leaves
/// space for a non-overlapping witness. Enclosing bodies cannot match.
fn candidates<'a, 'text>(bodies: &'a [Body<'text>]) -> Vec<&'a Body<'text>> {
    let (first_end, last_start) = bodies.iter().fold((usize::MAX, 0), |(end, start), body| {
        (end.min(body.tokens.end), start.max(body.tokens.start))
    });
    bodies
        .iter()
        .filter(|body| body.tokens.end <= last_start || body.tokens.start >= first_end)
        .collect()
}

pub(super) fn record(score: &mut Score, t: &[Token], bodies: &[Body]) {
    if bodies.len() < 2 {
        return;
    }
    let bodies = candidates(bodies);
    if bodies.is_empty() {
        return;
    }
    let matched = match_candidates(t, &bodies);
    for (body, hit) in bodies.iter().zip(matched) {
        if hit {
            score.record(Rule::Duplication, body.lines, body.line, body.name);
        }
    }
}

fn match_candidates(t: &[Token], bodies: &[&Body]) -> Vec<bool> {
    let mut matcher = Matcher::new(bodies.len());
    for (i, body) in bodies.iter().enumerate() {
        let sig = signature(&t[body.tokens.clone()]);
        matcher.compare(bodies, i, sig);
    }
    matcher.matched
}

struct Matcher {
    postings: HashMap<u64, Posting>,
    signatures: Vec<Vec<u64>>,
    matched: Vec<bool>,
    seen: Vec<usize>,
}

impl Matcher {
    fn new(count: usize) -> Self {
        Self {
            postings: HashMap::new(),
            signatures: Vec::with_capacity(count),
            matched: vec![false; count],
            seen: vec![usize::MAX; count],
        }
    }

    fn compare(&mut self, bodies: &[&Body], i: usize, signature: Vec<u64>) {
        let current = &bodies[i].tokens;
        let mut compare = |id: usize| {
            if self.seen[id] == i {
                return false;
            }
            self.seen[id] = i;
            disjoint(&bodies[id].tokens, current) && similar(&signature, &self.signatures[id])
        };
        for key in prefix_keys(&signature) {
            self.postings
                .entry(key)
                .or_default()
                .compare(i, &mut self.matched, &mut compare);
        }
        self.signatures.push(signature);
    }
}

fn disjoint(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.end <= b.start || b.end <= a.start
}
