//! A private manifest becomes the reader-visible selector at rename.
use super::{
    File, FileKind, Path, Publication, PublicationBarrier, Remove, Worker, checkpoint, durable, fs,
    io, private,
};
use crate::adapters::snapshot::{EnvelopeHeader, write_envelope};

enum Sync {
    Inline(File),
    Staged(Worker<File>, PublicationBarrier),
}

pub(super) struct Selector {
    sync: Sync,
    temp: Remove,
}

impl Selector {
    pub(super) fn prepare(
        cache: &Path,
        generation: u64,
        bytes: Vec<u8>,
        stage: Option<PublicationBarrier>,
    ) -> io::Result<Self> {
        let path = cache.join(format!("manifest-{generation:016x}-{}", std::process::id()));
        let mut file = private(&path)?;
        let temp = Remove(Some(path));
        write_envelope(
            &mut file,
            EnvelopeHeader {
                kind: FileKind::Manifest,
                generation,
                root: 0..312,
                len: bytes.len() as u64,
            },
            &bytes[..],
        )?;
        let sync = match stage {
            Some(barrier) => {
                let writer = Worker::start(move || {
                    barrier.stage(&file)?;
                    Ok(file)
                })?;
                Sync::Staged(writer, barrier)
            }
            None => Sync::Inline(file),
        };
        Ok(Self { sync, temp })
    }

    /// The caller has verified its inputs and joined every segment writer.
    pub(super) fn replace(
        self,
        cache: &Path,
        directory: &File,
        mut parts: [Remove; 2],
    ) -> io::Result<File> {
        let Self { sync, mut temp } = self;
        let file = match sync {
            Sync::Staged(worker, _barrier) => {
                let file = worker.finish()?;
                #[cfg(target_os = "macos")]
                _barrier.commit(directory)?;
                checkpoint(Publication::SegmentDurable);
                file
            }
            Sync::Inline(file) => {
                checkpoint(Publication::SegmentDurable);
                durable(&file)?;
                file
            }
        };
        checkpoint(Publication::ManifestDurable);
        fs::rename(temp.0.as_ref().unwrap(), cache.join("current"))?;
        temp.0 = None;
        // Rename transferred both private parts to the published selector.
        // Even a subsequent directory-sync failure must preserve their names.
        for part in &mut parts {
            part.0 = None;
        }
        checkpoint(Publication::Replaced);
        directory.sync_all()?;
        checkpoint(Publication::SelectorDurable);
        Ok(file)
    }
}
