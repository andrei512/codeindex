//! Private product files become selectable only after their writes finish.
use super::{
    File, Path, PublicationBarrier, Rebuilt, Remove, Worker, binary, io, manifest, pieces, private,
};
use crate::{
    adapters::snapshot::SegmentWriter,
    application::index::build::{Output, PathOrder, RankLane},
};
use std::sync::mpsc::{self, Receiver};

/// Reserve a generation and arm cleanup before any writer can fail.
fn create_primary(cache: &Path, generation: &mut u64) -> io::Result<(File, Remove)> {
    loop {
        let path = cache.join(super::segment_name(*generation));
        match private(&path) {
            Ok(file) => return Ok((file, Remove(Some(path)))),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                *generation = generation.checked_add(1).ok_or_else(binary::bad)?;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The primary writer owns its private name until both product streams have
/// finished. A failed build drops these handles before the next capture.
pub(super) struct Primary<'a> {
    pub writer: pieces::Writer<'a>,
    rank: Option<RankFile>,
    barrier: PublicationBarrier,
    cleanup: Remove,
}

impl<'a> Primary<'a> {
    pub fn open(
        cache: &Path,
        directory: &File,
        generation: &mut u64,
        (prior, fresh): (Option<&'a pieces::Segments>, bool),
    ) -> io::Result<(Self, Option<RankLane>)> {
        let barrier = PublicationBarrier::new(directory)?;
        let (file, cleanup) = create_primary(cache, generation)?;
        let writer = pieces::Writer::new(file, *generation, prior)?;
        let (lane, rank) = fresh
            .then(|| rank_lane(cache, *generation, barrier))
            .transpose()?
            .unzip();
        Ok((
            Self {
                writer,
                rank,
                barrier,
                cleanup,
            },
            lane,
        ))
    }

    pub fn stage(self, output: Output) -> io::Result<(Parts, Option<Worker<()>>, PathOrder)> {
        let Output {
            paths,
            dir,
            files,
            rank_write,
            retired,
            sdk_catalog,
            sdk_receipts,
        } = output;
        let retirement = if rank_write.is_some() && retired.len() >= 128 {
            Some(Worker::start(move || {
                drop(retired);
                Ok(())
            })?)
        } else {
            drop(retired);
            None
        };
        let (file, map) = self.writer.finish()?;
        let barrier = self.barrier;
        let primary = Worker::start(move || barrier.stage(&file))?;
        let products = Rebuilt {
            dir,
            map,
            files,
            sdk_catalog,
            receipts: sdk_receipts,
        };
        Ok((
            Parts {
                products,
                primary,
                rank: rank_write.zip(self.rank),
                barrier,
                cleanup: self.cleanup,
            },
            retirement,
            paths,
        ))
    }
}

pub(super) struct Parts {
    pub products: Rebuilt,
    primary: Worker<()>,
    rank: Option<(Worker<u64>, RankFile)>,
    pub barrier: PublicationBarrier,
    cleanup: Remove,
}

impl Parts {
    pub fn link_rank(&mut self, generation: u64) -> io::Result<()> {
        let Some((_, file)) = &self.rank else {
            return Ok(());
        };
        let size = match file.length.recv() {
            Ok(size) => size,
            Err(_) => {
                return Err(self
                    .rank
                    .take()
                    .unwrap()
                    .0
                    .finish()
                    .err()
                    .unwrap_or_else(binary::bad));
            }
        };
        let extent = &mut self.products.dir.0[manifest::RANK - 1];
        extent.end = extent.start.checked_add(size).ok_or_else(binary::bad)?;
        self.products.map.append_rank(generation, extent.clone())
    }

    pub fn finish(self, _directory: &File) -> io::Result<(Rebuilt, [Remove; 2])> {
        self.primary.finish()?;
        let rank_cleanup = if let Some((rank, file)) = self.rank {
            rank.finish()?;
            file.cleanup
        } else {
            Remove(None)
        };
        #[cfg(not(target_os = "macos"))]
        _directory.sync_all()?;
        Ok((self.products, [self.cleanup, rank_cleanup]))
    }
}

struct RankFile {
    cleanup: Remove,
    length: Receiver<u64>,
}

fn rank_lane(
    cache: &Path,
    generation: u64,
    barrier: PublicationBarrier,
) -> io::Result<(RankLane, RankFile)> {
    let path = cache.join(pieces::name(pieces::LEGACY | generation));
    let file = private(&path)?;
    let (send, length) = mpsc::channel();
    let lane: RankLane = Box::new(move |rank| {
        Worker::start(move || {
            let mut writer = SegmentWriter::new(file)?;
            rank.write_to(&mut writer)?;
            let size = writer.offset();
            let file = writer.finish(generation)?;
            // Manifest layout needs the length before publication needs durability.
            let _ = send.send(size);
            barrier.stage(&file)?;
            Ok(size)
        })
    });
    Ok((
        lane,
        RankFile {
            cleanup: Remove(Some(path)),
            length,
        },
    ))
}
