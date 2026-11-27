//! RFC 1950/1951 decoding for tree objects in a materialized sparse index.
//! Ordinary directory discovery never opens a Git object.
use super::invalid;
use std::io;
const MAX_TREE: usize = 256 << 20;
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Bits<'a> {
    fn from_zlib(data: &'a [u8]) -> io::Result<Self> {
        if data.len() < 6
            || data[0] & 15 != 8
            || data[0] >> 4 > 7
            || usize::from(u16::from_be_bytes([data[0], data[1]])) % 31 != 0
            || data[1] & 32 != 0
        {
            return Err(invalid("invalid zlib header"));
        }
        Ok(Self {
            data: &data[2..],
            pos: 0,
        })
    }
    fn block(&mut self, out: &mut Vec<u8>, limit: usize) -> io::Result<bool> {
        let last = self.read(1)? == 1;
        match self.read(2)? {
            0 => self.stored(out, limit)?,
            1 => Codes::fixed().block(self, out, limit)?,
            2 => Codes::dynamic(self)?.block(self, out, limit)?,
            _ => return Err(invalid("reserved deflate block")),
        }
        Ok(last)
    }
}
impl Bits<'_> {
    fn stored(&mut self, out: &mut Vec<u8>, limit: usize) -> io::Result<()> {
        self.pos = self.pos.next_multiple_of(8);
        let n = self.read(16)?;
        if self.read(16)? != n ^ 65535 || out.len() + n > limit {
            return Err(invalid("invalid uncompressed block"));
        }
        let start = self.pos / 8;
        let bytes = self
            .data
            .get(start..start + n)
            .ok_or_else(|| invalid("truncated deflate data"))?;
        out.extend_from_slice(bytes);
        self.pos += n * 8;
        Ok(())
    }

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
        let counts = lens.iter().try_fold([0; 16], |mut counts, &length| {
            *counts.get_mut(length).ok_or_else(|| invalid("invalid Huffman length"))? += 1;
            Ok::<_, io::Error>(counts)
        })?;
        let (mut starts, mut first, used) = Self::canonical(&counts)?;
        let mut symbols = vec![0; used];
        let mut fast = [0; 256];
        for (symbol, &n) in lens.iter().enumerate() {
            if n == 0 {
                continue;
            }
            symbols[starts[n]] = symbol;
            starts[n] += 1;
            Self::fill_short(&mut fast, first[n], symbol, n);
            first[n] += 1;
        }
        Ok(Self {
            fast,
            counts,
            symbols,
        })
    }
    fn canonical(counts: &[usize; 16]) -> io::Result<([usize; 16], [usize; 16], usize)> {
        // Each length starts after the previous length's canonical codes.
        let mut starts = [0; 16];
        let mut first = [0; 16];
        let (mut code, mut used) = (0, 0);
        for n in 1..=15 {
            starts[n] = used;
            first[n] = code;
            code += counts[n];
            if code > 1 << n {
                return Err(invalid("oversubscribed Huffman tree"));
            }
            used += counts[n];
            code <<= 1;
        }
        Ok((starts, first, used))
    }

    fn fill_short(fast: &mut [u16; 256], canonical: usize, symbol: usize, n: usize) {
        if n > 8 {
            return;
        }
        let code = (canonical as u8).reverse_bits() >> (8 - n);
        let entry = ((symbol as u16) << 4) | n as u16;
        for p in (usize::from(code)..fast.len()).step_by(1 << n) {
            fast[p] = entry;
        }
    }

    fn read(&self, b: &mut Bits) -> io::Result<usize> {
        if let Some(symbol) = self.read_short(b) {
            return Ok(symbol);
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
    fn read_short(&self, bits: &mut Bits) -> Option<usize> {
        let byte = bits.pos >> 3;
        let window = bits.data.get(byte..byte + 2)?;
        let key = (u16::from_le_bytes([window[0], window[1]]) >> (bits.pos % 8)) as u8;
        let entry = self.fast[key as usize];
        if entry == 0 {
            return None;
        }
        bits.pos += usize::from(entry & 15);
        Some(usize::from(entry >> 4))
    }
}
struct Codes {
    literals: Huffman,
    distances: Huffman,
}
impl Codes {
    fn fixed() -> &'static Codes {
        static FIXED: std::sync::OnceLock<Codes> = std::sync::OnceLock::new();
        FIXED.get_or_init(|| {
            let mut lens = [8; 288];
            lens[144..256].fill(9);
            lens[256..280].fill(7);
            Codes {
                literals: Huffman::new(&lens).unwrap(),
                distances: Huffman::new(&[5; 32]).unwrap(),
            }
        })
    }

    fn dynamic(bits: &mut Bits) -> io::Result<Self> {
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
        // HLIT and HDIST are five-bit fields. The code lengths form one stream
        // and a repeat can cross the literal/distance boundary.
        let mut lengths = [0; 320];
        let lengths = &mut lengths[..nl + nd];
        read_code_lengths(bits, &code, lengths)?;
        let (literals, distances) = lengths.split_at(nl);
        Ok(Self {
            literals: Huffman::new(literals)?,
            distances: Huffman::new(distances)?,
        })
    }
    fn copy_match(
        &self,
        bits: &mut Bits,
        out: &mut Vec<u8>,
        i: usize,
        limit: usize,
    ) -> io::Result<()> {
        const BASE: [usize; 29] = [
            3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99,
            115, 131, 163, 195, 227, 258,
        ];
        let extra = if i == 28 { 0 } else { i.saturating_sub(4) / 4 };
        let len = BASE[i] + bits.read(extra)?;
        let d = self.distances.read(bits)?;
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
        // Repeat an overlapping match from the bytes already produced. Each
        // copy extends the available prefix, including when the distance is one.
        let start = out.len() - gap;
        let mut remaining = len;
        while remaining != 0 {
            let take = remaining.min(out.len() - start);
            out.extend_from_within(start..start + take);
            remaining -= take;
        }
        Ok(())
    }

    fn block(&self, bits: &mut Bits, out: &mut Vec<u8>, limit: usize) -> io::Result<()> {
        loop {
            let s = self.literals.read(bits)?;
            match s {
                0..=255 => {
                    if out.len() >= limit {
                        return Err(invalid("Git tree size limit"));
                    }
                    out.push(s as u8)
                }
                256 => break,
                257..=285 => {
                    self.copy_match(bits, out, s - 257, limit)?;
                }
                _ => return Err(invalid("invalid deflate code")),
            }
        }
        Ok(())
    }
}
fn read_code_lengths(bits: &mut Bits, code: &Huffman, lengths: &mut [usize]) -> io::Result<()> {
    let mut remaining = lengths;
    let mut previous = None;
    while !remaining.is_empty() {
        let (value, width) = match code.read(bits)? {
            16 => (previous.ok_or_else(|| invalid("first length repeated"))?, bits.read(2)? + 3),
            17 => (0, bits.read(3)? + 3),
            18 => (0, bits.read(7)? + 11),
            value => (value, 1),
        };
        let (run, tail) = remaining.split_at_mut_checked(width)
            .ok_or_else(|| invalid("excess Huffman lengths"))?;
        run.fill(value);
        remaining = tail;
        previous = Some(value);
    }
    Ok(())
}

