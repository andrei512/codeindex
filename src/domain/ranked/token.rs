use super::unicode::RANGES;
use std::ops::Range;

const MAX_KEY: usize = 32768;

/// Emits a borrowed, normalized key and the complete span of its original spelling.
/// SQLite caps key *bytes*, including when the cap falls inside a scalar.
pub(super) fn tokenize(text: &str, emit: impl FnMut(&[u8], Range<usize>)) {
    tokenize_with(text, &mut Vec::new(), emit);
}

pub(super) fn tokenize_with(
    text: &str,
    key: &mut Vec<u8>,
    mut emit: impl FnMut(&[u8], Range<usize>),
) {
    key.clear();
    let mut start = None;
    for (i, c) in text.char_indices() {
        let (kind, folded) = if c.is_ascii() {
            (
                u8::from(c.is_ascii_alphanumeric() || c == '_'),
                Some(c.to_ascii_lowercase()),
            )
        } else {
            let scalar = match c as u32 {
                0xfffe | 0xffff => 0xfffd,
                n => n,
            };
            let index = RANGES.partition_point(|&(lower, _, _)| lower <= scalar) - 1;
            let (_, kind, delta) = RANGES[index];
            (kind, char::from_u32((scalar as i32 + delta) as u32))
        };
        if kind == 1 || (kind == 2 && start.is_some()) {
            start.get_or_insert(i);
            if let Some(folded) = folded.filter(|_| key.len() < MAX_KEY) {
                let mut buffer = [0; 4];
                let bytes = folded.encode_utf8(&mut buffer).as_bytes();
                key.extend_from_slice(&bytes[..bytes.len().min(MAX_KEY - key.len())]);
            }
        } else if let Some(begin) = start.take() {
            emit(key, begin..i);
            key.clear();
        }
    }
    if let Some(begin) = start {
        emit(key, begin..text.len());
    }
}

/// Identifier splitting follows Rust's character predicates, independently of
/// unicode61's frozen token character set. Each emitted part borrows the source.
pub(super) fn identifier_parts(text: &str, mut emit: impl FnMut(usize, &str)) {
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let base = word.as_ptr() as usize - text.as_ptr() as usize;
        let mut start = 0;
        let mut previous = ' ';
        let mut chars = word.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c == '_'
                || (c.is_uppercase()
                    && (previous.is_lowercase()
                        || (previous.is_uppercase()
                            && chars.peek().is_some_and(|(_, next)| next.is_lowercase()))))
            {
                if at > start {
                    emit(base + start, &word[start..at]);
                }
                start = at + usize::from(c == '_');
            }
            previous = c;
        }
        if start > 0 && start < word.len() {
            emit(base + start, &word[start..]);
        }
    }
}

/// Scan ASCII word boundaries once for both ranked streams. ASCII punctuation
/// separates words for both tokenizers. A word touching Unicode uses each
/// tokenizer's original, independent classification and folding rules.
pub(super) fn ranked_words(text: &str, mut emit: impl FnMut(bool, &[u8], Range<usize>)) {
    let mut key = Vec::new();
    let mut word =
        |start, end, special| emit_word(&text[start..end], start, special, &mut key, &mut emit);
    #[cfg(all(
        target_arch = "aarch64",
        target_feature = "neon",
        target_endian = "little"
    ))]
    scan_neon(text.as_bytes(), &mut word);
    #[cfg(not(all(
        target_arch = "aarch64",
        target_feature = "neon",
        target_endian = "little"
    )))]
    scan_scalar(text, &mut word);
}

#[cfg(not(all(
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little"
)))]
fn scan_scalar(text: &str, emit: &mut impl FnMut(usize, usize, bool)) {
    let bytes = text.as_bytes();

    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii()
            && !bytes[cursor].is_ascii_alphanumeric()
            && bytes[cursor] != b'_'
        {
            cursor += 1;
            continue;
        }
        let start = cursor;

        let mut camel_or_snake = false;
        while cursor < bytes.len() {
            let c = bytes[cursor];
            if c.is_ascii() && !c.is_ascii_alphanumeric() && c != b'_' {
                break;
            }

            camel_or_snake |= !c.is_ascii() || c == b'_' || c.is_ascii_uppercase();
            cursor += 1;
        }
        emit(start, cursor, camel_or_snake);
    }
}

