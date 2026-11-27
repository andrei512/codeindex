use std::ops::Range;

pub(crate) const CHUNK_LINES: usize = 60;
pub(crate) const CHUNK_STRIDE: usize = 50;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkRange {
    pub first_line: usize,
    pub last_line: usize,
    pub bytes: Range<usize>,
}

/// Heading lines are one-based and increasing. The preamble starts at line one.
pub fn chunk_ranges(text: &str, section_first_lines: &[usize]) -> Vec<ChunkRange> {
    let ends: Vec<_> = text.match_indices('\n').map(|(i, _)| i + 1).collect();
    from_sections(&ends, text.len(), |heading| {
        section_first_lines.iter().copied().for_each(heading)
    })
}

pub fn chunks_from_line_ends(ends: &[usize], section_first_lines: &[usize]) -> Vec<ChunkRange> {
    from_sections(ends, ends.last().copied().unwrap_or(0), |heading| {
        for &line in section_first_lines {
            heading(line);
        }
    })
}

/// Consume heading starts as the file parser discovers them.
pub(crate) fn from_sections(
    ends: &[usize],
    byte_len: usize,
    visit: impl FnOnce(&mut dyn FnMut(usize)),
) -> Vec<ChunkRange> {
    let lines = ends.len() + usize::from(ends.last().copied().unwrap_or(0) < byte_len);
    let mut chunks = Vec::new();
    let mut first = 0;
    visit(&mut |line| {
        if line > 1 && line <= lines {
            append_section(&mut chunks, first..line - 1, ends, byte_len);
            first = line - 1;
        }
    });
    append_section(&mut chunks, first..lines, ends, byte_len);
    chunks
}

fn append_section(chunks: &mut Vec<ChunkRange>, section: Range<usize>, ends: &[usize], byte_len: usize) {
    let stop = section.end;
    for first in section.step_by(CHUNK_STRIDE) {
        let end = (first + CHUNK_LINES).min(stop);
        let start_byte = first.checked_sub(1).map_or(0, |previous| ends[previous]);
        chunks.push(ChunkRange {
            first_line: first + 1,
            last_line: end,
            bytes: start_byte..ends.get(end - 1).copied().unwrap_or(byte_len),
        });
        if end == stop {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_final_partial_line_shares_the_chunk_grid_with_terminated_lines() {
        for count in [0, 1, 49, 50, 51, 59, 60, 61, 109, 110, 111] {
            for suffix in ["", "末", "末\n"] {
                let text = "one\r\n".repeat(count) + suffix;
                let complete: Vec<_> = text.match_indices('\n').map(|(i, _)| i + 1).collect();
                let mut all = complete.clone();
                if !text.is_empty() && !text.ends_with('\n') {
                    all.push(text.len());
                }
                for section in [1, 2, 50, 60, count + 1] {
                    let chunks = chunk_ranges(&text, &[section]);
                    assert_eq!(chunks, chunks_from_line_ends(&all, &[section]));
                    assert_eq!(chunks.last().map_or(0, |c| c.bytes.end), text.len());
                    assert_eq!(chunks.last().map_or(0, |c| c.last_line), all.len());
                }
            }
        }
    }
}
