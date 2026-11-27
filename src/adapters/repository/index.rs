use super::{invalid, optional_bytes};
use std::{collections::BTreeSet, io, path::Path};

#[derive(Default)]
pub(super) struct Tracked {
    pub paths: BTreeSet<String>,
    pub gitlinks: BTreeSet<String>,
    pub sparse: Vec<(String, Vec<u8>)>,
}

impl Tracked {
    pub(super) fn has_descendant(&self, directory: &str) -> bool {
        use std::ops::Bound::{Included, Unbounded};
        if self.paths.is_empty() { return false; }
        let prefix = format!("{directory}/");
        self.paths.range::<str, _>((Included(prefix.as_str()), Unbounded))
            .next().is_some_and(|p| p.starts_with(&prefix))
    }
}

pub(super) fn take<'a>(data: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    let (part, rest) = data
        .split_at_checked(n)
        .ok_or_else(|| invalid("truncated Git index"))?;
    *data = rest;
    Ok(part)
}
pub(super) fn number(data: &[u8]) -> usize {
    data.iter().fold(0, |n, b| n << 8 | usize::from(*b))
}
struct Entry {
    name: Vec<u8>,
    mode: usize,
    tree: Option<Vec<u8>>,
}
fn prefix_length(data: &mut &[u8], previous_len: usize) -> io::Result<usize> {
    let mut cut = 0usize;
    loop {
        let n = take(data, 1)?[0];
        cut = cut
            .checked_mul(128)
            .and_then(|x| x.checked_add(usize::from(n & 127)))
            .ok_or_else(|| invalid("bad Git index prefix"))?;
        if n & 128 == 0 {
            break;
        }
        cut = cut
            .checked_add(1)
            .ok_or_else(|| invalid("bad Git index prefix"))?;
    }
    previous_len.checked_sub(cut).ok_or_else(|| invalid("bad Git index prefix"))
}

impl Entry {
    fn read(data: &mut &[u8], version: usize, hash_len: usize, name: &mut Vec<u8>) -> io::Result<Self> {
        let start = data.len();
        let stat = take(data, 40 + hash_len)?;
        let mode = number(&stat[24..28]);
        let flags = number(take(data, 2)?);
        if flags & 0x4000 != 0 {
            take(data, 2)?;
        }
        if version == 4 {
            name.truncate(prefix_length(data, name.len())?);
        } else {
            name.clear()
        }
        let len = data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| invalid("unterminated Git index name"))?;
        name.extend_from_slice(take(data, len)?);
        take(data, 1)?;
        if version != 4 {
            let used = start - data.len();
            take(data, (8 - used % 8) % 8)?;
        }
        Ok(Entry {
            name: if version == 4 { name.clone() } else { std::mem::take(name) },
            mode,
            tree: (mode == 0o040000).then(|| stat[40..].to_owned()),
        })
    }
}

struct Header {
    version: usize,
    count: usize,
}

impl Header {
    fn read(data: &mut &[u8]) -> io::Result<Self> {
        if take(data, 4)? != b"DIRC" {
            return Err(invalid("invalid Git index signature"));
        }
        let version = number(take(data, 4)?);
        if !(2..=4).contains(&version) {
            return Err(invalid("unsupported Git index version"));
        }
        Ok(Self {
            version,
            count: number(take(data, 4)?),
        })
    }
}

#[derive(Default)]
struct Extensions<'a> {
    link: Option<&'a [u8]>,
}

impl<'a> Extensions<'a> {
    fn read(mut data: &'a [u8]) -> io::Result<Self> {
        let mut extensions = Self::default();
        while !data.is_empty() {
            let tag = take(&mut data, 4)?;
            let n = number(take(&mut data, 4)?);
            let payload = take(&mut data, n)?;
            match tag {
                b"link" => extensions.link = Some(payload),
                b"sdir" => {}
                _ if tag[0].is_ascii_uppercase() => {}
                _ => return Err(invalid("unsupported required Git index extension")),
            }
        }
        Ok(extensions)
    }

    fn apply(self, path: &Path, hash_len: usize, entries: Vec<Entry>) -> io::Result<Vec<Entry>> {
        let Some(data) = self.link else {
            return Ok(entries);
        };
        let Some(link) = SplitLink::read(data, hash_len)? else {
            return Ok(entries);
        };
        let base = link.shared_entries(path)?;
        let mut payload = link.bitmaps;
        let entries = SplitDelta::read(&mut payload, base.len())?.apply(base, entries)?;
        if !payload.is_empty() {
            return Err(invalid("trailing split extension bytes"));
        }
        Ok(entries)
    }
}

fn decode(bytes: &[u8], hash_len: usize) -> io::Result<(Vec<Entry>, Extensions<'_>)> {
    let payload_len = bytes
        .len()
        .checked_sub(hash_len)
        .ok_or_else(|| invalid("short index"))?;
    let mut data = &bytes[..payload_len];
    let Header { version, count } = Header::read(&mut data)?;
    let mut entries = Vec::new();
    let mut name = Vec::new();
    for _ in 0..count {
        entries.push(Entry::read(&mut data, version, hash_len, &mut name)?);
    }
    Ok((entries, Extensions::read(data)?))
}

