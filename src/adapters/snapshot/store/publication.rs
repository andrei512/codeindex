//! Transfer the durable selector to the caller and retire obsolete part names.
use super::*;

pub(super) struct Published {
    pub selector: File,
    pub generation: u64,
    pub commit: Option<String>,
    pub provenance: Checkpoint,
    rebuilt: Option<Rebuilt>,
    unchanged_prefix: usize,
    retirement: Option<Worker<()>>,
}

impl Published {
    pub fn commit(
        verified: attempt::Verified<'_>,
        cache: &Path,
        recovery: &mut recovery::Recovery,
    ) -> io::Result<Self> {
        let attempt::Verified {
            prepared,
            selector,
            commit,
            unchanged,
        } = verified;
        let attempt::Prepared {
            candidate,
            provenance,
            generation,
            retirement,
        } = prepared;
        let (rebuilt, cleanup) = candidate.finish(&recovery.directory)?;
        recovery.join_parents()?;
        let selector = selector.replace(cache, &recovery.directory, cleanup)?;
        Ok(Self {
            selector,
            generation,
            commit,
            provenance,
            rebuilt,
            unchanged_prefix: unchanged,
            retirement,
        })
    }

    pub fn pin_and_retire(
        mut self,
        cache: &Path,
        root: PathBuf,
        old: Option<Pin>,
        recovery: &recovery::Recovery,
    ) -> io::Result<Pin> {
        let retirement = self.retirement.take();
        let (pin, retired) = self.pin(cache, root, old)?;
        retire(cache, &recovery.directory, &recovery.marker, retired)?;
        if let Some(retirement) = retirement {
            retirement.finish()?;
        }
        Ok(pin)
    }

    fn pin(
        self,
        cache: &Path,
        root: PathBuf,
        old: Option<Pin>,
    ) -> io::Result<(Pin, pieces::Retired)> {
        let Self {
            selector,
            generation,
            commit,
            provenance,
            rebuilt,
            unchanged_prefix,
            ..
        } = self;
        match rebuilt {
            Some(products) => {
                let (segment, retired) =
                    Segments::replace(cache, products.map, old.map(|p| p.segment))?;
                Ok((
                    Pin {
                        coverage: Default::default(),
                        sdk_catalog: products.sdk_catalog,
                        sdk_receipts: std::sync::OnceLock::from(products.receipts),
                        selector: Some(HeldSelector::new(selector)),
                        native_v2: true,
                        generation,
                        indexed_commit: commit,
                        segment_id: generation,
                        checkpoint: Some(provenance),
                        reused: false,
                        root,
                        files: products.files,
                        segment,
                        dir: products.dir,
                        migration: super::super::registry::Migration::current(),
                    },
                    retired,
                ))
            }
            None => {
                let mut pin = old.ok_or_else(binary::bad)?;
                // Metadata proof already compared every stamp in this prefix.
                for (file, row) in pin.files[unchanged_prefix..]
                    .iter_mut()
                    .zip(&provenance.rows[unchanged_prefix..])
                {
                    file.update_stamp(row.stamp);
                }
                pin.selector = Some(HeldSelector::new(selector));
                pin.native_v2 = true;
                pin.generation = generation;
                pin.indexed_commit = commit;
                pin.checkpoint = Some(provenance);
                pin.reused = false;
                Ok((pin, std::collections::BTreeMap::new().into_keys()))
            }
        }
    }
}

pub(super) fn retire(
    cache: &Path,
    directory: &File,
    marker: &Path,
    retired: pieces::Retired,
) -> io::Result<()> {
    if retired.len() > 0 {
        for key in retired {
            fs::remove_file(cache.join(pieces::name(key)))?;
        }
        directory.sync_all()?;
    }
    // Recovery may see this marker again after a power loss. Both the selector
    // and any unlinks it depends on are already durable.
    fs::remove_file(marker)
}
