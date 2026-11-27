//! Native positional search. Token spans belong to the captured file, even
//! where two overlapping chunks refer to the same source tokens.
mod bulk;
mod chunk;
/// File count and accessors for the paths and source bytes referenced by a ranked extent.
pub struct ExtentCatalog<P, S> {
    /// Number of files in the catalog that was used to build the extent.
    pub count: usize,
    /// Looks up a repository-relative path by file ordinal.
    pub path: P,
    /// Reads the exact source text for a file ordinal and byte range.
    pub source: S,
}

mod cursor;
mod directory;
mod frequency;
mod grouped;
mod shards;
pub(crate) use bulk::BulkRank;
pub(crate) use shards::{FreshRank, ShardSet};
mod incremental;
pub(crate) mod patch;
mod result;
mod snippet;
pub(crate) use result::MatchedChunk;
mod storage;
mod term_key;
#[cfg(test)]
mod tests;
mod token;
mod unicode;

pub(crate) use chunk::from_sections;
pub use chunk::{ChunkRange, chunk_ranges, chunks_from_line_ends};
pub(crate) use chunk::{CHUNK_LINES, CHUNK_STRIDE};
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
impl Row {
    fn encoded_fields(&self, file: usize, previous: usize) -> [usize; 9] {
        [
            file,
            self.chunk.first_line,
            self.chunk.last_line - self.chunk.first_line,
            self.chunk.bytes.start,
            self.chunk.bytes.len(),
            self.span_start,
            self.body_start - previous,
            self.parts_start - self.body_start,
            self.end - self.parts_start,
        ]
    }

    fn span_range(&self) -> Range<usize> {
        self.span_start..self.span_start + (self.parts_start - self.body_start)
    }
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
    fn phrase(
        word: &str,
        lookup: &mut impl FnMut(&[u8]) -> std::io::Result<Option<usize>>,
    ) -> std::io::Result<Option<Vec<usize>>> {
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
        Ok((known && !ids.is_empty()).then_some(ids))
    }

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
            if let Some(ids) = Self::phrase(word, &mut lookup)? {
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
impl Query {
    fn score(
        &self,
        frequencies: &[f64],
        weight: impl Fn(usize) -> f64,
        len: usize,
        average: f64,
    ) -> f64 {
        // The frozen arm64 writer contracts the BM25 denominator. Its
        // eight-lane loop rounds products before summing them in order;
        // the scalar remainder contracts the final multiplication as well.
        let norm = 0.25 + 0.75 * len as f64 / average;
        let vector_end = self.phrase_count / 8 * 8;
        -self
            .order
            .iter()
            .zip(&self.slots)
            .fold(0.0, |sum, (&i, &slot)| {
                let freq = frequencies[i];
                let scaled = (freq * 2.2) / 1.2_f64.mul_add(norm, freq);
                if slot < vector_end {
                    sum + weight(i) * scaled
                } else {
                    weight(i).mul_add(scaled, sum)
                }
            })
    }
}

/// Keep result-limit and tie ordering together. Projection later groups the
/// selected rows by source file and restores this exact order before output.
struct BestChunks<'a, C> {
    rows: &'a [Row],
    compare_path: C,
    selected: Vec<(usize, f64)>,
    matched: usize,
}
impl<C: Fn(usize, usize) -> std::cmp::Ordering> BestChunks<'_, C> {
    fn consider(&mut self, row: usize, score: f64) {
        let r = &self.rows[row];
        let at = self.selected.partition_point(|&(id, s)| {
            let other = &self.rows[id];
            s.total_cmp(&score)
                .then_with(|| (self.compare_path)(other.file, r.file))
                .then(other.chunk.first_line.cmp(&r.chunk.first_line))
                .is_le()
        });
        if at < 50 {
            self.selected.insert(at, (row, score));
            self.selected.truncate(50);
        }
        self.matched += 1;
    }
}
struct RankingDocument<'a> {
    path: &'a str,
    body: &'a str,
    chunks: &'a [ChunkRange],
}

struct FileTokens {
    path: Vec<usize>,
    spans: Vec<Range<usize>>,
    body: Vec<usize>,
    parts: Vec<(usize, usize)>,
}

impl FileTokens {
    fn new(index: &mut RankedIndex, document: &RankingDocument<'_>, short: &mut [(u64, usize)]) -> Self {
        let mut tokens = Self { path: Vec::new(), spans: Vec::new(), body: Vec::new(), parts: Vec::new() };
        token::tokenize(document.path, |key, _| tokens.path.push(index.intern(key)));
        token::ranked_words(document.body, |is_part, key, span| {
            let id = index.intern_cached(key, short);
            if is_part {
                tokens.parts.push((span.start, id));
            } else {
                tokens.spans.push(span);
                tokens.body.push(id);
            }
        });
        tokens
    }

