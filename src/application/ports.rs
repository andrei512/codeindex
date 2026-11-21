//! Inward-facing contracts. Format implementations and storage live outside.
use crate::domain::{Capability, Coverage, Document, Fact, Product};

/// A registered format declares exactly which file-local facts it supplies.
/// Emission borrows the input and does not require a serialized intermediate.
pub trait FormatAnalyzer {
    fn capabilities(&self) -> &[Capability];
    fn analyze(&self, document: Document<'_>, output: &mut impl FactSink);
}

/// One sink per document. A producer reports coverage per declared capability.
pub trait FactSink {
    fn emit(&mut self, fact: Fact);
    fn coverage(&mut self, capability: Capability, coverage: Coverage);
}

/// A sealed section may consist of multiple worker-owned extents.
pub trait GenerationWriter {
    type Error;
    fn write_extent(&mut self, product: Product, bytes: &[u8]) -> Result<(), Self::Error>;
    /// Implementations publish all required sections atomically and durably.
    fn publish(self) -> Result<(), Self::Error>;
}
