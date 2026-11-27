//! Source and encoded products share one logical address space. Each allocation
//! stays with its original owner until the query releases the memory index.
use crate::application::index::binary::bad;
use std::{io, ops::Range};

pub(crate) struct MemoryBytes {
    pub(super) source: Vec<u8>,
    pub(super) products: Vec<u8>,
}

impl MemoryBytes {
    pub fn slices(&self, range: Range<u64>) -> io::Result<(&[u8], &[u8])> {
        let start = usize::try_from(range.start).map_err(|_| bad())?;
        let end = usize::try_from(range.end).map_err(|_| bad())?;
        let split = self.source.len();
        if start > end {
            return Err(bad());
        }
        if end <= split {
            return Ok((&self.source[start..end], &[]));
        }
        let tail = self
            .products
            .get(start.saturating_sub(split)..end - split)
            .ok_or_else(bad)?;
        if start >= split {
            Ok((tail, &[]))
        } else {
            Ok((&self.source[start..], tail))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_ranges_borrow_from_the_original_allocations() {
        let data = MemoryBytes {
            source: b"abc".to_vec(),
            products: b"defg".to_vec(),
        };
        for start in 0..=8 {
            for end in 0..=8 {
                match b"abcdefg".get(start..end) {
                    Some(expected) => {
                        let (a, b) = data.slices(start as u64..end as u64).unwrap();
                        assert_eq!([a, b].concat(), expected);
                        assert_eq!(!b.is_empty(), start < 3 && end > 3);
                    }
                    None => assert!(data.slices(start as u64..end as u64).is_err()),
                }
            }
        }
    }
}