    fn add_row(
        &self,
        index: &mut RankedIndex,
        file: usize,
        chunk: &ChunkRange,
        mut first_posting: impl FnMut(usize),
    ) {
        let body = |at| self.spans.partition_point(|span| span.start < at);
        let part = |at| self.parts.partition_point(|part| part.0 < at);
        let begin = body(chunk.bytes.start);
        let end = body(chunk.bytes.end);
        let parts = part(chunk.bytes.start)..part(chunk.bytes.end);
        let start = index.tokens;
        let ids = self.path.iter().copied()
            .chain(self.body[begin..end].iter().copied())
            .chain(self.parts[parts].iter().map(|part| part.1));
        for id in ids {
            index.record_posting(id, start, &mut first_posting);
        }
        let body_start = start + self.path.len();
        index.rows.push(Row {
            file,
            chunk: chunk.clone(),
            span_start: begin,
            body_start,
            parts_start: body_start + end - begin,
            end: index.tokens,
        });
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

    fn intern_cached(&mut self, key: &[u8], short: &mut [(u64, usize)]) -> usize {
        if !short.is_empty() && key.len() < 8 {
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
        }
    }

    /// Returns the ordinal supplied to the source accessor when searching.
    pub fn add_file(&mut self, path: &str, body: &str, chunks: &[ChunkRange]) -> usize {
        self.add_file_with(RankingDocument { path, body, chunks }, &mut [], |_| {})
    }

    fn add_file_with(
        &mut self,
        document: RankingDocument<'_>,
        short: &mut [(u64, usize)],
        mut first_posting: impl FnMut(usize),
    ) -> usize {
        let file = self.files.len();
        let spans = if document.chunks.is_empty() {
            Vec::new()
        } else {
            self.add_file_rows(&document, file, short, &mut first_posting)
        };
        self.files.push(File { path: document.path.to_owned(), spans });
        file
    }

    fn add_file_rows(
        &mut self,
        document: &RankingDocument<'_>,
        file: usize,
        short: &mut [(u64, usize)],
        mut first_posting: impl FnMut(usize),
    ) -> Vec<Range<usize>> {
        let tokens = FileTokens::new(self, document, short);
        for chunk in document.chunks {
            tokens.add_row(self, file, chunk, &mut first_posting);
        }
        tokens.spans
    }

    #[inline]
    fn record_posting(&mut self, id: usize, start: usize, first_posting: &mut impl FnMut(usize)) {
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
            |row, score, hits| {
                if current != Some(row.file) {
                    source_text = source(row.file).map_err(|e| e.to_string())?;
                    current = Some(row.file);
                }
                MatchedChunk::new(
                    row,
                    score,
                    result::ChunkText { text: source_text.as_ref(), base: 0 },
                    result::ChunkEvidence {
                        spans: &self.files[row.file].spans[row.span_range()],
                        hits,
                    },
                )
                .map(|chunk| chunk.full_hit())
            },
        )
    }
    fn evaluate<T>(
        &self,
        plan: &Query,
        compare_path: impl Fn(usize, usize) -> std::cmp::Ordering,
        mut project: impl FnMut(&Row, f64, result::ChunkHits<'_>) -> Result<T, String>,
    ) -> Result<(Vec<T>, bool), String> {
        let (best, truncated) = self.best_chunks(plan, compare_path);
        let mut selected: Vec<_> = best.into_iter().enumerate().collect();
        selected.sort_unstable_by_key(|(_, (id, _))| self.rows[*id].file);
        let mut results = Vec::with_capacity(selected.len());
        let mut hits = Vec::new();
        for (rank, (id, score)) in selected {
            let row = &self.rows[id];
            self.collect_hits(row, plan, &mut hits);
            results.push((rank, project(row, score, result::ChunkHits {
                instances: &hits,
                count: plan.order.len(),
            })?));
        }
        results.sort_unstable_by_key(|(rank, _)| *rank);
        Ok((results.into_iter().map(|(_, hit)| hit).collect(), truncated))
    }

    fn best_chunks(
        &self,
        plan: &Query,
        compare_path: impl Fn(usize, usize) -> std::cmp::Ordering,
    ) -> (Vec<(usize, f64)>, bool) {
        let phrases = &plan.phrases;
        let documents = self.rows.len() as f64;
        let average = self.tokens as f64 / documents;
        let mut streams: Vec<_> = phrases
            .iter()
            .map(|phrase| {
                let known = (phrase.len() == 1).then(|| self.terms[phrase[0]].document_frequency);
                frequency::Frequencies::new(self.instances(phrase), known, documents)
            })
            .collect();
        let mut best = BestChunks {
            rows: &self.rows,
            compare_path,
            selected: Vec::with_capacity(51),
            matched: 0,
        };
        let mut frequencies = vec![0.; phrases.len()];
        while let Some(row) = streams
            .iter_mut()
            .filter_map(frequency::Frequencies::next_row)
            .min()
        {
            let r = &self.rows[row];
            let previous_end = row.checked_sub(1).map_or(0, |i| self.rows[i].end);
            let len = r.end - previous_end;
            frequencies.iter_mut().zip(&mut streams)
                .for_each(|(freq, stream)| *freq = stream.take_row(row));
            let score = plan.score(&frequencies, |i| streams[i].weight, len, average);
            best.consider(row, score);
        }
        (best.selected, best.matched > 50)
    }

    fn collect_hits(&self, row: &Row, plan: &Query, hits: &mut Vec<snippet::Instance>) {
        hits.clear();
        for (phrase, &i) in plan.order.iter().enumerate() {
            let ids = &plan.phrases[i];
            let postings = &self.terms[ids[0]].positions;
            let offset = postings.partition_point(|&p| p < row.body_start);
            for &pos in postings[offset..]
                .iter()
                .take_while(|&&p| p < row.parts_start)
            {
                if ids.len() <= row.parts_start - pos
                    && ids
                        .iter()
                        .copied()
                        .enumerate()
                        .skip(1)
                        .all(|(j, id)| self.terms[id].positions.binary_search(&(pos + j)).is_ok())
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
    }
}
