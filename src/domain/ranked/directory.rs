//! Persistent order-statistic tree over record lengths. Byte addresses are
//! derived from subtree sums, so an edit never renumbers physical offsets
//! in unrelated leaves. Each node owns its small header; child subtrees
//! can be borrowed intact from a predecessor.
use super::{patch::Patch, storage::invalid};
use std::{collections::HashMap, io, ops::Range};

const LEAF: usize = 64;
const BRANCH: usize = 16;
pub(super) const HEADER: usize = 16;

#[derive(Default)]
pub(super) struct Node {
    children: Children,
    count: usize,
    sum: u64,
    size: usize,
    old: Option<Range<u64>>,
}
enum Children {
    Leaf(Vec<u64>),
    Branch(Vec<Node>),
}
impl Default for Children {
    fn default() -> Self {
        Self::Leaf(Vec::new())
    }
}
pub(super) enum Edit {
    Set(u64),
    Insert(u64),
    Remove,
}
fn word(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}
fn usize_at(bytes: &[u8]) -> io::Result<usize> {
    usize::try_from(word(bytes)).map_err(|_| invalid())
}
fn header(bytes: &[u8]) -> io::Result<(u8, usize)> {
    match *bytes {
        [0, n] if usize::from(n) <= LEAF => Ok((0, usize::from(n))),
        [1, n] if n > 0 && usize::from(n) <= BRANCH => Ok((1, usize::from(n))),
        _ => Err(invalid()),
    }
}

