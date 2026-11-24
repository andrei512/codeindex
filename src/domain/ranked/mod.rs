//! Native positional search. Token spans belong to the captured file, even
//! where two overlapping chunks refer to the same source tokens.
mod bulk;
mod chunk;
mod cursor;
mod directory;
mod grouped;
mod shards;
pub(crate) use bulk::BulkRank;
pub(crate) use shards::{FreshRank, ShardSet};
mod incremental;
pub(crate) mod patch;
mod snippet;
mod storage;
#[cfg(test)]
mod tests;
mod token;
mod unicode;

pub use chunk::{ChunkRange, chunk_ranges, chunks_from_line_ends};
use std::collections::HashMap;
use std::ops::Range;

#[derive(Default)]
pub struct RankedIndex {
    dictionary: HashMap<Vec<u8>, usize>,
    terms: Vec<Term>,
    files: Vec<File>,
    rows: Vec<Row>,
    tokens: usize,
}
#[derive(Default)]
struct Term {
    positions: Vec<usize>,
    document_frequency: usize,
}
struct File {
    path: String,
    spans: Vec<Range<usize>>,
}
struct Row {
    file: usize,
    chunk: ChunkRange,
    span_start: usize,
    body_start: usize,
    parts_start: usize,
    end: usize,
}
#[derive(Debug)]
pub struct RankedHit {
    pub file: usize,
    pub line: usize,
    pub last_line: usize,
    pub excerpt: String,
    pub anchor: usize,
    pub score: f64,
}

struct Query {
    phrases: Vec<Vec<usize>>,
    order: Vec<usize>,
    slots: Vec<usize>,
    phrase_count: usize,
}
impl Query {
    fn parse(
        query: &str,
        mut lookup: impl FnMut(&[u8]) -> std::io::Result<Option<usize>>,
    ) -> std::io::Result<Self> {
        if query.trim().is_empty() {
            return Err(std::io::Error::other("empty lexical query"));
        }
        let mut parsed = Self {
            phrases: vec![],
            order: vec![],
            slots: vec![],
            phrase_count: 0,
        };
        for word in query.split_whitespace() {
            let mut known = true;
            let mut ids = vec![];
            let mut error = None;
            token::tokenize(word, |key, _| {
                if !known {
                    return;
                }
                match lookup(key) {
                    Ok(Some(id)) => ids.push(id),
                    Ok(None) => known = false,
                    Err(e) => {
                        known = false;
                        error = Some(e)
                    }
                }
            });
            if let Some(e) = error {
                return Err(e);
            }
            if known && !ids.is_empty() {
                let i = parsed
                    .phrases
                    .iter()
                    .position(|p| p == &ids)
                    .unwrap_or_else(|| {
                        parsed.phrases.push(ids);
                        parsed.phrases.len() - 1
                    });
                parsed.order.push(i);
                parsed.slots.push(parsed.phrase_count);
            }
            parsed.phrase_count += 1;
        }
        Ok(parsed)
    }
}
impl RankedIndex {
    fn intern(&mut self, key: &[u8]) -> usize {
        if let Some(&id) = self.dictionary.get(key) {
            return id;
        }
        let id = self.terms.len();
        self.terms.push(Term::default());
        self.dictionary.insert(key.to_vec(), id);
        id
    }

    /// Returns the ordinal supplied to the source accessor when searching.
    pub fn add_file(&mut self, path: &str, body: &str, chunks: &[ChunkRange]) -> usize {
        self.add_file_with(path, body, chunks, &mut [], |_| {})
    }

