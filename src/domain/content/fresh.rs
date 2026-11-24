//! Transpose disjoint file ranges by the first two Unicode scalars. Within
//! each row a counting pass assigns a contiguous output range to every third
//! scalar. A second pass scatters file IDs directly into their durable order.
use super::{SAMPLE, SCALAR_MASK, grams};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{self, Write},
};

const ASCII: usize = 128;
type Hit = (u32, u32); // Third scalar and file ID.
type Row = (u64, Vec<Vec<Hit>>);

pub(crate) struct Run {
    common: Vec<Vec<Hit>>,
    rare: HashMap<u64, Vec<Hit>>,
    seen: Vec<u64>,
    touched: Vec<usize>,
    unicode: HashSet<u64>,
}
impl Run {
    pub(crate) fn new() -> Self {
        Self {
            common: vec![Vec::new(); ASCII * ASCII],
            rare: HashMap::new(),
            seen: vec![0; ASCII * ASCII * ASCII / 64],
            touched: Vec::new(),
            unicode: HashSet::new(),
        }
    }

    #[inline]
    fn ascii(&mut self, a: usize, b: usize, c: usize, file: u32) {
        let prefix = a * ASCII + b;
        let gram = prefix * ASCII + c;
        let word = gram / 64;
        let bit = 1 << (gram % 64);
        let old = self.seen[word];
        if old & bit == 0 {
            if old == 0 {
                self.touched.push(word);
            }
            self.seen[word] = old | bit;
            self.common[prefix].push((c as u32, file));
        }
    }

    pub(crate) fn add(&mut self, file: u32, text: &str) {
        if text.is_ascii() {
            for triple in text.as_bytes().windows(3) {
                self.ascii(
                    triple[0] as usize,
                    triple[1] as usize,
                    triple[2] as usize,
                    file,
                );
            }
        } else {
            for key in grams(text) {
                let a = (key >> 42) as usize;
                let b = ((key >> 21) & SCALAR_MASK) as usize;
                let c = (key & SCALAR_MASK) as usize;
                if (a | b | c) < ASCII {
                    self.ascii(a, b, c, file);
                } else if self.unicode.insert(key) {
                    let row = if (a | b) < ASCII {
                        &mut self.common[a * ASCII + b]
                    } else {
                        self.rare.entry(key >> 21).or_default()
                    };
                    row.push((c as u32, file));
                }
            }
            self.unicode.clear();
        }
        for word in self.touched.drain(..) {
            self.seen[word] = 0;
        }
    }
}

#[derive(Default)]
struct Part {
    keys: Vec<u8>,
    lengths: Vec<u64>,
    ids: Vec<[u8; 8]>,
}
impl Part {
    fn row(&mut self, prefix: u64, runs: Vec<Vec<Hit>>) {
        let mut ascii = [0usize; ASCII];
        let mut rare = BTreeMap::<u32, usize>::new();
        for &(c, _) in runs.iter().flatten() {
            if let Some(n) = ascii.get_mut(c as usize) {
                *n += 1;
            } else {
                *rare.entry(c).or_default() += 1;
            }
        }
        let mut end = self.ids.len();
        let mut assign = |c: u32, value: &mut usize| {
            let n = *value;
            if n != 0 {
                for scalar in [(prefix >> 21) as u32, (prefix & SCALAR_MASK) as u32, c] {
                    self.keys.extend_from_slice(&scalar.to_le_bytes());
                }
                self.lengths.push(n as u64);
                *value = end;
                end += n;
            }
        };
        for (c, value) in ascii.iter_mut().enumerate() {
            assign(c as u32, value);
        }
        for (&c, value) in &mut rare {
            assign(c, value);
        }
        self.ids.resize(end, [0; 8]);
        for (c, file) in runs.into_iter().flatten() {
            let offset = if let Some(offset) = ascii.get_mut(c as usize) {
                offset
            } else {
                rare.get_mut(&c).unwrap()
            };
            self.ids[*offset] = u64::from(file).to_le_bytes();
            *offset += 1;
        }
    }
}

pub(crate) struct Product(Vec<Part>);