/// One bit per nibble marks the sixteen input bytes. Little-endian lane order
/// lets trailing_zeros identify the next boundary without a byte-at-a-time loop.
#[cfg(all(
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little"
))]
#[inline]
fn masks(block: &[u8; 16]) -> (u64, u64) {
    use std::arch::aarch64::*;
    // The target provides NEON and the load is exactly one complete block.
    unsafe {
        let c = vld1q_u8(block.as_ptr());
        let range = |v, base, width| vcleq_u8(vsubq_u8(v, vdupq_n_u8(base)), vdupq_n_u8(width));
        let upper = range(c, b'A', 25);
        let high = vcgeq_u8(c, vdupq_n_u8(128));
        let special = vorrq_u8(vorrq_u8(high, upper), vceqq_u8(c, vdupq_n_u8(b'_')));
        let word = vorrq_u8(special, vorrq_u8(range(c, b'a', 25), range(c, b'0', 9)));
        let bits = |v| {
            vget_lane_u64(
                vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(v))),
                0,
            ) & 0x1111_1111_1111_1111
        };
        (bits(word), bits(special))
    }
}
#[cfg(all(
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little"
))]
fn scan_neon(bytes: &[u8], emit: &mut impl FnMut(usize, usize, bool)) {
    let mut start = 0;
    let mut in_word = false;
    let mut special = false;
    let (blocks, tail) = bytes.as_chunks::<16>();
    for (index, block) in blocks.iter().enumerate() {
        let base = index * 16;
        let (words, mut flagged) = masks(block);
        let mut edges = words ^ ((words << 4) | u64::from(in_word));
        while edges != 0 {
            let edge = edges & edges.wrapping_neg();
            let at = base + edge.trailing_zeros() as usize / 4;
            if in_word {
                emit(start, at, special || (flagged & (edge - 1)) != 0);
                flagged &= !(edge - 1);
                special = false;
            } else {
                start = at;
            }
            in_word = !in_word;
            edges ^= edge;
        }
        special |= flagged != 0;
    }
    for (i, &b) in tail.iter().enumerate() {
        let at = blocks.len() * 16 + i;
        let word = !b.is_ascii() || b.is_ascii_alphanumeric() || b == b'_';
        if word {
            if !in_word {
                start = at;
            }
            special |= !b.is_ascii() || b.is_ascii_uppercase() || b == b'_';
        } else if in_word {
            emit(start, at, special);
            special = false;
        }
        in_word = word;
    }
    if in_word {
        emit(start, bytes.len(), special);
    }
}

fn emit_word(
    word: &str,
    start: usize,
    camel_or_snake: bool,
    key: &mut Vec<u8>,
    emit: &mut impl FnMut(bool, &[u8], Range<usize>),
) {
    let cursor = start + word.len();
    // Keep scratch bounded for generated identifiers that occupy most of a file.
    if (camel_or_snake && !word.is_ascii()) || (camel_or_snake && word.len() > MAX_KEY) {
        tokenize_with(word, key, |k, span| {
            emit(false, k, start + span.start..start + span.end)
        });
        identifier_parts(word, |base, part| {
            tokenize_with(part, key, |k, span| {
                emit(true, k, start + base + span.start..start + base + span.end)
            })
        });
        return;
    }
    let word = word.as_bytes();
    if !camel_or_snake {
        emit(false, &word[..word.len().min(MAX_KEY)], start..cursor);
        return;
    }
    key.clear();
    key.extend_from_slice(word);
    key.make_ascii_lowercase();
    emit(false, &key[..key.len().min(MAX_KEY)], start..cursor);
    let mut begin = 0;
    for (i, &b) in word.iter().enumerate() {
        if b == b'_'
            || (i > 0
                && b.is_ascii_uppercase()
                && (word[i - 1].is_ascii_lowercase()
                    || (word[i - 1].is_ascii_uppercase()
                        && word.get(i + 1).is_some_and(u8::is_ascii_lowercase))))
        {
            if begin < i {
                emit(
                    true,
                    &key[begin..i.min(begin + MAX_KEY)],
                    start + begin..start + i,
                );
            }
            begin = i + usize::from(b == b'_');
        }
    }
    if begin > 0 && begin < word.len() {
        emit(
            true,
            &key[begin..key.len().min(begin + MAX_KEY)],
            start + begin..cursor,
        );
    }
}