    fn add_file_with(
        &mut self,
        path: &str,
        body: &str,
        chunks: &[ChunkRange],
        short: &mut [(u64, usize)],
        mut first_posting: impl FnMut(usize),
    ) -> usize {
        let file = self.files.len();
        let mut spans = Vec::new();
        if !chunks.is_empty() {
            let mut path_ids = Vec::new();
            token::tokenize(path, |k, _| path_ids.push(self.intern(k)));
            let mut body_ids = Vec::new();
            let mut parts = Vec::new();
            token::ranked_words(body, |is_part, key, span| {
                let id = if !short.is_empty() && key.len() < 8 {
                    let mut bytes = [0; 8];
                    bytes[..key.len()].copy_from_slice(key);
                    bytes[7] = key.len() as u8;
                    let packed = u64::from_le_bytes(bytes);
                    let slot = &mut short[(packed.wrapping_mul(0x9e3779b97f4a7c15) >> 52) as usize];
                    if slot.0 != packed {
                        *slot = (packed, self.intern(key));
                    }
                    slot.1
                } else {
                    self.intern(key)
                };
                if is_part {
                    parts.push((span.start, id));
                } else {
                    spans.push(span);
                    body_ids.push(id);
                }
            });
            for chunk in chunks {
                let begin = spans.partition_point(|s| s.start < chunk.bytes.start);
                let stop = spans.partition_point(|s| s.start < chunk.bytes.end);
                let part_begin = parts.partition_point(|p| p.0 < chunk.bytes.start);
                let part_stop = parts.partition_point(|p| p.0 < chunk.bytes.end);
                let start = self.tokens;
                for id in path_ids
                    .iter()
                    .copied()
                    .chain(body_ids[begin..stop].iter().copied())
                    .chain(parts[part_begin..part_stop].iter().map(|p| p.1))
                {
                    let term = &mut self.terms[id];
                    if term.positions.is_empty() {
                        first_posting(id);
                    }
                    if term.positions.last().is_none_or(|&p| p < start) {
                        term.document_frequency += 1;
                    }
                    term.positions.push(self.tokens);
                    self.tokens += 1;
                }
                let body_start = start + path_ids.len();
                self.rows.push(Row {
                    file,
                    chunk: chunk.clone(),
                    span_start: begin,
                    body_start,
                    parts_start: body_start + stop - begin,
                    end: self.tokens,
                });
            }
        }
        self.files.push(File {
            path: path.to_owned(),
            spans,
        });
        file
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
    pub fn chunk_count(&self) -> usize {
        self.rows.len()
    }

    fn instances<'a>(&'a self, phrase: &'a [usize]) -> impl Iterator<Item = (usize, usize)> + 'a {
        let (anchor, &term) = phrase
            .iter()
            .enumerate()
            .min_by_key(|&(_, &id)| self.terms[id].positions.len())
            .unwrap();
        let mut row = 0;
        self.terms[term].positions.iter().filter_map(move |&pos| {
            let start = pos.checked_sub(anchor)?;
            if self.rows[row].end <= start {
                row += 1;
                if self.rows[row].end <= start {
                    row += self.rows[row..].partition_point(|r| r.end <= start);
                }
            }
            let r = &self.rows[row];
            let field_end = if start < r.body_start {
                r.body_start
            } else if start < r.parts_start {
                r.parts_start
            } else {
                r.end
            };
            if phrase.len() > field_end - start {
                return None;
            }
            phrase
                .iter()
                .enumerate()
                .all(|(i, &id)| {
                    i == anchor || self.terms[id].positions.binary_search(&(start + i)).is_ok()
                })
                .then_some((row, start))
        })
    }

    /// Returns results in score, path, first-line order and an overflow flag.
    /// The source accessor provides the exact text corresponding to add_file.
    pub fn search<'a>(
        &self,
        query: &str,
        source: impl Fn(usize) -> &'a str,
    ) -> Result<(Vec<RankedHit>, bool), String> {
        self.search_with(query, |file| {
            Ok::<_, std::convert::Infallible>(std::borrow::Cow::Borrowed(source(file)))
        })
    }

