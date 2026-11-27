//! Transpose disjoint file ranges by the first two Unicode scalars. Within
//! each row a counting pass assigns a contiguous output range to every third
//! scalar. A second pass scatters file IDs directly into their durable order.
use super::{PostingLayout, SAMPLE, SCALAR_MASK, grams};
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
    seen: super::SeenAscii,
    unicode: HashSet<u64>,
}
impl Run {
    pub(crate) fn new() -> Self {
        let mut seen = super::SeenAscii::default();
        seen.allocate();
        Self {
            common: vec![Vec::new(); ASCII * ASCII],
            rare: HashMap::new(),
            seen,
            unicode: HashSet::new(),
        }
    }

    #[inline]
    fn ascii(&mut self, a: usize, b: usize, c: usize, file: u32) {
        let prefix = a * ASCII + b;
        let gram = prefix * ASCII + c;
        if self.seen.insert(gram) {
            self.common[prefix].push((c as u32, file));
        }
    }

    fn add_scalar_trigram(&mut self, key: u64, file: u32) {
        let a = (key >> 42) as usize;
        let b = ((key >> 21) & SCALAR_MASK) as usize;
        let c = (key & SCALAR_MASK) as usize;
        if (a | b | c) < ASCII {
            self.ascii(a, b, c, file);
            return;
        }
        if self.unicode.insert(key) {
            self.scalar_row(a, b, key >> 21).push((c as u32, file));
        }
    }

    fn scalar_row(&mut self, a: usize, b: usize, prefix: u64) -> &mut Vec<Hit> {
        if (a | b) < ASCII {
            &mut self.common[a * ASCII + b]
        } else {
            self.rare.entry(prefix).or_default()
        }
    }

    fn ascii_text(&mut self, text: &str, file: u32) {
        for [a, b, c] in super::ascii_grams(text) {
            self.ascii(a.into(), b.into(), c.into(), file);
        }
    }

    fn scalar_text(&mut self, text: &str, file: u32) {
        for key in grams(text) {
            self.add_scalar_trigram(key, file);
        }
        self.unicode.clear();
    }

    pub(crate) fn add(&mut self, file: u32, text: &str) {
        if text.is_ascii() {
            self.ascii_text(text, file);
        } else {
            self.scalar_text(text, file);
        }
        self.seen.clear();
    }
}

#[derive(Default)]
struct Part {
    keys: Vec<u8>,
    lengths: Vec<u64>,
    ids: Vec<[u8; 8]>,
}
struct ThirdScalars {
    ascii: [usize; ASCII],
    rare: BTreeMap<u32, usize>,
}

impl ThirdScalars {
    fn collect(runs: &[Vec<Hit>]) -> Self {
        let mut scalars = Self { ascii: [0; ASCII], rare: BTreeMap::new() };
        for &(scalar, _) in runs.iter().flatten() {
            *scalars.count(scalar) += 1;
        }
        scalars
    }

    fn count(&mut self, scalar: u32) -> &mut usize {
        match self.ascii.get_mut(scalar as usize) {
            Some(count) => count,
            None => self.rare.entry(scalar).or_default(),
        }
    }

    fn offset(&mut self, scalar: u32) -> &mut usize {
        match self.ascii.get_mut(scalar as usize) {
            Some(offset) => offset,
            None => self.rare.get_mut(&scalar).unwrap(),
        }
    }

    fn assign(&mut self, mut emit: impl FnMut(u32, &mut usize)) {
        for (index, count) in self.ascii.iter_mut().enumerate() {
            emit(index as u32, count);
        }
        for (&scalar, count) in &mut self.rare {
            emit(scalar, count);
        }
    }
}

impl Part {
    fn row(&mut self, prefix: u64, runs: Vec<Vec<Hit>>) {
        let mut scalars = ThirdScalars::collect(&runs);
        let mut end = self.ids.len();
        scalars.assign(|scalar, value| {
            let count = *value;
            if count != 0 {
                self.keys.extend_from_slice(&super::encoded_key((prefix << 21) | u64::from(scalar)));
                self.lengths.push(count as u64);
                *value = end;
                end += count;
            }
        });
        self.ids.resize(end, [0; 8]);
        for (c, file) in runs.into_iter().flatten() {
            let offset = scalars.offset(c);
            self.ids[*offset] = u64::from(file).to_le_bytes();
            *offset += 1;
        }
    }
}

pub(crate) struct Product(Vec<Part>);

fn collect_rows(runs: Vec<Run>) -> Vec<Row> {
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
    rows
}

fn row_size(row: &Row) -> usize {
    row.1.iter().map(Vec::len).sum()
}

fn next_part(rows: &mut impl Iterator<Item = Row>, target: usize) -> (usize, Vec<Row>) {
    let mut part = Vec::new();
    let mut hits = 0;
    for row in rows {
        hits += row_size(&row);
        part.push(row);
        if hits >= target {
            break;
        }
    }
    (hits, part)
}

impl Product {
    pub(crate) fn transpose(runs: Vec<Run>) -> io::Result<Self> {
        let width = runs.len();
        let rows = collect_rows(runs);
        let target = rows.iter().map(row_size).sum::<usize>().div_ceil(width);
        std::thread::scope(|scope| {
            let mut jobs = Vec::new();
            let mut rows = rows.into_iter().peekable();
            while rows.peek().is_some() {
                let (hits, part) = next_part(&mut rows, target);
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
        out.write_all(&PostingLayout::Sampled.tag())?;
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
