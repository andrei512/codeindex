//! Gather a consecutive group of variable-length rows using their offset directory.
//! Bound scratch space by the larger of one row and the read window.
use super::{ExtentRead, bad, io};

const OFFSET_ROWS: usize = 128;
const WINDOW: u64 = 64 * 1024;

pub(in crate::application::index) struct Rows<'a> {
    pub table: &'a std::ops::Range<u64>,
    pub data_end: u64,
}

impl Rows<'_> {
    pub fn visit(
        &self,
        reader: &mut impl ExtentRead,
        ids: &[[u8; 8]],
        mut row: impl FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let limit = (self.table.end - self.table.start) / 8 - 1;
        let mut group = Group {
            data_end: self.data_end,
            bytes: Vec::new(),
            row: &mut row,
        };
        let adjacent = |a: &[u8; 8], b: &[u8; 8]| {
            u64::from_le_bytes(*a).checked_add(1) == Some(u64::from_le_bytes(*b))
        };
        for run in ids.chunk_by(adjacent) {
            // Consecutive ordinals make the last member an upper bound for all.
            if u64::from_le_bytes(run[run.len() - 1]) >= limit {
                return Err(bad());
            }
            self.visit_run(reader, run, &mut group)?;
        }
        Ok(())
    }

    fn visit_run(
        &self,
        reader: &mut impl ExtentRead,
        run: &[[u8; 8]],
        group: &mut Group<'_, impl FnMut(&[u8]) -> io::Result<()>>,
    ) -> io::Result<()> {
        for ids in run.chunks(OFFSET_ROWS) {
            let mut raw = [0; (OFFSET_ROWS + 1) * 8];
            let offsets = &mut raw[..(ids.len() + 1) * 8];
            reader.read(self.table.start + u64::from_le_bytes(ids[0]) * 8, offsets)?;
            group.read(reader, offsets.as_chunks::<8>().0)?;
        }
        Ok(())
    }
}

struct Group<'a, F> {
    data_end: u64,
    bytes: Vec<u8>,
    row: &'a mut F,
}

impl<F: FnMut(&[u8]) -> io::Result<()>> Group<'_, F> {
    fn read(&mut self, reader: &mut impl ExtentRead, mut offsets: &[[u8; 8]]) -> io::Result<()> {
        while offsets.len() > 1 {
            let (count, from, to) = window(offsets, self.data_end)?;
            self.bytes
                .resize(usize::try_from(to - from).map_err(|_| bad())?, 0);
            reader.read(from, &mut self.bytes)?;
            self.visit(&offsets[1..=count], from)?;
            offsets = &offsets[count..];
        }
        Ok(())
    }

    fn visit(&mut self, offsets: &[[u8; 8]], from: u64) -> io::Result<()> {
        let mut start = 0;
        for &word in offsets {
            let end = (u64::from_le_bytes(word) - from) as usize;
            (self.row)(&self.bytes[start..end])?;
            start = end;
        }
        Ok(())
    }
}

fn window(offsets: &[[u8; 8]], data_end: u64) -> io::Result<(usize, u64, u64)> {
    let from = u64::from_le_bytes(offsets[0]);
    let mut to = from;
    let mut count = 0;
    for &word in &offsets[1..] {
        let next = checked_offset(word, to..=data_end)?;
        if count > 0 && next - from > WINDOW {
            break;
        }
        count += 1;
        to = next;
    }
    Ok((count, from, to))
}

fn checked_offset(word: [u8; 8], valid: std::ops::RangeInclusive<u64>) -> io::Result<u64> {
    let next = u64::from_le_bytes(word);
    valid.contains(&next).then_some(next).ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_windows_preserve_large_rows_and_reject_invalid_boundaries() {
        let offsets = [2u64, 5, WINDOW + 10, WINDOW + 11].map(u64::to_le_bytes);
        assert_eq!(window(&offsets, WINDOW + 11).unwrap(), (1, 2, 5));
        assert_eq!(
            window(&offsets[1..], WINDOW + 11).unwrap(),
            (1, 5, WINDOW + 10)
        );
        assert!(window(&[5u64, 4].map(u64::to_le_bytes), 10).is_err());
        assert!(window(&offsets[1..], WINDOW).is_err());
    }
}