    pub fn search_with<'a, E: std::fmt::Display>(
        &self,
        query: &str,
        mut source: impl FnMut(usize) -> Result<std::borrow::Cow<'a, str>, E>,
    ) -> Result<(Vec<RankedHit>, bool), String> {
        let plan = Query::parse(query, |key| Ok(self.dictionary.get(key).copied()))
            .map_err(|e| e.to_string())?;
        let mut current = None;
        let mut source_text = std::borrow::Cow::Borrowed("");
        self.evaluate(
            &plan,
            |a, b| self.files[a].path.cmp(&self.files[b].path),
            |row, hits, count| {
                if current != Some(row.file) {
                    source_text = source(row.file).map_err(|e| e.to_string())?;
                    current = Some(row.file);
                }
                let stop = row.span_start + row.parts_start - row.body_start;
                render(
                    row,
                    &self.files[row.file].spans[row.span_start..stop],
                    source_text.as_ref(),
                    0,
                    hits,
                    count,
                )
            },
        )
    }
    fn evaluate(
        &self,
        plan: &Query,
        compare_path: impl Fn(usize, usize) -> std::cmp::Ordering,
        mut excerpt: impl FnMut(&Row, &[snippet::Instance], usize) -> Result<(String, usize), String>,
    ) -> Result<(Vec<RankedHit>, bool), String> {
        let Query {
            phrases,
            order,
            slots,
            phrase_count,
        } = plan;
        let documents = self.rows.len() as f64;
        let average = self.tokens as f64 / documents;
        let weights: Vec<_> = phrases
            .iter()
            .map(|phrase| {
                let hits = if phrase.len() == 1 {
                    self.terms[phrase[0]].document_frequency
                } else {
                    let mut previous = None;
                    self.instances(phrase)
                        .filter(|&(row, _)| previous.replace(row) != Some(row))
                        .count()
                } as f64;
                let idf = ((documents - hits + 0.5) / (hits + 0.5)).ln();
                if idf <= 0.0 { 1e-6 } else { idf }
            })
            .collect();
        let mut streams: Vec<_> = phrases
            .iter()
            .map(|p| self.instances(p).peekable())
            .collect();
        let mut best: Vec<(usize, f64)> = Vec::with_capacity(51);
        let mut matched = 0;
        let mut frequencies = vec![0.; phrases.len()];
        while let Some(row) = streams
            .iter_mut()
            .filter_map(|s| s.peek().map(|x| x.0))
            .min()
        {
            let r = &self.rows[row];
            let len = r.end - if row == 0 { 0 } else { self.rows[row - 1].end };
            for (freq, stream) in frequencies.iter_mut().zip(&mut streams) {
                *freq = 0.;
                while stream.next_if(|v| v.0 == row).is_some() {
                    *freq += 1.;
                }
            }
            // The frozen arm64 writer contracts the BM25 denominator. Its
            // eight-lane loop rounds products before summing them in order;
            // the scalar remainder contracts the final multiplication as well.
            let norm = 0.25 + 0.75 * len as f64 / average;
            let vector_end = phrase_count / 8 * 8;
            let score = -order.iter().zip(slots).fold(0.0, |sum, (&i, &slot)| {
                let freq = frequencies[i];
                let scaled = (freq * 2.2) / 1.2_f64.mul_add(norm, freq);
                if slot < vector_end {
                    sum + weights[i] * scaled
                } else {
                    weights[i].mul_add(scaled, sum)
                }
            });
            let at = best.partition_point(|&(id, s)| {
                let other = &self.rows[id];
                (s.total_cmp(&score))
                    .then_with(|| compare_path(other.file, r.file))
                    .then(other.chunk.first_line.cmp(&r.chunk.first_line))
                    .is_le()
            });
            if at < 50 {
                best.insert(at, (row, score));
                best.truncate(50);
            }
            matched += 1;
        }
        let mut results = Vec::with_capacity(best.len());
        let mut selected: Vec<_> = best.into_iter().enumerate().collect();
        selected.sort_unstable_by_key(|(_, (id, _))| self.rows[*id].file);
        for (rank, (id, score)) in selected {
            let row = &self.rows[id];
            let mut hits = Vec::new();
            for (phrase, &i) in order.iter().enumerate() {
                let ids = &phrases[i];
                let postings = &self.terms[ids[0]].positions;
                let offset = postings.partition_point(|&p| p < row.body_start);
                for &pos in postings[offset..]
                    .iter()
                    .take_while(|&&p| p < row.parts_start)
                {
                    if ids.len() <= row.parts_start - pos
                        && ids.iter().copied().enumerate().skip(1).all(|(j, id)| {
                            self.terms[id].positions.binary_search(&(pos + j)).is_ok()
                        })
                    {
                        let start = pos - row.body_start;
                        hits.push(snippet::Instance {
                            start,
                            end: start + ids.len(),
                            phrase,
                        });
                    }
                }
            }
            hits.sort_unstable_by_key(|h| (h.start, h.phrase));
            let (excerpt, anchor) = excerpt(row, &hits, order.len())?;
            results.push((
                rank,
                RankedHit {
                    file: row.file,
                    line: row.chunk.first_line,
                    last_line: row.chunk.last_line,
                    score,
                    excerpt,
                    anchor,
                },
            ));
        }
        results.sort_unstable_by_key(|(rank, _)| *rank);
        Ok((
            results.into_iter().map(|(_, hit)| hit).collect(),
            matched > 50,
        ))
    }
}

fn render(
    row: &Row,
    spans: &[Range<usize>],
    text: &str,
    base: usize,
    hits: &[snippet::Instance],
    count: usize,
) -> Result<(String, usize), String> {
    let region = &row.chunk.bytes;
    if region.start < base
        || text.get(region.start - base..region.end - base).is_none()
        || spans.iter().any(|s| {
            s.start < region.start
                || s.end > region.end
                || text.get(s.start - base..s.end - base).is_none()
        })
    {
        return Err("ranked source does not match the index".into());
    }
    let mut anchor = region.start;
    let mut best = 0;
    let mut seen = vec![usize::MAX; count];
    let mut at = region.start;
    let mut line = 0;
    let mut distinct = 0;
    let mut first = at;
    for hit in hits {
        let offset = spans[hit.start].start;
        let breaks = text.as_bytes()[at - base..offset - base]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        if breaks > 0 || distinct == 0 {
            distinct = 0;
            first = offset;
        }
        line += breaks;
        if seen[hit.phrase] != line {
            seen[hit.phrase] = line;
            distinct += 1;
            if distinct > best {
                best = distinct;
                anchor = first;
            }
        }
        at = offset;
    }
    Ok((
        snippet::excerpt(text, base, region.clone(), spans, hits, count),
        anchor,
    ))
}
