//! Within-file Jaccard similarity over multisets of consecutive two-token shingles.
//! Prefix filtering avoids comparing unrelated bodies. At 87.5% Jaccard, each sorted multiset
//! must share a shingle among its first n - ceil(7n/8) + 1 sorted elements.
use super::{Rule, Score, Token};
use std::{
    collections::{HashMap, HashSet},
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
    let mut previous: Option<u64> = None;
    for token in tokens {
        let mut hasher = DefaultHasher::new();
        token.text.hash(&mut hasher);
        let current = hasher.finish();
        if let Some(left) = previous {
            hashes.push(left.rotate_left(13) ^ current);
        }
        previous = Some(current);
    }
    hashes.sort_unstable();
    hashes
}
fn similar(a: &[u64], b: &[u64]) -> bool {
    let need = (7 * (a.len() + b.len())).div_ceil(15);
    let (mut i, mut j, mut common) = (0, 0, 0);
    while common + (a.len() - i).min(b.len() - j) >= need && i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
                if common >= need {
                    return true;
                }
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    false
}
pub(super) fn record(score: &mut Score, t: &[Token], bodies: &[Body]) {
    if bodies.len() < 2 {
        return;
    }
    let mut postings: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut signatures = Vec::<Vec<u64>>::with_capacity(bodies.len());
    let mut matched = vec![false; bodies.len()];
    let mut seen = HashSet::new();
    for (i, body) in bodies.iter().enumerate() {
        let sig = signature(&t[body.tokens.clone()]);
        if !sig.is_empty() {
            let prefix = &sig[..sig.len() / 8 + 1];
            seen.clear();
            for (j, key) in prefix.iter().enumerate() {
                if j > 0 && prefix[j - 1] == *key {
                    continue;
                }
                if let Some(ids) = postings.get(key) {
                    for &id in ids {
                        if matched[i] && matched[id] || !seen.insert(id) {
                            continue;
                        }
                        let other = &bodies[id].tokens;
                        if other.start < body.tokens.end && other.end > body.tokens.start {
                            continue;
                        }
                        if similar(&sig, &signatures[id]) {
                            matched[i] = true;
                            matched[id] = true;
                        }
                    }
                }
            }
            for (j, &key) in prefix.iter().enumerate() {
                if j == 0 || prefix[j - 1] != key {
                    postings.entry(key).or_default().push(i);
                }
            }
        }
        signatures.push(sig);
    }
    for (body, hit) in bodies.iter().zip(matched) {
        if hit {
            score.record(Rule::Duplication, body.lines, body.line, body.name);
        }
    }
}
