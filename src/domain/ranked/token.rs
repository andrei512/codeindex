use super::unicode::RANGES;
use std::ops::Range;

pub(super) const MAX_KEY: usize = 32768;

/// Emits a borrowed, normalized key and the complete span of its original spelling.
/// SQLite caps key *bytes*, including when the cap falls inside a scalar.
pub(super) fn tokenize(text: &str, emit: impl FnMut(&[u8], Range<usize>)) {
    tokenize_with(text, &mut Vec::new(), emit);
}

fn normalized(c: char) -> (u8, Option<char>) {
    if c.is_ascii() {
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
    }
}

pub(super) fn tokenize_with(
    text: &str,
    key: &mut Vec<u8>,
    mut emit: impl FnMut(&[u8], Range<usize>),
) {
    key.clear();
    let mut start = None;
    for (i, c) in text.char_indices() {
        let boundary = !append_word_char(c, i, &mut start, key);
        if let Some(begin) = start.take_if(|_| boundary) {
            emit(key, begin..i);
            key.clear();
        }
    }
    if let Some(begin) = start {
        emit(key, begin..text.len());
    }
}

fn append_word_char(c: char, at: usize, start: &mut Option<usize>, key: &mut Vec<u8>) -> bool {
    let (kind, folded) = normalized(c);
    let continues_word = kind == 2 && start.is_some();
    if kind == 1 || continues_word {
        start.get_or_insert(at);
        append_scalar(key, folded);
        true
    } else {
        false
    }
}

#[inline]
fn append_scalar(key: &mut Vec<u8>, folded: Option<char>) {
    let Some(folded) = folded.filter(|_| key.len() < MAX_KEY) else { return };
    let mut buffer = [0; 4];
    let bytes = folded.encode_utf8(&mut buffer).as_bytes();
    key.extend_from_slice(&bytes[..bytes.len().min(MAX_KEY - key.len())]);
}

/// Camel-case transitions and underscore separators share the same part ranges
/// in the ASCII and Unicode paths. Each scanner uses its own character predicates.
#[derive(Default)]
struct Parts {
    begin: usize,
}
impl Parts {
    fn split(&mut self, at: usize, underscore: bool) -> Option<Range<usize>> {
        let range = self.begin..at;
        self.begin = at + usize::from(underscore);
        (!range.is_empty()).then_some(range)
    }
    fn tail(self, end: usize) -> Option<Range<usize>> {
        (self.begin > 0 && self.begin < end).then_some(self.begin..end)
    }
}
/// Identifier splitting follows Rust's character predicates, independently of
/// unicode61's frozen token character set. Each emitted part borrows the source.
pub(super) fn identifier_parts(text: &str, mut emit: impl FnMut(usize, &str)) {
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let base = word.as_ptr() as usize - text.as_ptr() as usize;
        unicode_parts(word, |part| emit(base + part.start, &word[part]));
    }
}
/// An upper-case letter follows a lower-case letter or ends an acronym
/// immediately before a lower-case letter (as in HTTPServer).
fn starts_camel_part<T: Copy>(
    previous: T,
    next: Option<T>,
    upper: impl Fn(T) -> bool,
    lower: impl Fn(T) -> bool,
) -> bool {
    lower(previous) || upper(previous) && next.is_some_and(lower)
}
fn unicode_parts(word: &str, mut emit: impl FnMut(Range<usize>)) {
    let mut parts = Parts::default();
    let mut previous = ' ';
    let mut chars = word.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let boundary = c == '_'
            || c.is_uppercase()
                && starts_camel_part(
                    previous,
                    chars.peek().map(|(_, ch)| *ch),
                    char::is_uppercase,
                    char::is_lowercase,
                );
        if boundary && let Some(part) = parts.split(at, c == '_') {
            emit(part);
        }
        previous = c;
    }
    if let Some(part) = parts.tail(word.len()) {
        emit(part);
    }
}

/// Scan ASCII word boundaries once for both ranked streams. ASCII punctuation
/// separates words for both tokenizers. A word touching Unicode uses each
/// tokenizer's original, independent classification and folding rules.
pub(super) fn ranked_words(text: &str, mut emit: impl FnMut(bool, &[u8], Range<usize>)) {
    let mut output = RankedTokens {
        key: Vec::new(),
        emit: &mut emit,
    };
    let mut word = |start, end, special| output.word(&text[start..end], start, special);
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
    scalar_range(text.as_bytes(), 0, PendingWord::default(), &mut word);
}

