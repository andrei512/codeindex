//! A selector describes either the pinned products or a complete staged replacement.
use super::{Checkpoint, File, Pin, PublicationBarrier, Rebuilt, Remove, io, manifest, staged};

pub(super) enum Candidate<'a> {
    Retained(&'a Pin),
    Rebuilt(staged::Parts),
}

impl Candidate<'_> {
    pub fn manifest(
        &mut self,
        identity: manifest::Identity<'_>,
        generation: u64,
        provenance: &Checkpoint,
    ) -> io::Result<(Vec<u8>, Option<PublicationBarrier>)> {
        let (dir, segment, map, catalog, barrier) = match self {
            Self::Retained(pin) => (&pin.dir, pin.segment_id, &pin.segment.map, &pin.files, None),
            Self::Rebuilt(parts) => {
                parts.link_rank(generation)?;
                let products = &parts.products;
                (
                    &products.dir,
                    generation,
                    &products.map,
                    &products.files,
                    Some(parts.barrier),
                )
            }
        };
        let bytes = dir.manifest(identity, segment, map, |out| {
            provenance.encode_catalog(out, catalog)
        })?;
        Ok((bytes, barrier))
    }

    pub fn sdk_current(&self) -> io::Result<bool> {
        match self {
            Self::Retained(pin) => pin.sdk_current(),
            Self::Rebuilt(parts) => Ok(parts
                .products
                .receipts
                .iter()
                .all(|receipt| receipt.valid())),
        }
    }

    pub fn finish(self, directory: &File) -> io::Result<(Option<Rebuilt>, [Remove; 2])> {
        match self {
            Self::Retained(_) => Ok((None, [Remove(None), Remove(None)])),
            Self::Rebuilt(parts) => {
                let (products, cleanup) = parts.finish(directory)?;
                Ok((Some(products), cleanup))
            }
        }
    }
}
