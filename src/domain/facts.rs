use super::Span;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Product {
    Paths,
    Content,
    RankedChunks,
    Symbols,
    Relationships,
    Markdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    Declarations,
    NameUses,
    Sections,
    Links,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Coverage {
    Complete,
    Partial,
    Unsupported,
}

/// Initial file-local vocabulary. The enclosing batch identifies the document.
/// A name use gains its target only after resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fact {
    Declaration { name: Span },
    NameUse { name: Span },
    Heading { level: u8, title: Span },
    Link { destination: Span },
}
