use super::run;
use std::collections::HashMap;

// A search that reaches EOF proves the last possible closing run for every
// width it passed. Both bracket discovery and rendering share that proof.
pub(super) struct Backticks {
    complete_from: usize,
    last: HashMap<usize, usize>,
}

pub(super) struct CodeSpan<'a> {
    pub end: usize,
    pub body: Option<&'a str>,
}

/// Check a short prefix inline before switching to the string search for a long span.
fn next_tick(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let prefix = bytes.len().min(16);
    if let Some(at) = bytes[..prefix].iter().position(|&b| b == b'`') {
        return Some(at);
    }
    let tail = text.ceil_char_boundary(prefix);
    text[tail..].find('`').map(|at| tail + at)
}

impl Backticks {
    pub(super) fn span<'a>(&mut self, source: &'a str, open: usize) -> CodeSpan<'a> {
        let width = run(source.as_bytes(), open);
        let start = open + width;
        let close = self.close(source, start, width);
        match close {
            Some(end) => CodeSpan {
                end: end + width,
                body: Some(&source[start..end]),
            },
            None => CodeSpan {
                end: start,
                body: None,
            },
        }
    }

    pub(super) fn new() -> Self {
        Self {
            complete_from: usize::MAX,
            last: HashMap::new(),
        }
    }

    pub(super) fn close(&mut self, text: &str, start: usize, width: usize) -> Option<usize> {
        if start >= self.complete_from && self.last.get(&width).is_none_or(|&at| at < start) {
            return None;
        }
        let mut pos = start;
        while let Some(delta) = next_tick(&text[pos..]) {
            pos += delta;
            let n = run(text.as_bytes(), pos);
            if n == width {
                return Some(pos);
            }
            let last = self.last.entry(n).or_insert(pos);
            *last = (*last).max(pos);
            pos += n;
        }
        self.complete_from = self.complete_from.min(start);
        None
    }
}

pub(super) fn append_code(title: &mut String, code: &str) {
    // CommonMark turns newlines into spaces before removing one surrounding
    // space. Apply both rules while appending directly to the heading title.
    let spaces = [' ', '\n'];
    let body = if code.starts_with(spaces)
        && code.ends_with(spaces)
        && !code.trim_matches(spaces).is_empty()
    {
        &code[1..code.len() - 1]
    } else {
        code
    };
    let mut lines = body.split('\n');
    title.push_str(lines.next().unwrap());
    for line in lines {
        title.push(' ');
        title.push_str(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_run_proof_survives_a_second_pass_from_an_earlier_offset() {
        let mut bytes = Vec::new();
        for width in [2, 5, 1, 5, 3, 2, 4] {
            bytes.extend(std::iter::repeat_n(b'`', width));
            bytes.push(b' ');
        }
        let mut ticks = Backticks::new();
        // Openers can be inside a run when the preceding tick was escaped.
        let mut queries = Vec::new();
        for pos in 0..bytes.len() {
            if bytes[pos] == b'`' {
                let width = run(&bytes, pos);
                queries.push((pos + width, width));
            }
        }
        for &(start, width) in queries.iter().rev().chain(&queries) {
            let expected = reference_close(&bytes, start, width);
            assert_eq!(
                ticks.close(std::str::from_utf8(&bytes).unwrap(), start, width),
                expected
            );
        }
    }

    #[test]
    fn code_text_preserves_all_space_runs_and_trims_one_normalized_space() {
        for (raw, expected) in [
            ("  ", "  "),
            ("\n \n", "   "),
            ("\n a \n", " a "),
            (" a\nb ", "a b"),
            ("\nτ\n", "τ"),
        ] {
            let mut title = String::from("Heading ");
            append_code(&mut title, raw);
            assert_eq!(title, format!("Heading {expected}"));
        }
    }
    fn reference_close(bytes: &[u8], mut pos: usize, width: usize) -> Option<usize> {
        loop {
            pos += bytes[pos..].iter().position(|&c| c == b'`')?;
            let length = run(bytes, pos);
            if length == width {
                return Some(pos);
            }
            pos += length;
        }
    }

}
