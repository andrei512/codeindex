use std::ops::Range;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkRange {
    pub first_line: usize,
    pub last_line: usize,
    pub bytes: Range<usize>,
}

/// Heading lines are one-based and increasing. The preamble starts at line one.
pub fn chunk_ranges(text: &str, section_first_lines: &[usize]) -> Vec<ChunkRange> {
    let ends = text
        .as_bytes()
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| (b == b'\n').then_some(i + 1))
        .chain((!text.is_empty() && !text.ends_with('\n')).then_some(text.len()))
        .collect::<Vec<_>>();
    chunks_from_line_ends(&ends, section_first_lines)
}

pub fn chunks_from_line_ends(ends: &[usize], section_first_lines: &[usize]) -> Vec<ChunkRange> {
    let mut chunks = Vec::new();
    let mut first = 0;
    for stop in section_first_lines
        .iter()
        .copied()
        .filter(|&n| n > 1 && n <= ends.len())
        .map(|n| n - 1)
        .chain(std::iter::once(ends.len()))
    {
        while first < stop {
            let end = (first + 60).min(stop);
            chunks.push(ChunkRange {
                first_line: first + 1,
                last_line: end,
                bytes: (if first == 0 { 0 } else { ends[first - 1] })..ends[end - 1],
            });
            if end == stop {
                break;
            }
            first += 50;
        }
        first = stop;
    }
    chunks
}
