use std::ops::Range;

#[derive(Copy, Clone, Debug)]
pub(super) struct Instance {
    pub start: usize,
    pub end: usize,
    pub phrase: usize,
}

const WINDOW: usize = 18;

pub(super) fn excerpt(
    text: &str,
    base: usize,
    body: Range<usize>,
    tokens: &[Range<usize>],
    hits: &[Instance],
    phrase_count: usize,
) -> String {
    let mut best = 0;
    let mut start = 0;
    if tokens.len() > WINDOW && !hits.is_empty() {
        let mut sentences = vec![0];
        for (i, token) in tokens.iter().enumerate().skip(1) {
            let gap = &text.as_bytes()[tokens[i - 1].end - base..token.start - base];
            let punctuation = gap
                .iter()
                .rposition(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'));
            if punctuation.is_some_and(|j| j + 1 < gap.len() && matches!(gap[j], b'.' | b':')) {
                sentences.push(i);
            }
        }
        let mut seen = vec![0; phrase_count];
        let mut epoch = 0;
        let mut score = |at: usize| {
            epoch += 1;
            let from = hits.partition_point(|h| h.start < at);
            let mut points = 0;
            let mut last = at;
            for h in hits[from..].iter().take_while(|h| h.start < at + WINDOW) {
                points += if seen[h.phrase] == epoch { 1 } else { 1000 };
                seen[h.phrase] = epoch;
                last = h.end;
            }
            (points, last)
        };
        for hit in hits {
            let (points, last) = score(hit.start);
            if points > best {
                best = points;
                let adjusted =
                    hit.start as i128 - (WINDOW as i128 - (last - hit.start) as i128) / 2;
                start = adjusted.clamp(0, (tokens.len() - WINDOW) as i128) as usize;
            }
            let sentence = sentences[sentences.partition_point(|&n| n <= hit.start) - 1];
            if sentence < hit.start {
                let points = score(sentence).0 + if sentence == 0 { 120 } else { 100 };
                if points > best {
                    best = points;
                    start = sentence;
                }
            }
        }
    }
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
    let mut matches = hits.iter().peekable();
    while let Some(first) = matches.next() {
        let mut stop = first.end;
        while matches.peek().is_some_and(|h| h.start < stop) {
            stop = stop.max(matches.next().unwrap().end);
        }
        if first.start < start || first.start >= end {
            continue;
        }
        let left = tokens[first.start].start;
        let right = tokens[stop.min(end) - 1].end;
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
