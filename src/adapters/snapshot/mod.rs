//! Native snapshot files. Publication is separate from writing private files.
mod crc;
mod envelope;

pub use envelope::{Envelope, FileKind, SegmentWriter, write_envelope};
pub mod manifest;

mod store;
pub use store::Pin;

mod checkpoint;
mod refresh_plan;

mod pieces;

mod registry;

mod read_cache;