// Shared by the scalar scanner and the trailing bytes after a SIMD block.
fn word_byte(b: u8) -> bool {
    !b.is_ascii() || b.is_ascii_alphanumeric() || b == b'_'
}

fn needs_subtokens(b: u8) -> bool {
    !b.is_ascii() || b.is_ascii_uppercase() || b == b'_'
}

#[derive(Default)]
struct PendingWord {
    start: Option<usize>,
    special: bool,
}

impl PendingWord {
    fn observe(&mut self, byte: u8, at: usize, emit: &mut impl FnMut(usize, usize, bool)) {
        if word_byte(byte) {
            self.start.get_or_insert(at);
            self.special |= needs_subtokens(byte);
        } else {
            self.finish(at, emit);
        }
    }

    #[inline]
    fn finish(&mut self, at: usize, emit: &mut impl FnMut(usize, usize, bool)) {
        if let Some(start) = self.start.take() {
            emit(start, at, self.special);
            self.special = false;
        }
    }
}

/// Finish a scalar range, including a word carried in from a SIMD block.
fn scalar_range(
    bytes: &[u8],
    base: usize,
    mut pending: PendingWord,
    emit: &mut impl FnMut(usize, usize, bool),
) {
    for (i, &b) in bytes.iter().enumerate() {
        pending.observe(b, base + i, emit);
    }
    pending.finish(base + bytes.len(), emit);
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
    scalar_range(
        tail,
        blocks.len() * 16,
        PendingWord { start: in_word.then_some(start), special },
        emit,
    );
}

fn ascii_parts(
    word: &[u8],
    key: &[u8],
    start: usize,
    emit: &mut impl FnMut(bool, &[u8], Range<usize>),
) {
    let mut parts = Parts::default();
    let mut part = |range: Range<usize>| {
        let span = start + range.start..start + range.end;
        emit(true, &key[range], span);
    };
    for (i, &b) in word.iter().enumerate() {
        let boundary = b == b'_'
            || i > 0
                && b.is_ascii_uppercase()
                && starts_camel_part(
                    word[i - 1],
                    word.get(i + 1).copied(),
                    |b| b.is_ascii_uppercase(),
                    |b| b.is_ascii_lowercase(),
                );
        if boundary && let Some(range) = parts.split(i, b == b'_') {
            part(range);
        }
    }
    if let Some(range) = parts.tail(word.len()) {
        part(range);
    }
}

struct RankedTokens<'a, F> {
    key: Vec<u8>,
    emit: &'a mut F,
}
impl<F: FnMut(bool, &[u8], Range<usize>)> RankedTokens<'_, F> {
    fn folded(&mut self, text: &str, start: usize, part: bool) {
        tokenize_with(text, &mut self.key, |key, span| {
            (self.emit)(part, key, start + span.start..start + span.end)
        });
    }
    fn word(&mut self, word: &str, start: usize, camel_or_snake: bool) {
        let span = start..start + word.len();
        if !camel_or_snake {
            let bytes = word.as_bytes();
            (self.emit)(false, &bytes[..bytes.len().min(MAX_KEY)], span);
            return;
        }
        // Unicode and oversized generated identifiers use bounded scratch.
        if !word.is_ascii() || word.len() > MAX_KEY {
            self.folded(word, start, false);
            identifier_parts(word, |base, part| self.folded(part, start + base, true));
            return;
        }
        let bytes = word.as_bytes();
        self.key.clear();
        self.key.extend_from_slice(bytes);
        self.key.make_ascii_lowercase();
        (self.emit)(false, &self.key, span);
        ascii_parts(bytes, &self.key, start, self.emit);
    }
}

#[cfg(all(
    test,
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little"
))]
#[test]
fn simd_word_boundaries_match_scalar_for_every_tail_alignment() {
    for prefix in 0..16 {
        for count in [0, 1, 15, 16, 17, 31, 32] {
            let text = format!(
                "{}aBC_Δ..word{} _tail",
                ".".repeat(prefix),
                "X".repeat(count)
            );
            let mut simd = Vec::new();
            scan_neon(text.as_bytes(), &mut |a, b, flag| simd.push((a, b, flag)));
            let mut scalar = Vec::new();
            scalar_range(text.as_bytes(), 0, PendingWord::default(), &mut |a, b, flag| {
                scalar.push((a, b, flag))
            });
            assert_eq!(simd, scalar, "{text}");
        }
    }
}
