//! Hash a short header and an immutable payload without copying the full payload.
use super::provenance::BlobId;

fn blocks(header: &[u8], mut data: &[u8], mut push: impl FnMut(&[u8])) {
    let total = header.len() as u64 + data.len() as u64;
    let mut tail = [0; 128];
    let mut used = header.len();
    tail[..used].copy_from_slice(header);
    if used != 0 && data.len() >= 64 - used {
        let take = 64 - used;
        tail[used..64].copy_from_slice(&data[..take]);
        push(&tail[..64]);
        data = &data[take..];
        used = 0;
    }
    let mut chunks = data.chunks_exact(64);
    for chunk in &mut chunks {
        push(chunk);
    }
    let end = used + chunks.remainder().len();
    tail[used..end].copy_from_slice(chunks.remainder());
    let padded = if end < 56 { 64 } else { 128 };
    tail[end..padded].fill(0);
    tail[end] = 0x80;
    tail[padded - 8..padded].copy_from_slice(&total.wrapping_mul(8).to_be_bytes());
    for block in tail[..padded].chunks_exact(64) {
        push(block);
    }
}

pub(super) fn digest(header: &[u8], data: &[u8], sha256: bool) -> BlobId {
    let mut bytes = [0; 32];
    if sha256 {
        let mut state: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        blocks(header, data, |block| sha256_block(&mut state, block));
        write_words(&state, &mut bytes);
    } else {
        let mut state = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
        sha1_blocks(&mut state, header, data);
        write_words(&state, &mut bytes);
    }
    BlobId {
        bytes,
        len: if sha256 { 32 } else { 20 },
    }
}

#[inline]
fn write_words(words: &[u32], bytes: &mut [u8; 32]) {
    for (word, out) in words.iter().zip(bytes.as_chunks_mut::<4>().0) {
        *out = word.to_be_bytes();
    }
}

fn sha1_blocks(state: &mut [u32; 5], header: &[u8], data: &[u8]) {
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("sha2") {
        // Runtime dispatch happens once per file, outside the compression loop.
        unsafe { arm_sha1_blocks(state, header, data) }
        return;
    }
    blocks(header, data, |block| sha1_block(state, block))
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "sha2")]
unsafe fn arm_sha1_blocks(state: &mut [u32; 5], header: &[u8], data: &[u8]) {
    use std::arch::aarch64::*;
    let mut abcd = unsafe { vld1q_u32(state.as_ptr()) };
    let mut e = state[4];
    let k = [0x5a827999, 0x6ed9eba1, 0x8f1bbcdc, 0xca62c1d6].map(|n| vdupq_n_u32(n));
    blocks(header, data, |block| {
        let (initial_abcd, initial_e) = (abcd, e);
        let mut words: [uint32x4_t; 4] = std::array::from_fn(|i| {
            let raw = unsafe { vld1q_u8(block.as_ptr().add(i * 16)) };
            vreinterpretq_u32_u8(vrev32q_u8(raw))
        });
        (abcd, e) = unsafe { arm_sha1_rounds((abcd, e), &mut words, k) };
        abcd = vaddq_u32(abcd, initial_abcd);
        e = e.wrapping_add(initial_e);
    });
    unsafe { vst1q_u32(state.as_mut_ptr(), abcd) }
    state[4] = e;
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "sha2")]
#[inline]
unsafe fn arm_sha1_schedule_next(words: &mut [std::arch::aarch64::uint32x4_t; 4], group: usize) {
    use std::arch::aarch64::*;
    if group < 16 {
        let lane = group % 4;
        let start = vsha1su0q_u32(words[lane], words[(lane + 1) % 4], words[(lane + 2) % 4]);
        words[lane] = vsha1su1q_u32(start, words[(lane + 3) % 4]);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "sha2")]
#[inline]
unsafe fn arm_sha1_rounds(
    (mut abcd, mut e): (std::arch::aarch64::uint32x4_t, u32),
    words: &mut [std::arch::aarch64::uint32x4_t; 4],
    k: [std::arch::aarch64::uint32x4_t; 4],
) -> (std::arch::aarch64::uint32x4_t, u32) {
    use std::arch::aarch64::*;
    for group in 0..20 {
        let lane = group % 4;
        let wk = vaddq_u32(words[lane], k[group / 5]);
        unsafe { arm_sha1_schedule_next(words, group) };
        let next_e = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
        abcd = match group / 5 {
            0 => vsha1cq_u32(abcd, e, wk),
            2 => vsha1mq_u32(abcd, e, wk),
            _ => vsha1pq_u32(abcd, e, wk),
        };
        e = next_e;
    }
    (abcd, e)
}

fn sha1_schedule(block: &[u8]) -> [u32; 80] {
    let mut w = [0u32; 80];
    for (word, raw) in w.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes(raw.try_into().unwrap());
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    w
}

fn sha1_block(state: &mut [u32; 5], block: &[u8]) {
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (i, word) in sha1_schedule(block).into_iter().enumerate() {
        let (f, k) = match i {
            0..20 => ((b & c) | (!b & d), 0x5a827999),
            20..40 => (b ^ c ^ d, 0x6ed9eba1),
            40..60 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
            _ => (b ^ c ^ d, 0xca62c1d6),
        };
        let next = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    for (x, y) in state.iter_mut().zip([a, b, c, d, e]) {
        *x = x.wrapping_add(y);
    }
}

fn sha256_block(state: &mut [u32; 8], block: &[u8]) {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut w = [0u32; 64];
    for (i, b) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes(b.try_into().unwrap())
    }
    for i in 16..64 {
        let x = w[i - 15];
        let y = w[i - 2];
        w[i] = w[i - 16]
            .wrapping_add(x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3))
            .wrapping_add(w[i - 7])
            .wrapping_add(y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10));
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let t = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let u = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t);
        d = c;
        c = b;
        b = a;
        a = t.wrapping_add(u);
    }
    for (x, y) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *x = x.wrapping_add(y)
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
#[test]
fn arm_and_portable_sha1_match_at_padding_boundaries() {
    if !std::arch::is_aarch64_feature_detected!("sha2") {
        return;
    }
    let data: Vec<_> = (0..4096).map(|i| (i * 127) as u8).collect();
    for len in (0..145).chain([255, 1023, 4096]) {
        let mut portable = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
        let mut accelerated = portable;
        let header = format!("blob {len}\0");
        blocks(header.as_bytes(), &data[..len], |b| {
            sha1_block(&mut portable, b)
        });
        unsafe { arm_sha1_blocks(&mut accelerated, header.as_bytes(), &data[..len]) }
        assert_eq!(accelerated, portable, "length {len}");
    }
}
