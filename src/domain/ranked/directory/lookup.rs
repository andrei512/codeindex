//! Read individual record ranges by walking cached branch headers and leaf endpoints.
use super::{BRANCH, Metadata, Shape, invalid, word};
use std::{collections::HashMap, io, ops::Range};

type Reader<'a> = dyn FnMut(u64, &mut [u8]) -> io::Result<()> + 'a;
pub(in crate::domain::ranked) struct Subtree {
    pub bytes: Range<u64>,
    pub count: usize,
    pub start: u64,
}

struct Branch {
    // Cumulative boundaries relative to the subtree; decode once for every lookup.
    boundaries: Vec<Metadata>,
}
impl Branch {
    fn at(read: &mut Reader<'_>, node: &Subtree) -> io::Result<Option<Self>> {
        let mut h = [0; 2];
        read(node.bytes.start, &mut h)?;
        match Shape::decode(&h)? {
            Shape::Branch(width) => Self::read(read, node, width).map(Some),
            Shape::Leaf(width) if width == node.count => Ok(None),
            Shape::Leaf(_) => Err(invalid()),
        }
    }

    fn read(read: &mut Reader<'_>, node: &Subtree, width: usize) -> io::Result<Self> {
        let len = node.bytes.len_checked()?;
        let mut end = Metadata::branch_header(width);
        if end.bytes > len {
            return Err(invalid());
        }
        let mut buffer = [0; Metadata::SIZE * BRANCH];
        let data = &mut buffer[..Metadata::SIZE * width];
        read(node.bytes.start + 2, data)?;
        let mut boundaries = Vec::with_capacity(width + 1);
        boundaries.push(end);
        for child in data.chunks_exact(Metadata::SIZE) {
            end = end.advance(Metadata::decode(child)?)?;
            boundaries.push(end);
        }
        node.start.checked_add(end.sum).ok_or_else(invalid)?;
        if (end.slots, end.bytes) != (node.count, len) {
            return Err(invalid());
        }
        Ok(Self { boundaries })
    }
    fn child(&self, node: &Subtree, slot: usize) -> io::Result<(Subtree, usize)> {
        // At the end sentinel, descend through the last child even if it is empty.
        let ends = &self.boundaries[1..];
        let i = ends
            .partition_point(|end| end.slots <= slot)
            .min(ends.len() - 1);
        let first = self.boundaries[i];
        let last = ends[i];
        let end = node
            .bytes
            .start
            .checked_add(last.bytes)
            .filter(|&n| n <= node.bytes.end)
            .ok_or_else(invalid)?;
        if slot > last.slots {
            return Err(invalid());
        }
        Ok((
            Subtree {
                bytes: node.bytes.start + first.bytes..end,
                count: last.slots - first.slots,
                start: node.start.checked_add(first.sum).ok_or_else(invalid)?,
            },
            slot - first.slots,
        ))
    }
}
#[derive(Default)]
pub(in crate::domain::ranked) struct Cursor {
    branches: HashMap<u64, Branch>,
    last_leaf: Option<(u64, usize)>,
    endpoints: Endpoints,
}
impl Cursor {
    /// Return a record's start and end, with a zero-length sentinel at count.
    pub(in crate::domain::ranked) fn range(
        &mut self,
        read: &mut Reader<'_>,
        mut node: Subtree,
        mut slot: usize,
    ) -> io::Result<Range<u64>> {
        if slot > node.count {
            return Err(invalid());
        }
        for _ in 0..=32 {
            let leaf = (node.bytes.start, node.count);
            if self.last_leaf != Some(leaf) {
                if node.bytes.len_checked()? < 2 {
                    return Err(invalid());
                }
                let branch = match self.branches.entry(node.bytes.start) {
                    std::collections::hash_map::Entry::Occupied(entry) => Some(entry.into_mut()),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        Branch::at(read, &node)?.map(|branch| entry.insert(branch))
                    }
                };
                if let Some(branch) = branch {
                    (node, slot) = branch.child(&node, slot)?;
                    continue;
                }
                self.last_leaf = Some(leaf);
            }
            return node.leaf_range(
                &mut |at, dst| self.endpoints.read(read, at, dst, node.bytes.end),
                slot,
            );
        }
        Err(invalid())
    }
}
// A single lookup reads only its endpoints. Nearby reads in the same pinned extent
// share up to eight endpoints without materializing an entire leaf.
struct Endpoints {
    start: u64,
    len: usize,
    data: [u8; 64],
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            start: 0,
            len: 0,
            data: [0; 64],
        }
    }
}
impl Endpoints {
    fn read(&mut self, read: &mut Reader<'_>, at: u64, dst: &mut [u8], end: u64) -> io::Result<()> {
        if let Some(offset) = at
            .checked_sub(self.start)
            .and_then(|n| usize::try_from(n).ok())
            && let Some(hit) = self.data[..self.len].get(offset..offset.saturating_add(dst.len()))
        {
            dst.copy_from_slice(hit);
            return Ok(());
        }
        let size = if self.len > 0 && self.start.abs_diff(at) <= 32 {
            (end - at).min(self.data.len() as u64) as usize
        } else {
            dst.len()
        };
        self.len = 0;
        read(at, &mut self.data[..size])?;
        self.start = at;
        self.len = size;
        dst.copy_from_slice(&self.data[..dst.len()]);
        Ok(())
    }
}
impl Subtree {
    /// Read just the endpoints belonging to this record, including the end sentinel.
    fn leaf_range(&self, read: &mut Reader<'_>, slot: usize) -> io::Result<Range<u64>> {
        let Self {
            bytes: tree,
            count,
            start,
        } = self;
        let (count, start) = (*count, *start);
        if tree.len_checked()? != 2 + 8 * count as u64 {
            return Err(invalid());
        }
        if count == 0 {
            return Ok(start..start);
        }
        let mut pair = [0; 16];
        let (a, b) = if slot == 0 {
            read(tree.start + 2, &mut pair[..8])?;
            (0, word(&pair[..8]))
        } else if slot == count {
            read(tree.start + 2 + 8 * (slot - 1) as u64, &mut pair[..8])?;
            let pos = word(&pair[..8]);
            (pos, pos)
        } else {
            read(tree.start + 2 + 8 * (slot - 1) as u64, &mut pair)?;
            (word(&pair[..8]), word(&pair[8..]))
        };
        if a > b {
            return Err(invalid());
        }
        Ok(start.checked_add(a).ok_or_else(invalid)?..start.checked_add(b).ok_or_else(invalid)?)
    }
}
trait RangeLen {
    fn len_checked(&self) -> io::Result<u64>;
}
impl RangeLen for Range<u64> {
    fn len_checked(&self) -> io::Result<u64> {
        self.end.checked_sub(self.start).ok_or_else(invalid)
    }
}
