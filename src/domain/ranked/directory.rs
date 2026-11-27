//! Persistent order-statistic tree over record lengths. Byte addresses are
//! derived from subtree sums, so an edit never renumbers physical offsets
//! in unrelated leaves. Each node owns its small header; child subtrees
//! can be borrowed intact from a predecessor.
use super::{patch::Patch, storage::invalid};
mod lookup;
pub(super) use lookup::{Cursor, Subtree};
use std::{io, ops::Range};

const LEAF: usize = 64;
const BRANCH: usize = 16;
pub(super) const HEADER: usize = 16;

#[derive(Default)]
pub(super) struct Node {
    children: Children,
    metadata: Metadata,
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
#[derive(Clone, Copy)]
pub(super) enum Edit {
    Set(u64),
    Insert(u64),
    Remove,
}
impl Edit {
    fn apply_leaf(self, v: &mut Vec<u64>, index: usize) -> io::Result<bool> {
        if let Edit::Set(value) = self {
            let slot = v.get_mut(index).ok_or_else(invalid)?;
            return Ok(std::mem::replace(slot, value) != value);
        }
        match self {
            Edit::Insert(value) if index <= v.len() => v.insert(index, value),
            Edit::Remove if index < v.len() => { v.remove(index); }
            _ => return Err(invalid()),
        }
        Ok(true)
    }
}
fn word(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}
enum Shape {
    Leaf(usize),
    Branch(usize),
}

impl Shape {
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        match *bytes {
            [0, n] if usize::from(n) <= LEAF => Ok(Self::Leaf(usize::from(n))),
            [1, n] if n > 0 && usize::from(n) <= BRANCH => Ok(Self::Branch(usize::from(n))),
            _ => Err(invalid()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Metadata {
    slots: usize,
    sum: u64,
    bytes: u64,
}

impl Metadata {
    const SIZE: usize = 24;

    fn advance(self, child: Self) -> io::Result<Self> {
        Ok(Self {
            slots: self.slots.checked_add(child.slots).ok_or_else(invalid)?,
            sum: self.sum.checked_add(child.sum).ok_or_else(invalid)?,
            bytes: self.bytes.checked_add(child.bytes).ok_or_else(invalid)?,
        })
    }

    fn branch_header(width: usize) -> Self {
        Self { bytes: 2 + (Self::SIZE * width) as u64, ..Self::default() }
    }

    fn encode(self, bytes: &mut Vec<u8>) {
        for word in [self.slots as u64, self.sum, self.bytes] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }

    fn decode(row: &[u8]) -> io::Result<Self> {
        let bytes = usize::try_from(word(&row[16..])).map_err(|_| invalid())?;
        if bytes < 2 {
            return Err(invalid());
        }
        Ok(Self {
            slots: usize::try_from(word(&row[..8])).map_err(|_| invalid())?,
            sum: word(&row[8..16]),
            bytes: bytes as u64,
        })
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
        self.metadata = match &self.children {
            Children::Leaf(values) => Metadata {
                slots: values.len(),
                sum: values.iter().sum(),
                bytes: 2 + 8 * values.len() as u64,
            },
            Children::Branch(nodes) => nodes.iter()
                .try_fold(Metadata::branch_header(nodes.len()), |total, child| {
                    total.advance(child.metadata)
                })
                .expect("directory already fits in memory and indexes representable record offsets"),
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
    fn touch(&mut self, index: usize, edit: Edit) -> io::Result<bool> {
        let changed = match &mut self.children {
            Children::Leaf(v) => edit.apply_leaf(v, index)?,
            Children::Branch(v) => Self::edit_child(v, index, edit)?,
        };
        if changed {
            self.refresh();
        }
        Ok(changed)
    }

    fn edit_child(nodes: &mut Vec<Node>, mut index: usize, edit: Edit) -> io::Result<bool> {
        // The last child owns the end insertion point. Its leaf validates
        // out-of-range edits before mutating anything.
        let last = nodes.len() - 1;
        let mut at = 0;
        while at < last && index >= nodes[at].metadata.slots {
            index -= nodes[at].metadata.slots;
            at += 1;
        }
        if !nodes[at].touch(index, edit)? {
            return Ok(false);
        }
        if matches!(edit, Edit::Insert(_)) {
            Self::split_child(nodes, at);
        }
        if matches!(edit, Edit::Remove) && last > 0 && nodes[at].underfull() {
            Self::rebalance_pair(nodes, at.min(last - 1))?;
        }
        Ok(true)
    }
    fn underfull(&self) -> bool {
        self.width() < self.capacity() / 4
    }

    fn split_child(nodes: &mut Vec<Node>, at: usize) -> bool {
        let Some(right) = nodes[at].split() else {
            return false;
        };
        nodes.insert(at + 1, right);
        true
    }

    fn rebalance_pair(nodes: &mut Vec<Node>, first: usize) -> io::Result<()> {
        let right = nodes.remove(first + 1);
        let left = &mut nodes[first];
        match (&mut left.children, right.children) {
            (Children::Leaf(a), Children::Leaf(b)) => a.extend(b),
            (Children::Branch(a), Children::Branch(b)) => a.extend(b),
            _ => return Err(invalid()),
        }
        if !Self::split_child(nodes, first) {
            nodes[first].refresh();
        }
        Ok(())
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
    fn encode_header<'b>(&self, buffer: &'b mut Vec<u8>) -> &'b [u8] {
        buffer.clear();
        buffer.push(u8::from(matches!(self.children, Children::Branch(_))));
        buffer.push(self.width() as u8);
        match &self.children {
            Children::Leaf(weights) => encode_endpoints(weights, buffer),
            Children::Branch(nodes) => nodes.iter().for_each(|node| node.metadata.encode(buffer)),
        }
        buffer
    }
    /// A retained subtree already includes its children in their serialized order.
    fn visit<E>(&self, emit: &mut impl FnMut(&Node) -> Result<bool, E>) -> Result<(), E> {
        if emit(self)?
            && let Children::Branch(nodes) = &self.children
        {
            for child in nodes {
                child.visit(emit)?;
            }
        }
        Ok(())
    }
    fn write(&self, out: &mut impl io::Write, buffer: &mut Vec<u8>) -> io::Result<()> {
        self.visit(&mut |node| {
            out.write_all(node.encode_header(buffer))?;
            Ok::<_, io::Error>(true)
        })
    }
    fn append(&self, patch: &mut Patch, buffer: &mut Vec<u8>) {
        let Ok(()) = self.visit(&mut |node| {
            if let Some(range) = &node.old {
                patch.borrow(range.clone());
                return Ok(false);
            }
            patch.write(node.encode_header(buffer));
            Ok::<_, std::convert::Infallible>(true)
        });
    }
}

fn encode_endpoints(weights: &[u64], output: &mut Vec<u8>) {
    let mut end = 0u64;
    for weight in weights {
        end += weight;
        output.extend_from_slice(&end.to_le_bytes());
    }
}

/// The same decoder can validate a tree or retain its nodes and record offsets
/// for an incremental edit. Validation only needs the running count and address.
struct Decoder {
    end: u64,
    count: usize,
    offsets: Option<Vec<u64>>,
    leaf_depth: Option<usize>,
}

impl Decoder {
    fn new(first: u64, entries: Option<usize>) -> Self {
        let offsets = entries.map(|count| {
            let mut v = Vec::with_capacity(count + 1);
            v.push(first);
            v
        });
        Self {
            end: first,
            count: 0,
            offsets,
            leaf_depth: None,
        }
    }

    fn node(
        &mut self,
        bytes: &[u8],
        offset: u64,
        depth: usize,
        parent: Option<Metadata>,
    ) -> io::Result<Option<Node>> {
        if depth > 32 {
            return Err(invalid());
        }
        let shape = Shape::decode(bytes.get(..2).ok_or_else(invalid)?)?;
        let start = self.end;
        let count = self.count;
        let children = match shape {
            Shape::Leaf(width) => self.leaf(&bytes[2..], width, depth)?,
            Shape::Branch(width) => self.branch(&bytes[2..], width, offset, depth)?,
        };
        let metadata = Metadata {
            slots: self.count - count,
            sum: self.end - start,
            bytes: bytes.len() as u64,
        };
        if parent.is_some_and(|expected| expected != metadata) {
            return Err(invalid());
        }
        Ok(children.map(|children| Node {
            children,
            metadata,
            old: Some(offset..offset + metadata.bytes),
        }))
    }

    fn leaf(&mut self, data: &[u8], n: usize, depth: usize) -> io::Result<Option<Children>> {
        // Rebalancing can only merge nodes of the same height.
        if data.len() != n * 8 || *self.leaf_depth.get_or_insert(depth) != depth {
            return Err(invalid());
        }
        let first = self.end;
        let mut weights = self.offsets.as_ref().map(|_| Vec::with_capacity(n));
        for b in data.chunks_exact(8) {
            let end = first.checked_add(word(b)).ok_or_else(invalid)?;
            let weight = end.checked_sub(self.end).ok_or_else(invalid)?;
            if let Some((weights, offsets)) = weights.as_mut().zip(self.offsets.as_mut()) {
                weights.push(weight);
                offsets.push(end);
            }
            self.end = end;
        }
        self.count += n;
        Ok(weights.map(Children::Leaf))
    }

    fn branch(
        &mut self,
        bytes: &[u8],
        n: usize,
        offset: u64,
        depth: usize,
    ) -> io::Result<Option<Children>> {
        let (headers, mut data) = bytes
            .split_at_checked(n * Metadata::SIZE)
            .ok_or_else(invalid)?;
        let mut at = offset + 2 + headers.len() as u64;
        let mut nodes = self.offsets.as_ref().map(|_| Vec::with_capacity(n));
        for row in headers.chunks_exact(Metadata::SIZE) {
            let header = Metadata::decode(row)?;
            let (child, tail) = data.split_at_checked(header.bytes as usize).ok_or_else(invalid)?;
            let node = self.node(child, at, depth + 1, Some(header))?;
            if let Some(nodes) = &mut nodes {
                nodes.push(node.unwrap());
            }
            at += header.bytes;
            data = tail;
        }
        if !data.is_empty() {
            return Err(invalid());
        }
        Ok(nodes.map(Children::Branch))
    }
}
/// The file and term tree share one pair of byte-length fields on disk.
pub(super) fn tree_ranges(header: &[u8; HEADER], at: u64) -> io::Result<[Range<u64>; 2]> {
    let first = at.checked_add(HEADER as u64).ok_or_else(invalid)?;
    let middle = first.checked_add(word(&header[..8])).ok_or_else(invalid)?;
    let end = middle.checked_add(word(&header[8..])).ok_or_else(invalid)?;
    Ok([first..middle, middle..end])
}
fn capacity_header(trees: &[Node; 2]) -> [u8; HEADER] {
    let mut header = [0; HEADER];
    for (bytes, tree) in header.chunks_exact_mut(8).zip(trees) {
        bytes.copy_from_slice(&tree.metadata.bytes.to_le_bytes());
    }
    header
}
pub(super) fn write(trees: &[Node; 2], out: &mut impl io::Write) -> io::Result<()> {
    out.write_all(&capacity_header(trees))?;
    let mut buffer = Vec::new();
    for tree in trees {
        tree.write(out, &mut buffer)?;
    }
    Ok(())
}
pub(super) fn append(trees: &[Node; 2], patch: &mut Patch) {
    patch.write(&capacity_header(trees));
    let mut buffer = Vec::new();
    for tree in trees {
        tree.append(patch, &mut buffer);
    }
}
pub(super) type Expanded = ([Node; 2], [Vec<u64>; 2]);
pub(super) fn load(
    bytes: &[u8],
    start: u64,
    firsts: [u64; 2],
    counts: [usize; 2],
) -> io::Result<Expanded> {
    let [files, terms] = DirectoryInput::new(bytes, start, firsts, counts)?.trees;
    let (files, file_offsets) = files.expand()?;
    let (terms, term_offsets) = terms.expand()?;
    Ok(([files, terms], [file_offsets, term_offsets]))
}

pub(super) fn validate(bytes: &[u8], firsts: [u64; 2], counts: [usize; 2]) -> io::Result<[u64; 2]> {
    let [files, terms] = DirectoryInput::new(bytes, 0, firsts, counts)?.trees;
    Ok([files.decode(None)?.0.end, terms.decode(None)?.0.end])
}

struct TreeInput<'a> {
    bytes: &'a [u8],
    offset: u64,
    first: u64,
    count: usize,
}

impl TreeInput<'_> {
    fn decode(self, entries: Option<usize>) -> io::Result<(Decoder, Option<Node>)> {
        if self.count > self.bytes.len() / 8 {
            return Err(invalid());
        }
        let mut decoder = Decoder::new(self.first, entries);
        let root = decoder.node(self.bytes, self.offset, 0, None)?;
        if decoder.count != self.count {
            return Err(invalid());
        }
        Ok((decoder, root))
    }

    fn expand(self) -> io::Result<(Node, Vec<u64>)> {
        let count = self.count;
        let (decoder, root) = self.decode(Some(count))?;
        Ok((root.unwrap(), decoder.offsets.unwrap()))
    }
}

struct DirectoryInput<'a> {
    trees: [TreeInput<'a>; 2],
}

impl<'a> DirectoryInput<'a> {
    fn new(bytes: &'a [u8], start: u64, firsts: [u64; 2], counts: [usize; 2]) -> io::Result<Self> {
        let (header, data) = bytes.split_at_checked(HEADER).ok_or_else(invalid)?;
        let end = start.checked_add(bytes.len() as u64).ok_or_else(invalid)?;
        let [files, terms] = tree_ranges(header.try_into().unwrap(), start)?;
        if terms.end != end {
            return Err(invalid());
        }
        let (file_bytes, term_bytes) = data.split_at((files.end - files.start) as usize);
        let trees = std::array::from_fn(|i| TreeInput {
            bytes: [file_bytes, term_bytes][i],
            offset: [files.start, terms.start][i],
            first: firsts[i],
            count: counts[i],
        });
        Ok(Self { trees })
    }
}
pub(super) fn var_len(n: usize) -> u64 {
    ((usize::BITS - n.leading_zeros()).max(1) as u64).div_ceil(7)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_expands_directory_offsets_from_each_first_record_address() {
        let mut offsets = vec![19];
        for i in 0..LEAF * BRANCH + 7 {
            offsets.push(offsets.last().unwrap() + (i % 5) as u64);
        }
        let trees = [Node::from_offsets(&offsets), Node::from_offsets(&[])];
        let mut bytes = Vec::new();
        write(&trees, &mut bytes).unwrap();
        let counts = [offsets.len() - 1, 0];
        let (_, actual) = load(&bytes, 103, [19, 79], counts).unwrap();
        assert_eq!(actual, [offsets, vec![79]]);
        assert!(load(&bytes, 103, [u64::MAX - 1, 79], counts).is_err());
        assert!(load(&bytes, 103, [19, 79], [usize::MAX, 0]).is_err());
    }

    #[test]
    fn validation_and_expanded_edits_agree_on_corrupt_directories() {
        let offsets: Vec<_> = (0..=LEAF * BRANCH + 17).map(|i| 3 * i as u64).collect();
        let trees = [Node::from_offsets(&offsets), Node::from_offsets(&[0, 3, 6])];
        let counts = [offsets.len() - 1, 2];
        let starts = [16, 10000];
        let mut bytes = Vec::new();
        write(&trees, &mut bytes).unwrap();
        let expected = validate(&bytes, starts, counts).unwrap();
        assert_eq!(expected, [16 + 3 * counts[0] as u64, 10006]);
        // Include capacity headers, branch headers, leaf weights and tail lengths.
        for at in (0..bytes.len()).step_by(7) {
            bytes[at] ^= 0x80;
            let checked = validate(&bytes, starts, counts);
            let expanded = load(&bytes, 0, starts, counts)
                .map(|(_, offsets)| offsets.map(|v| *v.last().unwrap()));
            assert_eq!(checked.ok(), expanded.ok(), "byte {at}");
            bytes[at] ^= 0x80;
        }
    }
    #[test]
    fn mixed_leaf_depth_is_rejected_before_an_edit_can_rebalance_incompatible_siblings() {
        let tree = Node::from(Children::Branch(vec![
            Node::from(Children::Leaf(vec![2; 8])),
            Node::from(Children::Branch(vec![Node::from(Children::Leaf(vec![
                1;
                5
            ]))])),
        ]));
        let mut bytes = Vec::new();
        write(&[tree, Node::from_offsets(&[])], &mut bytes).unwrap();
        assert!(validate(&bytes, [0, 0], [13, 0]).is_err());
        assert!(load(&bytes, 0, [0, 0], [13, 0]).is_err());
    }

    #[test]
    fn persistent_tree_limits_changed_bytes_and_finds_all_records_after_splits_and_merges() {
        let mut values = vec![4u64; 4_097];
        let offsets: Vec<_> = (0..=values.len()).map(|i| 4 * i as u64).collect();
        let mut original = Vec::new();
        Node::from_offsets(&offsets)
            .write(&mut original, &mut Vec::new())
            .unwrap();
        let mut tree = Decoder::new(0, Some(0))
            .node(&original, 0, 0, None)
            .unwrap()
            .unwrap();
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
        tree.append(&mut plan, &mut Vec::new());
        let (mut bytes, written) = materialize_patch(plan, &original);
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
                    Subtree {
                        bytes: 0..bytes.len() as u64,
                        count: values.len(),
                        start: 0,
                    },
                    i,
                )
                .unwrap();
            assert_eq!(r, last..last + weight);
            last += weight;
        }
        bytes[2..10].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!((Decoder::new(0, None)).node(&bytes, 0, 0, None).is_err());
        assert!(
            Cursor::default()
                .range(
                    &mut |off, dst| {
                        dst.copy_from_slice(&bytes[off as usize..off as usize + dst.len()]);
                        Ok(())
                    },
                    Subtree {
                        bytes: 0..bytes.len() as u64,
                        count: values.len(),
                        start: 0
                    },
                    0
                )
                .is_err()
        );
    }
    fn materialize_patch(plan: Patch, original: &[u8]) -> (Vec<u8>, usize) {
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
        (bytes, written)
    }

}
