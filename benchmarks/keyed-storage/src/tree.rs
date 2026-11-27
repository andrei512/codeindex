//! Immutable, fixed-size pages. An internal entry is a lower fence and a child page ID.
//! The offset table supports binary search without decoding earlier keys or values.
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, VecDeque},
    fs::File,
    io::{self, Write},
    os::unix::fs::FileExt,
    rc::Rc,
};
pub type Rows = BTreeMap<Vec<u8>, Vec<u8>>;
pub type Edits = BTreeMap<Vec<u8>, Option<Vec<u8>>>;
pub type Entry = (Vec<u8>, Vec<u8>);
const SIZE: usize = 8192;
const HEADER: usize = 16;
const CACHE: usize = 256;
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid keyed storage page")
}
fn checksum(bytes: &[u8]) -> u64 {
    bytes.chunks_exact(8).fold(0xcbf29ce484222325, |h, w| {
        (h ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(0x100000001b3)
    })
}
struct Page(Box<[u8; SIZE]>);
impl Page {
    fn count(&self) -> usize {
        u16::from_le_bytes(self.0[10..12].try_into().unwrap()) as usize
    }
    fn leaf(&self) -> bool {
        self.0[8] == 0
    }
    fn offset(&self, i: usize) -> usize {
        let at = HEADER + i * 2;
        u16::from_le_bytes(self.0[at..at + 2].try_into().unwrap()) as usize
    }
    fn row(&self, i: usize) -> (&[u8], &[u8]) {
        let at = self.offset(i);
        let end = self.offset(i + 1);
        let key = self.key_at(at);
        (&self.0[key.clone()], &self.0[key.end..end])
    }
    fn key_at(&self, at: usize) -> std::ops::Range<usize> {
        let start = at + 2;
        let length = usize::from(u16::from_le_bytes(self.0[at..start].try_into().unwrap()));
        start..start + length
    }
    fn validate(&self) -> io::Result<()> {
        let n = self.count();
        if self.0[8] > 1
            || n == 0
            || HEADER + 2 * (n + 1) > SIZE
            || self.offset(0) < HEADER + 2 * (n + 1)
            || u64::from_le_bytes(self.0[..8].try_into().unwrap()) != checksum(&self.0[8..])
        {
            return Err(invalid());
        }
        for i in 0..n {
            self.validate_row(i)?;
        }
        Ok(())
    }
    fn validate_row(&self, i: usize) -> io::Result<()> {
        let (a, b) = (self.offset(i), self.offset(i + 1));
        if b > SIZE || a + 2 > b {
            return Err(invalid());
        }
        let k = u16::from_le_bytes(self.0[a..a + 2].try_into().unwrap()) as usize;
        if a + 2 + k > b
            || (!self.leaf() && b - a - 2 - k != 8)
            || (i > 0 && self.row(i - 1).0 >= self.row(i).0)
        {
            return Err(invalid());
        }
        Ok(())
    }

    fn lower(&self, key: &[u8]) -> usize {
        let starts = &self.0[HEADER..HEADER + 2 * self.count()];
        starts.as_chunks::<2>().0.partition_point(|&offset| {
            let start = usize::from(u16::from_le_bytes(offset));
            &self.0[self.key_at(start)] < key
        })
    }
}
#[derive(Default)]
struct Cache {
    pages: HashMap<u64, Rc<Page>>,
    order: VecDeque<u64>,
}
pub struct Arena {
    file: File,
    end: u64,
    cache: RefCell<Cache>,
}
fn child(v: &[u8]) -> u64 {
    u64::from_le_bytes(v.try_into().unwrap())
}
fn patch_leaf(p: &Page, edits: &[BorrowedEdit<'_>]) -> Rows {
    let mut rows: Rows = (0..p.count())
        .map(|i| {
            let (k, v) = p.row(i);
            (k.to_vec(), v.to_vec())
        })
        .collect();
    for &(key, value) in edits {
        patch_leaf_row(&mut rows, key, value.as_deref());
    }
    rows
}

fn patch_leaf_row(rows: &mut Rows, key: &[u8], value: Option<&[u8]>) {
    if let Some(value) = value {
        rows.insert(key.to_vec(), value.to_vec());
    } else {
        rows.remove(key);
    }
}

fn page_prefix(rows: &[Entry]) -> usize {
    let mut size = HEADER + 2;
    rows.iter()
        .take_while(|(key, value)| {
            size += 4 + key.len() + value.len();
            size <= SIZE
        })
        .count()
}

type BorrowedEdit<'a> = (&'a Vec<u8>, &'a Option<Vec<u8>>);

fn edits_before<'a, 'rows>(
    edits: &'rows [BorrowedEdit<'a>],
    upper: Option<&[u8]>,
) -> (&'rows [BorrowedEdit<'a>], &'rows [BorrowedEdit<'a>]) {
    let count = upper.map_or(edits.len(), |upper| {
        edits.partition_point(|(key, _)| key.as_slice() < upper)
    });
    edits.split_at(count)
}

