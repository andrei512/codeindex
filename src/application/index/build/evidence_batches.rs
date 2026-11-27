use super::{Source, Unit, retained::{Evidence, EvidenceRows}};
use crate::{
    application::index::layout::ExtentSink,
    domain::code::{BoundedOutput, Language},
};
use std::{io, ops::Range, thread::ScopedJoinHandle};

/// This source-sized planning pass runs before independent product workers start.
/// Byte buffers and auxiliary row offsets share the retired capture allowance.
pub(super) fn plan(
    units: &[Unit<'_>],
    workers: usize,
    available: usize,
) -> Option<(Vec<Range<usize>>, usize)> {
    let size = units.iter().map(weight).try_fold(0usize, usize::checked_add)?;
    let workers = workers.min(4).min(size.div_ceil(2 << 20));
    let budget = available.min(32 << 20)
        .checked_sub(units.len().checked_mul(size_of::<u64>())?)?;
    if !parallel_worthwhile(workers, budget, size) {
        return None;
    }
    let ranges = divide(units, workers, size);
    let capacity = budget / ranges.len();
    Some((ranges, capacity))
}

fn weight(unit: &Unit<'_>) -> usize {
    if Language::for_path(unit.path).is_some() { unit.text.len() } else { 0 }
}

fn parallel_worthwhile(workers: usize, budget: usize, size: usize) -> bool {
    let minimum = workers >= 2 && budget >= 4 << 20;
    minimum && size <= budget.saturating_mul(2)
}

fn divide(units: &[Unit<'_>], workers: usize, size: usize) -> Vec<Range<usize>> {
    let target = size.div_ceil(workers);
    let mut ranges = Vec::with_capacity(workers);
    let (mut start, mut accumulated) = (0, 0);
    for (i, unit) in units.iter().enumerate() {
        accumulated += weight(unit);
        if accumulated >= target && ranges.len() + 1 < workers {
            ranges.push(start..i + 1);
            start = i + 1;
            accumulated = 0;
        }
    }
    if start < units.len() {
        ranges.push(start..units.len());
    }
    ranges
}

#[derive(Default)]
pub(super) struct Batch {
    bytes: Vec<u8>,
    offsets: Vec<u64>,
}
impl Batch {
    fn write(&self, sink: &mut impl ExtentSink, offsets: &mut Vec<u64>) -> io::Result<()> {
        let base = sink.offset();
        for relative in &self.offsets {
            offsets.push(base.checked_add(*relative)
                .ok_or_else(|| io::Error::other("evidence offset overflow"))?);
        }
        sink.write_all(&self.bytes)
    }
}

pub(super) fn encode(units: &[Unit<'_>], sources: &[Option<Source>], capacity: usize) -> Batch {
    let Some(mut buffer) = BoundedOutput::new(capacity) else {
        return Batch::default();
    };
    let mut offsets = Vec::new();
    if offsets.try_reserve_exact(units.len()).is_err() {
        return Batch::default();
    }
    for (unit, source) in units.iter().zip(sources) {
        let row_start = buffer.data.len();
        if let Evidence::Encoded(code) = Evidence::classify(unit.path, source.is_some(), unit.analysis, None) {
            code.encode_bounded(&mut buffer);
            if buffer.overflowed {
                buffer.data.truncate(row_start);
                break;
            }
        }
        offsets.push(row_start as u64);
    }
    Batch {
        bytes: buffer.data,
        offsets,
    }
}

pub(super) fn write(
    sink: &mut impl ExtentSink,
    batches: Vec<(Range<usize>, ScopedJoinHandle<'_, Batch>)>,
    rows: EvidenceRows<'_>,
) -> io::Result<Vec<u64>> {
    let mut offsets = rows.offsets()?;
    for (mut range, handle) in batches {
        let batch = handle
            .join()
            .map_err(|_| io::Error::other("evidence worker failed"))?;
        batch.write(sink, &mut offsets)?;
        // Every completed offset corresponds to a row from this range.
        range.start += batch.offsets.len();
        drop(batch);
        if !range.is_empty() {
            rows.slice(range).append(sink, &mut offsets)?;
        }
    }
    offsets.push(sink.offset());
    Ok(offsets)
}

#[cfg(test)]
mod tests {
    use super::super::{Entry, Observation, analyze, tests::MemorySink};
    use super::*;

    #[test]
    fn staged_and_overflowing_batches_reproduce_serial_offsets_and_bytes() {
        let specs = [
            ("a.py", Some("def a():\n    pass\n")),
            ("b.py", None),
            ("c.rs", Some("pub fn c() {}\n")),
            ("d.txt", Some("ignored by the parser")),
            ("e.rs", Some("")),
        ];
        let sources: Vec<_> = specs
            .iter()
            .map(|(_, s)| s.map(|s| Source::Heap(s.to_owned())))
            .collect();
        let files: Vec<_> = specs
            .iter()
            .map(|(path, s)| Entry {
                score_marker: false,
                score: None,
            test_score: None,
                blob: None,
                catalog_row: 0..0,
                original_stamp: None,
                observation: Observation {
                    path: path.to_string(),
                    stamp: [0; 7],
                },
                bytes: s.map(|s| 0..s.len() as u64),
                line_ends: vec![],
                evidence: 0..0,
            })
            .collect();
        let locals: Vec<_> = specs.iter().map(|(p, s)| analyze(p, *s)).collect();
        let units: Vec<_> = specs
            .iter()
            .zip(&locals)
            .map(|((path, text), local)| Unit {
                path,
                text: text.unwrap_or(""),
                analysis: &local.code,
            })
            .collect();
        let old = vec![None; files.len()];
        let mut serial = MemorySink(vec![12; 13]);
        let evidence = || EvidenceRows { prior: None, reuse: &old, locals: &locals, files: &files };
        let offsets = evidence().write(&mut serial).unwrap();
        // Try every cutoff, including one inside the first/middle/final row.
        // A zero-length row remains complete even at the exact capacity boundary.
        for capacity in 0..=serial.0.len() - 13 {
            std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    let first = encode(&units[..4], &sources[..4], capacity);
                    let complete =
                        offsets[1..=4].partition_point(|&end| end - 13 <= capacity as u64);
                    assert_eq!(first.offsets.len(), complete);
                    assert_eq!(first.bytes, serial.0[13..offsets[complete] as usize]);
                    first
                });
                let second = scope.spawn(|| encode(&units[4..], &sources[4..], 2048));
                let mut result = MemorySink(vec![12; 13]);
                let actual = write(
                    &mut result,
                    vec![(0..4, first), (4..5, second)],
                    evidence(),
                )
                .unwrap();
                assert_eq!(offsets, actual);
                assert_eq!(serial.0, result.0);
            });
        }
    }
}
