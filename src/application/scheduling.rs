//! Admission quantities represent incremental live memory. Shared input is
//! charged to its owner once, for the lifetime of all its consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resources {
    pub workers: usize,
    pub bytes: usize,
}

impl Resources {
    pub fn fits(self, available: Self) -> bool {
        self.workers <= available.workers && self.bytes <= available.bytes
    }
}
