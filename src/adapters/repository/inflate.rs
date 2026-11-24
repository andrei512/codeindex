//! RFC 1950/1951 decoding for tree objects in a materialized sparse index.
//! Ordinary directory discovery never opens a Git object.
use super::invalid;
use std::io;
const MAX_TREE: usize = 256 << 20;
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}
impl Bits<'_> {
    fn read(&mut self, n: usize) -> io::Result<usize> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| invalid("bit offset overflow"))?;
        if end.div_ceil(8) > self.data.len() {
            return Err(invalid("truncated deflate data"));
        }
        let mut v = 0;
        for bit in 0..n {
            v |= usize::from((self.data[self.pos / 8] >> (self.pos % 8)) & 1) << bit;
            self.pos += 1
        }
        Ok(v)
    }
}
struct Huffman {
    fast: [u16; 256],
    counts: [usize; 16],
    symbols: Vec<usize>,
}
impl Huffman {
    fn new(lens: &[usize]) -> io::Result<Self> {
        let mut counts = [0; 16];
        for &n in lens {
            if n > 15 {
                return Err(invalid("invalid Huffman length"));
            }
            counts[n] += 1
        }
        let mut slots = 1usize;
        for &count in &counts[1..] {
            slots = (2 * slots)
                .checked_sub(count)
                .ok_or_else(|| invalid("oversubscribed Huffman tree"))?
        }
        let mut starts = [0usize; 16];
        let mut first = [0usize; 16];
        for n in 2..=15 {
            starts[n] = starts[n - 1] + counts[n - 1];
            first[n] = (first[n - 1] + counts[n - 1]) << 1;
        }
        let mut symbols = vec![0; lens.len() - counts[0]];
        let mut fast = [0; 256];
        for (symbol, &n) in lens.iter().enumerate() {
            if n == 0 {
                continue;
            }
            symbols[starts[n]] = symbol;
            starts[n] += 1;
            if n <= 8 {
                let code = (first[n] as u8).reverse_bits() >> (8 - n);
                for p in (usize::from(code)..fast.len()).step_by(1 << n) {
                    fast[p] = ((symbol as u16) << 4) | n as u16;
                }
            }
            first[n] += 1;
        }
        Ok(Self {
            fast,
            counts,
            symbols,
        })
    }
    fn read(&self, b: &mut Bits) -> io::Result<usize> {
        let byte = b.pos >> 3;
        if let Some(window) = b.data.get(byte..byte + 2) {
            let key = (u16::from_le_bytes([window[0], window[1]]) >> (b.pos % 8)) as u8;
            let entry = self.fast[key as usize];
            if entry != 0 {
                b.pos += usize::from(entry & 15);
                return Ok(usize::from(entry >> 4));
            }
        }
        let (mut code, mut first, mut start) = (0, 0, 0);
        for n in 1..=15 {
            code = 2 * code + b.read(1)?;
            let len = self.counts[n];
            if code < first + len {
                return self
                    .symbols
                    .get(start + code - first)
                    .copied()
                    .ok_or_else(|| invalid("invalid Huffman symbol"));
            }
            start += len;
            first = (first + len) * 2;
        }
        Err(invalid("invalid Huffman prefix"))
    }
}
fn dynamic(bits: &mut Bits) -> io::Result<(Huffman, Huffman)> {
    let nl = bits.read(5)? + 257;
    let nd = bits.read(5)? + 1;
    let nc = bits.read(4)? + 4;
    let mut lengths = [0; 19];
    for &i in &[
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ][..nc]
    {
        lengths[i] = bits.read(3)?
    }
    let code = Huffman::new(&lengths)?;
    let mut lens = Vec::with_capacity(nl + nd);
    while lens.len() < nl + nd {
        let symbol = code.read(bits)?;
        let (v, n) = match symbol {
            16 => (
                *lens
                    .last()
                    .ok_or_else(|| invalid("first length repeated"))?,
                bits.read(2)? + 3,
            ),
            17 => (0, bits.read(3)? + 3),
            18 => (0, bits.read(7)? + 11),
            _ => (symbol, 1),
        };
        if lens.len() + n > nl + nd {
            return Err(invalid("excess Huffman lengths"));
        }
        lens.resize(lens.len() + n, v);
    }
    Ok((Huffman::new(&lens[..nl])?, Huffman::new(&lens[nl..])?))
}
fn block(
    bits: &mut Bits,
    out: &mut Vec<u8>,
    codes: &Huffman,
    dist: &Huffman,
    limit: usize,
) -> io::Result<()> {
    const BASE: [usize; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    loop {
        let s = codes.read(bits)?;
        match s {
            0..=255 => {
                if out.len() >= limit {
                    return Err(invalid("Git tree size limit"));
                }
                out.push(s as u8)
            }
            256 => break,
            257..=285 => {
                let i = s - 257;
                let extra = if i == 28 { 0 } else { i.saturating_sub(4) / 4 };
                let len = BASE[i] + bits.read(extra)?;
                let d = dist.read(bits)?;
                if d > 29 {
                    return Err(invalid("bad deflate distance"));
                }
                let extra = (d / 2).saturating_sub(1);
                let start = if d < 4 {
                    d + 1
                } else {
                    ((2 + (d & 1)) << extra) + 1
                };
                let gap = start + bits.read(extra)?;
                if gap > out.len() || out.len() + len > limit {
                    return Err(invalid("invalid Git tree deflate copy"));
                }
                for _ in 0..len {
                    out.push(out[out.len() - gap])
                }
            }
            _ => return Err(invalid("invalid deflate code")),
        }
    }
    Ok(())
}
pub(super) fn zlib(data: &[u8], size: Option<usize>) -> io::Result<Vec<u8>> {
    if data.len() < 6
        || data[0] & 15 != 8
        || data[0] >> 4 > 7
        || usize::from(u16::from_be_bytes([data[0], data[1]])) % 31 != 0
        || data[1] & 32 != 0
    {
        return Err(invalid("invalid zlib header"));
    }
    let limit = size.unwrap_or(MAX_TREE);
    if limit > MAX_TREE {
        return Err(invalid("Git tree size limit"));
    }
    let mut bits = Bits {
        data: &data[2..],
        pos: 0,
    };
    let mut out = Vec::with_capacity(limit.min(8192));
    loop {
        let last = bits.read(1)? == 1;
        match bits.read(2)? {
            0 => {
                bits.pos = bits.pos.next_multiple_of(8);
                let n = bits.read(16)?;
                if bits.read(16)? != n ^ 65535 || out.len() + n > limit {
                    return Err(invalid("invalid uncompressed block"));
                }
                for _ in 0..n {
                    out.push(bits.read(8)? as u8)
                }
            }
            1 => {
                static FIXED: std::sync::OnceLock<(Huffman, Huffman)> = std::sync::OnceLock::new();
                let (codes, dist) = FIXED.get_or_init(|| {
                    let mut lens = [8; 288];
                    lens[144..256].fill(9);
                    lens[256..280].fill(7);
                    (
                        Huffman::new(&lens).unwrap(),
                        Huffman::new(&[5; 32]).unwrap(),
                    )
                });
                block(&mut bits, &mut out, codes, dist, limit)?
            }
            2 => {
                let (code, dist) = dynamic(&mut bits)?;
                block(&mut bits, &mut out, &code, &dist, limit)?
            }
            _ => return Err(invalid("reserved deflate block")),
        }
        if last {
            break;
        }
    }
    let end = 2 + bits.pos.div_ceil(8);
    let check = data
        .get(end..end + 4)
        .ok_or_else(|| invalid("missing zlib checksum"))?;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in out.chunks(5552) {
        for &c in chunk {
            a += u32::from(c);
            b += a
        }
        a %= 65521;
        b %= 65521
    }
    if check != (b << 16 | a).to_be_bytes() || size.is_some_and(|s| s != out.len()) {
        return Err(invalid("Git tree checksum or size mismatch"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_table_entries_and_long_fallback_agree_at_every_bit_offset() {
        let lengths = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 15];
        let encoder = Huffman::new(&lengths).unwrap();
        for (symbol, &length) in lengths.iter().enumerate() {
            let code = if symbol + 1 == lengths.len() {
                (1usize << length) - 1
            } else {
                (1usize << length) - 2
            };
            for skip in 0..8 {
                let mut buf = [0u8; 4];
                for i in 0..length {
                    buf[(skip + i) / 8] |=
                        (((code >> (length - 1 - i)) & 1) as u8) << ((skip + i) % 8);
                }
                let mut bits = Bits {
                    data: &buf,
                    pos: skip,
                };
                assert_eq!(encoder.read(&mut bits).unwrap(), symbol);
                assert_eq!(bits.pos, skip + length);
            }
        }
    }
}
