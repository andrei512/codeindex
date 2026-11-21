//! Bulk-load disjoint runs of file IDs. Joining runs in input order keeps
//! every posting sorted without sorting or deduplicating owner IDs.
use super::Unit;
use crate::domain::{Document, FileId, content::ContentBuilder};
use std::io;

fn run(first: usize, files: &[Unit<'_>]) -> io::Result<(ContentBuilder, ContentBuilder)> {
    let mut paths = ContentBuilder::default();
    let mut bodies = ContentBuilder::default();
    for (index, unit) in files.iter().enumerate() {
        let file = FileId((first + index) as u64);
        paths.add(Document {
            file,
            text: unit.path,
        })?;
        bodies.add(Document {
            file,
            text: unit.text,
        })?;
    }
    Ok((paths, bodies))
}

pub(super) fn build(units: &[Unit<'_>]) -> io::Result<(ContentBuilder, ContentBuilder)> {
    let bytes: usize = units.iter().map(|u| u.text.len() + u.path.len()).sum();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
        .min(bytes.div_ceil(2 * 1024 * 1024));
    if workers <= 1 {
        return run(0, units);
    }
    let target = bytes.div_ceil(workers);
    std::thread::scope(|scope| {
        let mut jobs = Vec::new();
        let (mut start, mut size) = (0, 0);
        for (i, unit) in units.iter().enumerate() {
            size += unit.path.len() + unit.text.len();
            if size >= target || i + 1 == units.len() {
                let first = start;
                let slice = &units[first..=i];
                jobs.push(scope.spawn(move || run(first, slice)));
                start = i + 1;
                size = 0;
            }
        }
        let mut results = jobs.into_iter().map(|job| {
            job.join()
                .map_err(|_| io::Error::other("posting worker failed"))?
        });
        let (mut paths, mut bodies) = results.next().expect("nonempty input")?;
        for result in results {
            let (p, b) = result?;
            paths.append_later(p);
            bodies.append_later(b);
        }
        Ok((paths, bodies))
    })
}