fn adler32(out: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in out.chunks(5552) {
        for &c in chunk {
            a += u32::from(c);
            b += a
        }
        a %= 65521;
        b %= 65521
    }
    b << 16 | a
}

pub(super) fn zlib(data: &[u8], size: Option<usize>) -> io::Result<Vec<u8>> {
    let mut bits = Bits::from_zlib(data)?;
    let limit = size.unwrap_or(MAX_TREE);
    if limit > MAX_TREE {
        return Err(invalid("Git tree size limit"));
    }
    let mut out = Vec::with_capacity(limit.min(8192));
    while !bits.block(&mut out, limit)? {}
    let end = 2 + bits.pos.div_ceil(8);
    let check = data
        .get(end..end + 4)
        .ok_or_else(|| invalid("missing zlib checksum"))?;
    if check != adler32(&out).to_be_bytes() || size.is_some_and(|s| s != out.len()) {
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
            let code = (1usize << length) - 1 - usize::from(symbol + 1 != lengths.len());
            assert_symbol_at_each_offset(&encoder, symbol, code, length);
        }
    }

    fn assert_symbol_at_each_offset(encoder: &Huffman, symbol: usize, code: usize, length: usize) {
        for skip in 0..8 {
            let buf = encoded_symbol(code, length, skip);
            let mut bits = Bits {
                data: &buf,
                pos: skip,
            };
            assert_eq!(encoder.read(&mut bits).unwrap(), symbol);
            assert_eq!(bits.pos, skip + length);
        }
    }
    fn encoded_symbol(code: usize, length: usize, skip: usize) -> [u8; 4] {
        let mut buf = [0u8; 4];
        for i in 0..length {
            buf[(skip + i) / 8] |=
                (((code >> (length - 1 - i)) & 1) as u8) << ((skip + i) % 8);
        }
        buf
    }

    #[test]
    fn decoding_checks_output_size_and_checksum_on_repeated_matches() {
        let stream = &[
            120, 156, 115, 116, 28, 5, 163, 96, 20, 12, 119, 224, 228, 236, 226, 58, 138, 71, 241,
            40, 30, 222, 24, 0, 50, 116, 5, 179,
        ];
        let expected = [vec![b'A'; 1000], b"BCDE".repeat(250)].concat();
        assert_eq!(zlib(stream, Some(2000)).unwrap(), expected);
        assert!(zlib(stream, Some(1999)).is_err());
        let mut corrupt = stream.to_vec();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(zlib(&corrupt, Some(2000)).is_err());
    }
}
