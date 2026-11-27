use std::{io::Write, ops::Range};
pub const FILES: usize = 1;
pub const SOURCE: usize = 2;
pub const PATHGRAM: usize = 3;
pub const CONTENT: usize = 4;
pub const RANK: usize = 5;
pub const SYMBOLS: usize = 6;
pub const REL: usize = 7;
pub const MD: usize = 8;
pub(crate) const N: usize = 8;
#[derive(Clone, Default)]
pub struct Directory(pub [Range<u64>; N]);

impl Directory {
    pub(super) fn write_product<W: ExtentSink, T>(&mut self, sink: &mut W, kind: usize,
        emit: impl FnOnce(&mut W) -> std::io::Result<T>) -> std::io::Result<T> {
        let start = sink.offset();
        let value = emit(sink)?;
        self.0[kind - 1] = start..sink.offset();
        Ok(value)
    }
}

pub trait ExtentSink: Write {
    fn offset(&self) -> u64;
    /// Advance by a verified logical range of the predecessor when this sink
    /// can keep its backing extent reachable through publication.
    fn retain(&mut self, _range: Range<u64>) -> std::io::Result<bool> {
        Ok(false)
    }
}

impl<W: ExtentSink> ExtentSink for std::io::BufWriter<&mut W> {
    fn offset(&self) -> u64 {
        self.get_ref().offset() + self.buffer().len() as u64
    }
}

impl ExtentSink for Vec<u8> {
    fn offset(&self) -> u64 { self.len() as u64 }
}