impl Arena {
    pub fn new(file: File) -> io::Result<Self> {
        let end = file.metadata()?.len();
        if end % SIZE as u64 != 0 {
            return Err(invalid());
        }
        Ok(Self {
            file,
            end,
            cache: Default::default(),
        })
    }
    pub fn len(&self) -> u64 {
        self.end
    }
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
    fn page(&self, id: u64) -> io::Result<Rc<Page>> {
        let mut cache = self.cache.borrow_mut();
        if let Some(p) = cache.pages.get(&id) {
            return Ok(Rc::clone(p));
        }
        let at = id
            .checked_mul(SIZE as u64)
            .filter(|&n| n < self.end)
            .ok_or_else(invalid)?;
        let mut p = Page(Box::new([0; SIZE]));
        self.file.read_exact_at(&mut p.0[..], at)?;
        p.validate()?;
        if !p.leaf() && (0..p.count()).any(|i| child(p.row(i).1) >= id) {
            return Err(invalid());
        }
        let p = Rc::new(p);
        if cache.order.len() == CACHE {
            let old = cache.order.pop_front().unwrap();
            cache.pages.remove(&old);
        }
        cache.order.push_back(id);
        cache.pages.insert(id, p.clone());
        Ok(p)
    }
    fn pack(&mut self, leaf: bool, rows: &[Entry]) -> io::Result<Vec<Entry>> {
        let mut result = Vec::new();
        let mut start = 0;
        while start < rows.len() {
            let end = start + page_prefix(&rows[start..]);
            if end == start {
                return Err(io::Error::other("replay record exceeds page size"));
            }
            let batch = &rows[start..end];
            let id = self.append_page(leaf, batch)?;
            result.push((batch[0].0.clone(), id.to_le_bytes().to_vec()));
            start = end;
        }
        Ok(result)
    }
    fn append_page(&mut self, leaf: bool, batch: &[Entry]) -> io::Result<u64> {
        let n = batch.len();
        let mut p = Box::new([0; SIZE]);
        p[8] = u8::from(!leaf);
        p[10..12].copy_from_slice(&(n as u16).to_le_bytes());
        let mut at = HEADER + 2 * (n + 1);
        for (i, (k, v)) in batch.iter().enumerate() {
            p[HEADER + 2 * i..HEADER + 2 * i + 2].copy_from_slice(&(at as u16).to_le_bytes());
            p[at..at + 2].copy_from_slice(&(k.len() as u16).to_le_bytes());
            at += 2;
            p[at..at + k.len()].copy_from_slice(k);
            at += k.len();
            p[at..at + v.len()].copy_from_slice(v);
            at += v.len();
        }
        p[HEADER + 2 * n..HEADER + 2 * n + 2].copy_from_slice(&(at as u16).to_le_bytes());
        let hash = checksum(&p[8..]);
        p[..8].copy_from_slice(&hash.to_le_bytes());
        let id = self.end / SIZE as u64;
        self.file.write_all(&p[..])?;
        self.end += SIZE as u64;
        Ok(id)
    }

    fn root(&mut self, mut entries: Vec<Entry>) -> io::Result<Option<u64>> {
        while entries.len() > 1 {
            let next = self.pack(false, &entries)?;
            if next.len() >= entries.len() {
                return Err(io::Error::other("fence key exceeds branching limit"));
            }
            entries = next;
        }
        Ok(entries.first().map(|e| child(&e.1)))
    }
    pub fn bulk(&mut self, rows: Rows) -> io::Result<Option<u64>> {
        let leaves = self.pack(true, &rows.into_iter().collect::<Vec<_>>())?;
        self.root(leaves)
    }
    fn patch(&mut self, id: u64, edits: &[BorrowedEdit<'_>]) -> io::Result<Vec<Entry>> {
        let p = self.page(id)?;
        if p.leaf() {
            let rows = patch_leaf(&p, edits);
            return self.pack(true, &rows.into_iter().collect::<Vec<_>>());
        }
        let mut out = Vec::new();
        let mut pending = edits;
        for i in 0..p.count() {
            let (k, v) = p.row(i);
            let upper = (i + 1 < p.count()).then(|| p.row(i + 1).0);
            let (group, rest) = edits_before(pending, upper);
            pending = rest;
            self.patch_child((k, v), group, &mut out)?;
        }
        self.pack(false, &out)
    }
    fn patch_child(&mut self, (key, value): (&[u8], &[u8]), edits: &[BorrowedEdit<'_>], out: &mut Vec<Entry>) -> io::Result<()> {
        if edits.is_empty() {
            out.push((key.to_vec(), value.to_vec()));
        } else {
            out.extend(self.patch(child(value), edits)?);
        }
        Ok(())
    }

    pub fn update(&mut self, root: Option<u64>, edits: &Edits) -> io::Result<Option<u64>> {
        if edits.is_empty() {
            return Ok(root);
        }
        if let Some(id) = root {
            let rows = self.patch(id, &edits.iter().collect::<Vec<_>>())?;
            self.root(rows)
        } else {
            self.bulk(
                edits
                    .iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.clone()?)))
                    .collect(),
            )
        }
    }
    fn visit(&self, id: u64, start: &[u8], limit: usize, out: &mut Vec<Entry>) -> io::Result<()> {
        let page = self.page(id)?;
        let lower = page.lower(start);
        if page.leaf() {
            out.extend((lower..page.count()).take(limit - out.len()).map(|index| {
                let (key, value) = page.row(index);
                (key.to_vec(), value.to_vec())
            }));
            return Ok(());
        }
        // A lower fence equal to start belongs to this child. Otherwise
        // start can lie in the preceding child's interval.
        let first = if lower < page.count() && page.row(lower).0 == start {
            lower
        } else {
            lower.saturating_sub(1)
        };
        self.visit_children(&page, (first, start), limit, out)
    }

    fn visit_children(
        &self,
        page: &Page,
        (first, start): (usize, &[u8]),
        limit: usize,
        out: &mut Vec<Entry>,
    ) -> io::Result<()> {
        let starts = std::iter::once(start).chain(std::iter::repeat(b"".as_slice()));
        for (index, from) in (first..page.count()).zip(starts) {
            if out.len() == limit {
                return Ok(());
            }
            self.visit(child(page.row(index).1), from, limit, out)?;
        }
        Ok(())
    }

    pub fn scan(&self, root: Option<u64>, start: &[u8], limit: usize) -> io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        if let Some(id) = root {
            self.visit(id, start, limit, &mut out)?;
        }
        Ok(out)
    }
}
