/// Identifies a file within one published generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileId(pub u64);

/// Borrowed source shared by all consumers of this version.
#[derive(Clone, Copy)]
pub struct Document<'a> {
    pub file: FileId,
    pub text: &'a str,
}

/// Half-open UTF-8 byte offsets relative to one document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    start: usize,
    end: usize,
}

impl Span {
    pub fn new(text: &str, start: usize, end: usize) -> Option<Self> {
        text.get(start..end).map(|_| Self { start, end })
    }

    pub fn range(self) -> std::ops::Range<usize> {
        self.start..self.end
    }
}

#[derive(Debug)]
pub struct ObservedPath {
    pub path: String,
    pub stamp: [u64; 7],
    /// Some when eligibility already consumed this path's bytes.
    pub captured: Option<Option<String>>,
}

/// Git-framed identity of the exact captured working bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct BlobId {
    pub bytes: [u8; 32],
    pub len: u8,
}

impl BlobId {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}
