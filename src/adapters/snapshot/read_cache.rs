//! The lifetime of the cache is the lifetime of an immutable generation pin.
//! Directory probes frequently revisit the same verified pages across queries.
use super::{Envelope, envelope::BLOCK};
use std::{io, ops::Range, sync::Mutex};
const SLOTS: usize = 256;
struct Page {
    file: u64,
    block: u64,
    bytes: Vec<u8>,
}
pub(super) struct ReadCache {
    slots: Box<[Mutex<Option<Page>>]>,
}
impl Default for ReadCache {
    fn default() -> Self {
        Self {
            slots: (0..SLOTS).map(|_| Mutex::new(None)).collect(),
        }
    }
}
impl ReadCache {
    pub fn visit(
        &self,
        file: u64,
        envelope: &Envelope,
        r: Range<u64>,
        mut consume: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if r.start > r.end || r.end > envelope.payload_len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid cached read range",
            ));
        }
        let mut at = r.start;
        while at < r.end {
            let block = at / BLOCK;
            let begin = block * BLOCK;
            let end = (begin + BLOCK).min(envelope.payload_len());
            let slot = (block ^ file.wrapping_mul(0x9e3779b9)) as usize % SLOTS;
            let mut slot = self.slots[slot].lock().unwrap();
            if slot
                .as_ref()
                .is_none_or(|p| p.file != file || p.block != block)
            {
                let mut bytes = slot.take().map_or_else(Vec::new, |p| p.bytes);
                envelope.read_block(block, &mut bytes)?;
                *slot = Some(Page { file, block, bytes });
            }
            let stop = r.end.min(end);
            consume(&slot.as_ref().unwrap().bytes[(at - begin) as usize..(stop - begin) as usize])?;
            at = stop;
        }
        Ok(())
    }
}
