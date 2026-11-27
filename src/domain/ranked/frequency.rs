//! BM25 needs the document count before scoring any row. Single terms carry
//! that count on disk. For positional phrases, retain one count per matching
//! row so counting documents and scoring use the same positional intersection.
use std::iter::Peekable;

pub(super) struct Frequencies<I: Iterator> {
    source: Source<I>,
    pub weight: f64,
}

enum Source<I: Iterator> {
    Positions(Peekable<I>),
    Counts(Peekable<std::vec::IntoIter<(usize, usize)>>),
}

impl<I: Iterator<Item = (usize, usize)>> Frequencies<I> {
    pub fn new(positions: I, known_documents: Option<usize>, documents: f64) -> Self {
        let (source, hits) = match known_documents {
            Some(count) => (Source::Positions(positions.peekable()), count),
            None => {
                let mut counts = Vec::new();
                for (row, _) in positions {
                    match counts.last_mut() {
                        Some((previous, count)) if *previous == row => *count += 1,
                        _ => counts.push((row, 1)),
                    }
                }
                let count = counts.len();
                (Source::Counts(counts.into_iter().peekable()), count)
            }
        };
        let hits = hits as f64;
        let idf = ((documents - hits + 0.5) / (hits + 0.5)).ln();
        Self {
            source,
            weight: if idf <= 0.0 { 1e-6 } else { idf },
        }
    }

    pub fn next_row(&mut self) -> Option<usize> {
        match &mut self.source {
            Source::Positions(p) => p.peek().map(|v| v.0),
            Source::Counts(c) => c.peek().map(|v| v.0),
        }
    }

    pub fn take_row(&mut self, row: usize) -> f64 {
        match &mut self.source {
            Source::Positions(p) => {
                let mut count = 0;
                while p.next_if(|v| v.0 == row).is_some() {
                    count += 1usize;
                }
                count as f64
            }
            Source::Counts(c) => c.next_if(|v| v.0 == row).map_or(0., |v| v.1 as f64),
        }
    }
}