impl Node {
    fn from(children: Children) -> Self {
        let mut n = Node {
            children,
            ..Node::default()
        };
        n.refresh();
        n
    }
    fn refresh(&mut self) {
        self.old = None;
        (self.count, self.sum, self.size) = match &self.children {
            Children::Leaf(values) => (values.len(), values.iter().sum(), 2 + 8 * values.len()),
            Children::Branch(nodes) => (
                nodes.iter().map(|n| n.count).sum(),
                nodes.iter().map(|n| n.sum).sum(),
                2 + 24 * nodes.len() + nodes.iter().map(|n| n.size).sum::<usize>(),
            ),
        };
    }
    fn width(&self) -> usize {
        match &self.children {
            Children::Leaf(v) => v.len(),
            Children::Branch(v) => v.len(),
        }
    }
    fn capacity(&self) -> usize {
        match &self.children {
            Children::Leaf(_) => LEAF,
            Children::Branch(_) => BRANCH,
        }
    }
    pub(super) fn from_offsets(offsets: &[u64]) -> Self {
        if offsets.len() <= 1 {
            return Self::from(Children::Leaf(Vec::new()));
        }
        let mut nodes: Vec<_> = (0..offsets.len() - 1)
            .step_by(LEAF)
            .map(|i| {
                let group = &offsets[i..(i + LEAF + 1).min(offsets.len())];
                Self::from(Children::Leaf(
                    group.windows(2).map(|p| p[1] - p[0]).collect(),
                ))
            })
            .collect();
        while nodes.len() > 1 {
            let mut src = nodes.into_iter();
            nodes = Vec::with_capacity(src.len().div_ceil(BRANCH));
            while src.len() > 0 {
                nodes.push(Self::from(Children::Branch(
                    src.by_ref().take(BRANCH).collect(),
                )))
            }
        }
        nodes.pop().unwrap()
    }
    fn split(&mut self) -> Option<Node> {
        if self.width() <= self.capacity() {
            return None;
        }
        let mid = self.width() / 2;
        let right = match &mut self.children {
            Children::Leaf(v) => Children::Leaf(v.split_off(mid)),
            Children::Branch(v) => Children::Branch(v.split_off(mid)),
        };
        self.refresh();
        Some(Self::from(right))
    }
    fn touch(&mut self, mut index: usize, edit: Edit) -> io::Result<bool> {
        match &mut self.children {
            Children::Leaf(v) => match edit {
                Edit::Set(x) => {
                    let prev = v.get_mut(index).ok_or_else(invalid)?;
                    if *prev == x {
                        return Ok(false);
                    }
                    *prev = x;
                }
                Edit::Insert(x) => {
                    if index > v.len() {
                        return Err(invalid());
                    }
                    v.insert(index, x);
                }
                Edit::Remove => {
                    if index >= v.len() {
                        return Err(invalid());
                    }
                    v.remove(index);
                }
            },
            Children::Branch(v) => {
                let at = v.iter().position(|n| {
                    if index < n.count {
                        true
                    } else {
                        index -= n.count;
                        false
                    }
                });
                let at = match at {
                    Some(i) => i,
                    None if matches!(edit, Edit::Insert(_)) && index == 0 => {
                        let last = v.len() - 1;
                        index = v[last].count;
                        last
                    }
                    None => return Err(invalid()),
                };
                let deleting = matches!(edit, Edit::Remove);
                if !v[at].touch(index, edit)? {
                    return Ok(false);
                }
                if let Some(right) = v[at].split() {
                    v.insert(at + 1, right);
                } else if deleting && v.len() > 1 && v[at].width() < v[at].capacity() / 4 {
                    let first = at.min(v.len() - 2);
                    let right = v.remove(first + 1);
                    match (&mut v[first].children, right.children) {
                        (Children::Leaf(a), Children::Leaf(b)) => a.extend(b),
                        (Children::Branch(a), Children::Branch(b)) => a.extend(b),
                        _ => return Err(invalid()),
                    }
                    v[first].refresh();
                    if let Some(right) = v[first].split() {
                        v.insert(first + 1, right);
                    }
                }
            }
        }
        self.refresh();
        Ok(true)
    }
    pub(super) fn edit(&mut self, index: usize, edit: Edit) -> io::Result<()> {
        self.touch(index, edit)?;
        if let Some(right) = self.split() {
            let left = std::mem::take(self);
            *self = Self::from(Children::Branch(vec![left, right]));
        }
        while let Children::Branch(v) = &mut self.children {
            if v.len() != 1 {
                break;
            }
            *self = v.pop().unwrap();
        }
        Ok(())
    }
    fn encode_header(&self) -> Vec<u8> {
        let mut out = vec![
            if matches!(self.children, Children::Leaf(_)) {
                0
            } else {
                1
            },
            self.width() as u8,
        ];
        match &self.children {
            Children::Leaf(v) => {
                let mut sum = 0u64;
                for n in v {
                    sum += *n;
                    out.extend_from_slice(&sum.to_le_bytes());
                }
            }
            Children::Branch(v) => {
                for n in v {
                    for x in [n.count as u64, n.sum, n.size as u64] {
                        out.extend_from_slice(&x.to_le_bytes())
                    }
                }
            }
        }
        out
    }
    fn append(&self, out: &mut Patch) {
        if let Some(r) = self.old.clone() {
            out.borrow(r);
            return;
        }
        out.write(&self.encode_header());
        if let Children::Branch(nodes) = &self.children {
            for child in nodes {
                child.append(out)
            }
        }
    }
    fn write(&self, out: &mut impl io::Write) -> io::Result<()> {
        out.write_all(&self.encode_header())?;
        if let Children::Branch(nodes) = &self.children {
            for child in nodes {
                child.write(out)?
            }
        }
        Ok(())
    }
    fn load(bytes: &[u8], offset: u64, depth: usize) -> io::Result<Self> {
        if depth > 32 {
            return Err(invalid());
        }
        let (kind, n) = header(bytes.get(..2).ok_or_else(invalid)?)?;
        let rest = &bytes[2..];
        let children = if kind == 0 {
            if rest.len() != n * 8 {
                return Err(invalid());
            }
            let mut last = 0;
            let mut v = Vec::with_capacity(n);
            for b in rest.chunks_exact(8) {
                let end = word(b);
                v.push(end.checked_sub(last).ok_or_else(invalid)?);
                last = end;
            }
            Children::Leaf(v)
        } else {
            let (head, mut data) = rest.split_at_checked(n * 24).ok_or_else(invalid)?;
            let mut start = offset + 2 + n as u64 * 24;
            let mut nodes = Vec::with_capacity(n);
            for d in head.chunks_exact(24) {
                let size = usize_at(&d[16..])?;
                let (sub, tail) = data.split_at_checked(size).ok_or_else(invalid)?;
                let child = Self::load(sub, start, depth + 1)?;
                if child.count != usize_at(&d[..8])? || child.sum != word(&d[8..16]) {
                    return Err(invalid());
                }
                nodes.push(child);
                start += size as u64;
                data = tail;
            }
            if !data.is_empty() {
                return Err(invalid());
            }
            // Check sums before refresh aggregates them.
            nodes
                .iter()
                .try_fold(0u64, |s, n| s.checked_add(n.sum).ok_or_else(invalid))?;
            Children::Branch(nodes)
        };
        let mut node = Self::from(children);
        node.old = Some(offset..offset + bytes.len() as u64);
        Ok(node)
    }
    fn offsets(&self, start: u64) -> io::Result<Vec<u64>> {
        fn visit(n: &Node, out: &mut Vec<u64>) -> io::Result<()> {
            match &n.children {
                Children::Leaf(v) => {
                    for weight in v {
                        out.push(
                            out.last()
                                .unwrap()
                                .checked_add(*weight)
                                .ok_or_else(invalid)?,
                        );
                    }
                }
                Children::Branch(v) => {
                    for node in v {
                        visit(node, out)?
                    }
                }
            }
            Ok(())
        }
        let mut offsets = Vec::with_capacity(self.count + 1);
        offsets.push(start);
        visit(self, &mut offsets)?;
        Ok(offsets)
    }
}
pub(super) fn capacity_header<'a>(trees: impl Iterator<Item = &'a Node>) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER);
    for t in trees {
        h.extend_from_slice(&(t.size as u64).to_le_bytes())
    }
    h
}
pub(super) fn write(trees: &[Node; 2], out: &mut impl io::Write) -> io::Result<()> {
    out.write_all(&capacity_header(trees.iter()))?;
    for t in trees {
        t.write(out)?
    }
    Ok(())
}
pub(super) fn append(trees: &[Node; 2], patch: &mut Patch) {
    patch.write(&capacity_header(trees.iter()));
    for t in trees {
        t.append(patch)
    }
}
pub(super) fn load(
    bytes: &[u8],
    start: u64,
    firsts: [u64; 2],
    counts: [usize; 2],
) -> io::Result<([Node; 2], [Vec<u64>; 2])> {
    let (h, mut data) = bytes.split_at_checked(HEADER).ok_or_else(invalid)?;
    start.checked_add(bytes.len() as u64).ok_or_else(invalid)?;
    let mut offset = start + HEADER as u64;
    let mut trees = [Node::default(), Node::default()];
    let mut values = [Vec::new(), Vec::new()];
    for (i, d) in h.chunks_exact(8).enumerate() {
        let len = usize_at(d)?;
        let (sub, tail) = data.split_at_checked(len).ok_or_else(invalid)?;
        let tree = Node::load(sub, offset, 0)?;
        if tree.count != counts[i] {
            return Err(invalid());
        }
        values[i] = tree.offsets(firsts[i])?;
        trees[i] = tree;
        offset += len as u64;
        data = tail;
    }
    if !data.is_empty() {
        return Err(invalid());
    }
    Ok((trees, values))
}
type Reader<'a> = dyn FnMut(u64, &mut [u8]) -> io::Result<()> + 'a;
struct Branch {
    entries: Vec<[u64; 3]>,
}
#[derive(Default)]
pub(super) struct Cursor {
    branches: HashMap<u64, Branch>,
}
impl Cursor {
    /// Return a record's start and end, with a zero-length sentinel at count.
    pub(super) fn range(
        &mut self,
        read: &mut Reader<'_>,
        mut tree: Range<u64>,
        mut slot: usize,
        mut count: usize,
        mut start: u64,
    ) -> io::Result<Range<u64>> {
        if slot > count {
            return Err(invalid());
        }
        for _ in 0..32 {
            if tree.len_checked()? < 2 {
                return Err(invalid());
            }
            if !self.branches.contains_key(&tree.start) {
                let mut h = [0; 2];
                read(tree.start, &mut h)?;
                let (kind, n) = header(&h)?;
                if kind == 0 {
                    if n != count || tree.len_checked()? != 2 + 8 * n as u64 {
                        return Err(invalid());
                    }
                    if n == 0 {
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
                    return Ok(start.checked_add(a).ok_or_else(invalid)?
                        ..start.checked_add(b).ok_or_else(invalid)?);
                }
                if 2 + 24 * n as u64 > tree.len_checked()? {
                    return Err(invalid());
                }
                let mut data = vec![0; 24 * n];
                read(tree.start + 2, &mut data)?;
                let entries = data
                    .chunks_exact(24)
                    .map(|b| [word(&b[..8]), word(&b[8..16]), word(&b[16..])])
                    .collect::<Vec<_>>();
                let mut slots = 0u64;
                let mut bytes = 2 + 24 * n as u64;
                let mut total = 0u64;
                for &[child_slots, child_total, child_bytes] in &entries {
                    if child_bytes < 2 {
                        return Err(invalid());
                    }
                    slots = slots.checked_add(child_slots).ok_or_else(invalid)?;
                    bytes = bytes.checked_add(child_bytes).ok_or_else(invalid)?;
                    total = total.checked_add(child_total).ok_or_else(invalid)?;
                }
                if slots != count as u64
                    || bytes != tree.len_checked()?
                    || start.checked_add(total).is_none()
                {
                    return Err(invalid());
                }
                self.branches.insert(tree.start, Branch { entries });
            }
            let old = tree.clone();
            let entries = &self.branches[&old.start].entries;
            let mut child_at = old.start + 2 + 24 * entries.len() as u64;
            let mut found = false;
            for (i, &[slots, sum, bytes]) in entries.iter().enumerate() {
                let stop = child_at
                    .checked_add(bytes)
                    .filter(|&v| v <= old.end)
                    .ok_or_else(invalid)?;
                let slots = usize::try_from(slots).map_err(|_| invalid())?;
                if slot < slots || i + 1 == entries.len() && slot == slots {
                    tree = child_at..stop;
                    count = slots;
                    found = true;
                    break;
                }
                slot = slot.checked_sub(slots).ok_or_else(invalid)?;
                start = start.checked_add(sum).ok_or_else(invalid)?;
                child_at = stop;
            }
            if !found {
                return Err(invalid());
            }
        }
        Err(invalid())
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

pub(super) fn var_len(n: usize) -> u64 {
    ((usize::BITS - n.leading_zeros()).max(1) as u64).div_ceil(7)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_tree_limits_changed_bytes_and_finds_all_records_after_splits_and_merges() {
        let mut values = vec![4u64; 4_097];
        let offsets: Vec<_> = (0..=values.len()).map(|i| 4 * i as u64).collect();
        let mut original = Vec::new();
        Node::from_offsets(&offsets).write(&mut original).unwrap();
        let mut tree = Node::load(&original, 0, 0).unwrap();
        for i in 0..96 {
            tree.edit(100, Edit::Insert(i)).unwrap();
            values.insert(100, i);
        }
        for _ in 0..148 {
            tree.edit(10, Edit::Remove).unwrap();
            values.remove(10);
        }
        tree.edit(11, Edit::Set(17)).unwrap();
        values[11] = 17;
        let mut plan = Patch::default();
        tree.append(&mut plan);
        let mut bytes = Vec::new();
        let mut written = 0;
        for p in plan.pieces {
            match p {
                super::super::patch::Piece::Bytes(b) => {
                    written += b.len();
                    bytes.extend(b);
                }
                super::super::patch::Piece::Borrow(r) => {
                    bytes.extend_from_slice(&original[r.start as usize..r.end as usize])
                }
            }
        }
        assert!(written < original.len() / 5);
        let mut cursor = Cursor::default();
        let mut last = 0;
        for (i, &weight) in values.iter().chain(std::iter::once(&0)).enumerate() {
            let r = cursor
                .range(
                    &mut |off, dst| {
                        dst.copy_from_slice(&bytes[off as usize..off as usize + dst.len()]);
                        Ok(())
                    },
                    0..bytes.len() as u64,
                    i,
                    values.len(),
                    0,
                )
                .unwrap();
            assert_eq!(r, last..last + weight);
            last += weight;
        }
        bytes[2..10].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Node::load(&bytes, 0, 0).is_err());
        assert!(
            Cursor::default()
                .range(
                    &mut |off, dst| {
                        dst.copy_from_slice(&bytes[off as usize..off as usize + dst.len()]);
                        Ok(())
                    },
                    0..bytes.len() as u64,
                    0,
                    values.len(),
                    0
                )
                .is_err()
        );
    }
}
