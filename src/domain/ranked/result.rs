use super::{RankedHit, Row, snippet};
use std::ops::Range;

#[derive(Clone, Copy)]
pub(super) struct ChunkText<'a> {
    pub text: &'a str,
    pub base: usize,
}

impl ChunkText<'_> {
    fn contains(&self, span: &Range<usize>) -> bool {
        span.start >= self.base && self.text.get(span.start - self.base..span.end - self.base).is_some()
    }

    fn crosses_line(&self, from: usize, to: usize) -> bool {
        self.text.as_bytes()[from - self.base..to - self.base].contains(&b'\n')
    }
}

#[derive(Clone, Copy)]
pub(super) struct ChunkHits<'a> {
    pub instances: &'a [snippet::Instance],
    pub count: usize,
}

/// Projecting a result needs both ranked occurrences and the token spans they index.
pub(super) struct ChunkEvidence<'a> {
    pub spans: &'a [Range<usize>],
    pub hits: ChunkHits<'a>,
}

/// Borrowed, validated source for one winning chunk. Each consumer projects only its output.
pub(crate) struct MatchedChunk<'a> {
    pub file: usize,
    pub line: usize,
    pub last_line: usize,
    pub score: f64,
    region: &'a Range<usize>,
    spans: &'a [Range<usize>],
    source: ChunkText<'a>,
    hits: ChunkHits<'a>,
}

impl<'a> MatchedChunk<'a> {
    pub(super) fn new(
        row: &'a Row,
        score: f64,
        source: ChunkText<'a>,
        evidence: ChunkEvidence<'a>,
    ) -> Result<Self, String> {
        let ChunkEvidence { spans, hits } = evidence;
        let region = &row.chunk.bytes;
        if !source.contains(region) || spans.iter().any(|span| !contained_span(&source, region, span)) {
            return Err("ranked source does not match the index".into());
        }
        Ok(Self {
            file: row.file,
            line: row.chunk.first_line,
            last_line: row.chunk.last_line,
            score,
            region,
            spans,
            source,
            hits,
        })
    }
    pub(super) fn full_hit(&self) -> RankedHit {
        RankedHit {
            file: self.file,
            line: self.line,
            last_line: self.last_line,
            score: self.score,
            excerpt: self.excerpt(),
            anchor: self.anchor(),
        }
    }
    pub(crate) fn excerpt(&self) -> String {
        snippet::excerpt(self.source, self.region.clone(), self.spans, self.hits)
    }
    /// First hit on the earliest line containing the most distinct query phrases.
    pub(crate) fn anchor(&self) -> usize {
        let Some(first) = self.hits.instances.first() else {
            return self.region.start;
        };
        let initial = self.spans[first.start].start;
        if self.hits.count == 1 {
            return initial;
        }
        let mut evidence = AnchorEvidence::new(initial, self.hits.count);
        for hit in self.hits.instances {
            let offset = self.spans[hit.start].start;
            let new_line = self.source.crosses_line(evidence.previous, offset);
            if evidence.observe(hit.phrase, offset, new_line) {
                break;
            }
        }
        evidence.anchor
    }
}

fn contained_span(source: &ChunkText<'_>, region: &Range<usize>, span: &Range<usize>) -> bool {
    let in_region = span.start >= region.start && span.end <= region.end;
    in_region && source.contains(span)
}

struct AnchorEvidence {
    anchor: usize,
    first_on_line: usize,
    previous: usize,
    line: usize,
    distinct: usize,
    best: usize,
    seen: Vec<usize>,
}

impl AnchorEvidence {
    fn new(anchor: usize, count: usize) -> Self {
        Self {
            anchor,
            first_on_line: anchor,
            previous: anchor,
            line: 0,
            distinct: 0,
            best: 0,
            seen: vec![usize::MAX; count],
        }
    }

    fn observe(&mut self, phrase: usize, offset: usize, new_line: bool) -> bool {
        self.previous = offset;
        if new_line {
            self.distinct = 0;
            self.first_on_line = offset;
            self.line += 1;
        }
        if std::mem::replace(&mut self.seen[phrase], self.line) != self.line {
            self.distinct += 1;
        }
        if self.distinct > self.best {
            self.anchor = self.first_on_line;
            self.best = self.distinct;
        }
        self.best == self.seen.len()
    }
}
