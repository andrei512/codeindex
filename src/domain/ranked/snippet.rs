use super::result::{ChunkHits, ChunkText};
use std::ops::Range;

#[derive(Copy, Clone, Debug)]
pub(super) struct Instance {
    pub start: usize,
    pub end: usize,
    pub phrase: usize,
}

const WINDOW: usize = 18;

pub(super) fn excerpt(
    source: ChunkText<'_>,
    body: Range<usize>,
    tokens: &[Range<usize>],
    hits: ChunkHits<'_>,
) -> String {
    let ChunkText { text, base } = source;
    let start = if tokens.len() > WINDOW && !hits.instances.is_empty() {
        window_start(source, tokens, hits)
    } else {
        0
    };
    let end = (start + WINDOW).min(tokens.len());
    let mut pos = if start == 0 {
        body.start
    } else {
        tokens[start].start
    };
    let tail = if end == tokens.len() {
        body.end
    } else {
        tokens[end - 1].end
    };
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    // Hits are ordered by token. Later starts cannot affect this excerpt.
    for range in highlights(tokens, hits.instances, start..end) {
        let left = range.start;
        let right = range.end;
        out.push_str(&text[pos - base..left - base]);
        out.push('[');
        out.push_str(&text[left - base..right - base]);
        out.push(']');
        pos = right;
    }
    out.push_str(&text[pos - base..tail - base]);
    if end < tokens.len() {
        out.push('…');
    }
    out
}

fn highlights<'a>(
    tokens: &'a [Range<usize>],
    hits: &'a [Instance],
    window: Range<usize>,
) -> impl Iterator<Item = Range<usize>> + 'a {
    let visible = hits.partition_point(|hit| hit.start < window.end);
    let mut matches = hits[..visible].iter().peekable();
    std::iter::from_fn(move || {
        let first = matches.next()?;
        let end = extend_hit(first.end, &mut matches);
        Some(first.start..end)
    })
    .filter(move |hit| hit.start >= window.start)
    .map(move |hit| tokens[hit.start].start..tokens[hit.end.min(window.end) - 1].end)
}

fn extend_hit<'a>(
    mut end: usize,
    hits: &mut std::iter::Peekable<impl Iterator<Item = &'a Instance>>,
) -> usize {
    while let Some(hit) = hits.next_if(|hit| hit.start < end) {
        end = end.max(hit.end);
    }
    end
}

/// Moving the candidate right admits each hit once and retires it at most once.
struct HitWindow<'a> {
    hits: &'a [Instance],
    left: usize,
    right: usize,
    counts: Vec<usize>,
    distinct: usize,
}

impl<'a> HitWindow<'a> {
    fn new(hits: &'a [Instance], phrase_count: usize) -> Self {
        Self {
            hits,
            left: 0,
            right: 0,
            counts: vec![0; phrase_count],
            distinct: 0,
        }
    }

    fn score_sentence(&mut self, start: usize) -> (usize, usize) {
        let bonus = if start == 0 { 120 } else { 100 };
        (start, self.measure(start).0 + bonus)
    }

    fn measure(&mut self, at: usize) -> (usize, usize) {
        while let Some(hit) = self.hits.get(self.right).filter(|h| h.start < at + WINDOW) {
            let count = &mut self.counts[hit.phrase];
            self.distinct += usize::from(*count == 0);
            *count += 1;
            self.right += 1;
        }
        while self.left < self.right && self.hits[self.left].start < at {
            let count = &mut self.counts[self.hits[self.left].phrase];
            *count -= 1;
            self.distinct -= usize::from(*count == 0);
            self.left += 1;
        }
        let active = &self.hits[self.left..self.right];
        (
            999 * self.distinct + active.len(),
            active.last().map_or(at, |h| h.end),
        )
    }
}

fn sentence_starts<'a>(
    text: &'a str,
    base: usize,
    tokens: &'a [Range<usize>],
) -> impl Iterator<Item = usize> + 'a {
    tokens.windows(2).enumerate().filter_map(move |(i, pair)| {
        let gap = &text.as_bytes()[pair[0].end - base..pair[1].start - base];
        let j = gap
            .iter()
            .rposition(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'))?;
        (j + 1 < gap.len() && matches!(gap[j], b'.' | b':')).then_some(i + 1)
    })
}

/// Each sentence start is considered at most once, after a hit inside that sentence.
struct SentenceCandidates<I: Iterator<Item = usize>> {
    starts: std::iter::Peekable<I>,
    pending: Option<usize>,
}

impl<I: Iterator<Item = usize>> SentenceCandidates<I> {
    fn before(&mut self, at: usize) -> Option<usize> {
        while let Some(start) = self.starts.next_if(|&start| start <= at) {
            self.pending = Some(start);
        }
        self.pending.take_if(|start| *start < at)
    }
}

#[derive(Default)]
struct BestWindow {
    score: usize,
    start: usize,
}

impl BestWindow {
    fn consider_sentence(&mut self, candidate: Option<(usize, usize)>) {
        if let Some((start, points)) = candidate.filter(|&(_, points)| points > self.score) {
            self.score = points;
            self.start = start;
        }
    }

    fn consider_hit(&mut self, (score, last): (usize, usize), at: usize, tokens: usize) {
        if score > self.score {
            let centered = at as i128 - (WINDOW as i128 - (last - at) as i128) / 2;
            self.score = score;
            self.start = centered.clamp(0, (tokens - WINDOW) as i128) as usize;
        }
    }
}

fn window_start(source: ChunkText<'_>, tokens: &[Range<usize>], hits: ChunkHits<'_>) -> usize {
    let mut sentences = SentenceCandidates {
        starts: sentence_starts(source.text, source.base, tokens).peekable(),
        pending: Some(0),
    };
    let mut window = HitWindow::new(hits.instances, hits.count);
    let mut best = BestWindow::default();
    for group in hits.instances.chunk_by(|a, b| a.start == b.start) {
        let at = group[0].start;
        // Score in source order, giving the hit precedence when both scores tie.
        let sentence = sentences.before(at).map(|start| window.score_sentence(start));
        best.consider_hit(window.measure(at), at, tokens.len());
        best.consider_sentence(sentence);
    }
    best.start
}