fn bitmap_literals(
    words: &mut &[u8],
    offset: usize,
    bits: usize,
    set: &mut Vec<usize>,
) -> io::Result<()> {
    let mut word = u64::from_be_bytes(take(words, 8)?.try_into().unwrap());
    while word != 0 {
        let i = offset + word.trailing_zeros() as usize;
        if i >= bits {
            return Err(invalid("split bitmap bit out of range"));
        }
        set.push(i);
        word &= word - 1;
    }
    Ok(())
}

fn bitmap(data: &mut &[u8], limit: usize) -> io::Result<Vec<usize>> {
    let bits = number(take(data, 4)?);
    if bits > limit {
        return Err(invalid("split index bitmap exceeds shared entries"));
    }
    let n = number(take(data, 4)?);
    let mut words = take(
        data,
        n.checked_mul(8).ok_or_else(|| invalid("bitmap overflow"))?,
    )?;
    let rlw = number(take(data, 4)?);
    if n == 0 || rlw >= n {
        return Err(invalid("invalid split-index bitmap header"));
    }
    let mut offset = 0usize;
    let mut set = Vec::new();
    while !words.is_empty() {
        bitmap_segment(&mut words, &mut offset, bits, &mut set)?;
    }
    Ok(set)
}

fn bitmap_segment(
    words: &mut &[u8],
    offset: &mut usize,
    bits: usize,
    set: &mut Vec<usize>,
) -> io::Result<()> {
    let control = u64::from_be_bytes(take(words, 8)?.try_into().unwrap());
    let run = ((control >> 1) & 0xffff_ffff) * 64;
    let end = *offset as u64 + run;
    if end > (bits as u64).next_multiple_of(64) {
        return Err(invalid("split bitmap run out of range"));
    }
    if control & 1 != 0 {
        set.extend(*offset..(end as usize).min(bits));
    }
    *offset = end as usize;
    for _ in 0..control >> 33 {
        bitmap_literals(words, *offset, bits, set)?;
        *offset += 64;
    }
    Ok(())
}

struct SplitLink<'a> {
    id: &'a [u8],
    bitmaps: &'a [u8],
}

impl<'a> SplitLink<'a> {
    fn read(mut data: &'a [u8], hash_len: usize) -> io::Result<Option<Self>> {
        let id = take(&mut data, hash_len)?;
        Ok(id
            .iter()
            .any(|&byte| byte != 0)
            .then_some(Self { id, bitmaps: data }))
    }

    fn shared_entries(&self, path: &Path) -> io::Result<Vec<Entry>> {
        let name = format!("sharedindex.{}", super::hex(self.id));
        let shared = std::fs::read(path.with_file_name(name))?;
        if !shared.ends_with(self.id) {
            return Err(invalid("split index shared ID mismatch"));
        }
        let (base, extensions) = decode(&shared, self.id.len())?;
        if extensions.link.is_some() {
            return Err(invalid("recursive split index"));
        }
        Ok(base)
    }
}

#[derive(Default)]
struct SplitDelta {
    deleted: Vec<usize>,
    replaced: Vec<usize>,
}

impl SplitDelta {
    fn read(data: &mut &[u8], limit: usize) -> io::Result<Self> {
        if data.is_empty() {
            return Ok(Self::default());
        }
        let deleted = bitmap(data, limit)?;
        let replaced = bitmap(data, limit)?;
        Ok(Self { deleted, replaced })
    }

    fn apply(self, mut base: Vec<Entry>, entries: Vec<Entry>) -> io::Result<Vec<Entry>> {
        let mut overlay = entries.into_iter();
        for slot in self.replaced {
            let replacement = overlay
                .next()
                .ok_or_else(|| invalid("missing split replacement"))?;
            if !replacement.name.is_empty() || self.deleted.binary_search(&slot).is_ok() {
                return Err(invalid("invalid split replacement"));
            }
            base[slot].mode = replacement.mode;
            base[slot].tree = replacement.tree;
        }
        Ok(base
            .into_iter()
            .enumerate()
            .filter_map(|(i, e)| self.deleted.binary_search(&i).is_err().then_some(e))
            .chain(overlay)
            .collect())
    }
}

pub(super) fn read(path: &Path, hash_len: usize) -> io::Result<Tracked> {
    let Some(bytes) = optional_bytes(path)? else {
        return Ok(Tracked::default());
    };
    let (entries, extensions) = decode(&bytes, hash_len)?;
    let entries = extensions.apply(path, hash_len, entries)?;
    let mut tracked = Tracked::default();
    for entry in entries {
        if let Ok(s) = String::from_utf8(entry.name) {
            if s.is_empty() {
                return Err(invalid("empty tracked path"));
            }
            match entry.mode {
                0o160000 => {
                    tracked.gitlinks.insert(s);
                }
                0o040000 => {
                    tracked.sparse.push((s, entry.tree.unwrap()));
                }
                _ => {
                    tracked.paths.insert(s);
                }
            }
        }
    }
    Ok(tracked)
}
