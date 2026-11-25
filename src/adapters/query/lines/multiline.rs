use super::Lines;
use std::ops::Range;

/// Matched records retain their source coordinates through replacement, even
/// when the replacement inserts or removes a record terminator.
pub(super) struct Block {
    pub start: usize,
    pub end: usize,
    pub hits: Range<usize>,
}
pub(super) fn blocks(source: &str, spans: &[Range<usize>]) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for (i, r) in spans.iter().enumerate() {
        if r.start == source.len() {
            continue;
        }
        let start = source[..r.start].rfind('\n').map_or(0, |p| p + 1);
        let end = if !r.is_empty() && source.as_bytes()[r.end - 1] == b'\n' {
            r.end
        } else {
            source[r.end..]
                .find('\n')
                .map_or(source.len(), |p| r.end + p + 1)
        };
        if let Some(last) = blocks.last_mut()
            && start <= last.end
        {
            last.end = last.end.max(end);
            last.hits.end = i + 1;
        } else {
            blocks.push(Block {
                start,
                end,
                hits: i..i + 1,
            });
        }
    }
    blocks
}

impl Lines<'_> {
    pub(super) fn replace_block(
        &self,
        template: &str,
        source: &str,
        spans: &[Range<usize>],
        block: &Block,
    ) -> Vec<(usize, Vec<u8>)> {
        let mut lines = Vec::new();
        if self.only {
            let mut cursor = block.start;
            let mut line = 0;
            for r in &spans[block.hits.clone()] {
                line += source.as_bytes()[cursor..r.start]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count();
                let bytes = self.replacement(template, source, r.clone());
                for (i, part) in bytes.split_inclusive(|&b| b == b'\n').enumerate() {
                    lines.push((line + i, part.strip_suffix(b"\n").unwrap_or(part).to_vec()));
                }
                cursor = r.start;
            }
        } else {
            let mut cursor = block.start;
            let mut body = Vec::new();
            for r in &spans[block.hits.clone()] {
                body.extend_from_slice(&source.as_bytes()[cursor..r.start]);
                body.extend(self.replacement(template, source, r.clone()));
                cursor = r.end;
            }
            body.extend_from_slice(&source.as_bytes()[cursor..block.end]);
            if body.is_empty() {
                lines.push((0, Vec::new()));
            }
            for (i, part) in body.split_inclusive(|&b| b == b'\n').enumerate() {
                lines.push((i, part.strip_suffix(b"\n").unwrap_or(part).to_vec()));
            }
        }
        lines
    }
}
