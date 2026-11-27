//! Bulk-load disjoint runs of file IDs. Joining runs in input order keeps
//! every posting sorted without sorting or deduplicating owner IDs.
use super::{Unit, incremental::Postings};
use crate::domain::content::fresh::{Product, Run};
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

fn shard(first: usize, units: &[Unit<'_>]) -> io::Result<(ContentBuilder, Run)> {
    let mut paths = ContentBuilder::default();
    let mut bodies = Run::new();
    for (i, unit) in units.iter().enumerate() {
        let id = first + i;
        paths.add(Document {
            file: FileId(id as u64),
            text: unit.path,
        })?;
        bodies.add(id as u32, unit.text);
    }
    Ok((paths, bodies))
}

pub(super) fn build(units: &[Unit<'_>]) -> io::Result<(ContentBuilder, Postings)> {
    let bytes: usize = units.iter().map(|u| u.text.len() + u.path.len()).sum();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
        .min(bytes.div_ceil(2 * 1024 * 1024));
    if workers <= 1 || units.len() > u32::MAX as usize {
        let (paths, bodies) = run(0, units)?;
        return Ok((paths, Postings::Built(bodies)));
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
                jobs.push(scope.spawn(move || shard(first, slice)));
                start = i + 1;
                size = 0;
            }
        }
        let mut results = jobs.into_iter().map(|job| {
            job.join()
                .map_err(|_| io::Error::other("posting worker failed"))?
        });
        let (mut paths, first) = results.next().expect("nonempty input")?;
        let mut bodies = vec![first];
        for result in results {
            let (p, b) = result?;
            paths.append_later(p);
            bodies.push(b);
        }
        Ok((paths, Postings::Fresh(Product::transpose(bodies)?)))
    })
}