impl Product {
    pub(crate) fn transpose(runs: Vec<Run>) -> io::Result<Self> {
        let width = runs.len();
        let mut rare = BTreeMap::<u64, Vec<Vec<Hit>>>::new();
        let mut common: Vec<_> = runs
            .into_iter()
            .map(|run| {
                for (prefix, hits) in run.rare {
                    rare.entry(prefix).or_default().push(hits);
                }
                run.common.into_iter()
            })
            .collect();
        let mut rows: Vec<Row> = rare.into_iter().collect();
        for i in 0..ASCII * ASCII {
            let lists: Vec<_> = common
                .iter_mut()
                .filter_map(|run| {
                    let hits = run.next().unwrap();
                    (!hits.is_empty()).then_some(hits)
                })
                .collect();
            if !lists.is_empty() {
                rows.push((((i / ASCII) as u64) << 21 | (i % ASCII) as u64, lists));
            }
        }
        rows.sort_unstable_by_key(|r| r.0);
        let size = |row: &Row| row.1.iter().map(Vec::len).sum::<usize>();
        let target = rows.iter().map(size).sum::<usize>().div_ceil(width);
        std::thread::scope(|scope| {
            let mut jobs = Vec::new();
            let mut rows = rows.into_iter().peekable();
            while rows.peek().is_some() {
                let mut part = Vec::new();
                let mut hits = 0;
                for row in rows.by_ref() {
                    hits += size(&row);
                    part.push(row);
                    if hits >= target {
                        break;
                    }
                }
                jobs.push(scope.spawn(move || {
                    let mut output = Part {
                        ids: Vec::with_capacity(hits),
                        ..Part::default()
                    };
                    for (prefix, runs) in part {
                        output.row(prefix, runs);
                    }
                    output
                }));
            }
            jobs.into_iter()
                .map(|j| {
                    j.join()
                        .map_err(|_| io::Error::other("posting transpose worker failed"))
                })
                .collect::<io::Result<_>>()
                .map(Self)
        })
    }

    pub(crate) fn seal(self, mut out: impl Write) -> io::Result<()> {
        let count = self.0.iter().map(|p| p.lengths.len()).sum::<usize>() as u64;
        out.write_all(&[2])?;
        out.write_all(&count.to_le_bytes())?;
        for p in &self.0 {
            out.write_all(&p.keys)?;
        }
        out.write_all(&count.to_le_bytes())?;
        let mut anchors = Vec::with_capacity(8 * (count as usize).div_ceil(SAMPLE));
        let mut offset = 0u64;
        let mut lengths = Vec::new();
        let mut index = 0;
        for p in &self.0 {
            lengths.clear();
            for &n in &p.lengths {
                if index % SAMPLE == 0 {
                    anchors.extend_from_slice(&offset.to_le_bytes());
                }
                lengths.extend_from_slice(&n.to_le_bytes());
                offset += n;
                index += 1;
            }
            out.write_all(&lengths)?;
        }
        out.write_all(&((anchors.len() / 8) as u64).to_le_bytes())?;
        out.write_all(&anchors)?;
        out.write_all(&offset.to_le_bytes())?;
        for p in self.0 {
            out.write_all(p.ids.as_flattened())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Document, FileId, content::ContentBuilder};

    #[test]
    fn transpose_matches_sampled_codec_across_shards_and_unicode() {
        let mut reference = ContentBuilder::default();
        let mut runs = Vec::new();
        let ascii: String = (1u8..127).map(char::from).collect();
        for run_id in 0..4 {
            let mut run = Run::new();
            for (i, text) in [
                "",
                "ab",
                &ascii,
                "abλabλaλb λab Ωλ🦀xyz",
                "short",
                "aaaaaaa",
            ]
            .iter()
            .enumerate()
            {
                let id = run_id * 6 + i as u32;
                run.add(id, text);
                reference
                    .add(Document {
                        file: FileId(id as u64),
                        text,
                    })
                    .unwrap();
            }
            runs.push(run);
        }
        let mut actual = Vec::new();
        Product::transpose(runs).unwrap().seal(&mut actual).unwrap();
        let mut expected = Vec::new();
        reference.seal_layout(&mut expected, true).unwrap();
        assert_eq!(actual, expected);
    }
}
