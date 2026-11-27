//! A refresh holds the writer lease while it recovers and prepares the cache.
use super::{
    File, ObservedPath, PUBLISHING, Path, PathBuf, Pin, Worker, durable, fs, io, observe, pieces,
    private,
};

pub(super) struct Recovery<'a> {
    cache: &'a Path,
    pub directory: File,
    pub marker: PathBuf,
    parents: Option<Worker<()>>,
}

impl<'a> Recovery<'a> {
    pub fn open(cache: &'a Path, current: Option<&Pin>) -> io::Result<Self> {
        let recovery = Self {
            cache,
            directory: File::open(cache)?,
            marker: cache.join(PUBLISHING),
            parents: None,
        };
        recovery.recover(current)?;
        Ok(recovery)
    }

    pub fn mark(&self) -> io::Result<()> {
        private(&self.marker)?;
        self.directory.sync_all()
    }

    // Before publishing generation one the cache and its newly created ancestors
    // must be durable. Their sync can overlap source discovery and indexing.
    pub fn observe_first(&mut self, root: &Path) -> io::Result<Vec<ObservedPath>> {
        let cache = self.cache.to_owned();
        self.parents = Some(Worker::start(move || {
            for ancestor in cache.ancestors().skip(1).take(3) {
                File::open(ancestor)?.sync_all()?;
            }
            Ok(())
        })?);
        std::thread::scope(|scope| {
            let mark = scope.spawn(|| self.mark());
            let rows = observe(root);
            mark.join()
                .map_err(|_| io::Error::other("cache preparation failed"))??;
            rows
        })
    }

    pub fn observe_current(
        &mut self,
        root: &Path,
        old: Option<&Pin>,
        checked: Option<super::lease::Checked>,
    ) -> io::Result<super::lease::Checked> {
        if let Some(checked) = checked {
            return Ok(checked);
        }
        let rows = if old.is_some() { observe(root)? } else { self.observe_first(root)? };
        Ok(super::lease::Checked::new(rows, old))
    }

    pub fn join_parents(&mut self) -> io::Result<()> {
        if let Some(parents) = self.parents.take() {
            parents.finish()?;
        }
        Ok(())
    }
    // The refresh lease excludes writers. A validated pin already owns the segment
    // handle, so unlinking an obsolete name cannot invalidate an existing reader.
    fn recover(&self, current: Option<&Pin>) -> io::Result<()> {
        if !self.marker.try_exists()? {
            return Ok(());
        }
        let cache = self.cache;
        let directory = &self.directory;
        // After a process died immediately after rename, current is visible but the
        // switch may still be volatile. Commit that choice before retiring either side.
        if current.is_some() {
            durable(&File::open(cache.join("current"))?)?;
        }
        directory.sync_all()?;
        let selected: std::collections::HashSet<_> = current
            .into_iter()
            .flat_map(|p| p.segment.keys().map(pieces::name))
            .collect();
        for entry in fs::read_dir(cache)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str()
                && !selected.contains(name)
                && engine_private(name)
            {
                fs::remove_file(entry.path())?;
            }
        }
        directory.sync_all()?;
        fs::remove_file(&self.marker)?;
        directory.sync_all()
    }
}

fn engine_private(name: &str) -> bool {
    fn generation(s: &str) -> bool {
        s.len() == 16
            && s.bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    }
    if let Some(id) = name
        .strip_prefix("segment-")
        .or_else(|| name.strip_prefix("part-"))
    {
        return generation(id);
    }
    name.strip_prefix("manifest-")
        .and_then(|s| s.split_once('-'))
        .is_some_and(|(id, pid)| {
            generation(id) && pid.parse::<u32>().is_ok_and(|n| n.to_string() == pid)
        })
}
