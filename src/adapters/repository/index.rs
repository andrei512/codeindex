use super::{invalid, optional_bytes};
use std::{collections::BTreeSet, io, path::Path};

#[derive(Default)]
pub(super) struct Tracked {
    pub paths: BTreeSet<String>,
    pub gitlinks: BTreeSet<String>,
    pub sparse: Vec<(String, Vec<u8>)>,
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
fn decode(bytes: &[u8], hash_len: usize) -> io::Result<(Vec<Entry>, Option<&[u8]>)> {
    let mut entries = Vec::new();
    let mut link = None;
    let payload_len = bytes
        .len()
        .checked_sub(hash_len)
        .ok_or_else(|| invalid("short index"))?;
    let mut data = &bytes[..payload_len];
    if take(&mut data, 4)? != b"DIRC" {
        return Err(invalid("invalid Git index signature"));
    }
    let version = number(take(&mut data, 4)?);
    if !(2..=4).contains(&version) {
        return Err(invalid("unsupported Git index version"));
    }
    let count = number(take(&mut data, 4)?);
    let mut name = Vec::new();
    for _ in 0..count {
        let start = data.len();
        let stat = take(&mut data, 40 + hash_len)?;
        let mode = number(&stat[24..28]);
        let flags = number(take(&mut data, 2)?);
        if flags & 0x4000 != 0 {
            take(&mut data, 2)?;
        }
        if version == 4 {
            let mut cut = 0usize;
            loop {
                let n = take(&mut data, 1)?[0];
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
            name.truncate(
                name.len()
                    .checked_sub(cut)
                    .ok_or_else(|| invalid("bad Git index prefix"))?,
            );
        } else {
            name.clear()
        }
        let len = data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| invalid("unterminated Git index name"))?;
        name.extend_from_slice(take(&mut data, len)?);
        take(&mut data, 1)?;
        if version != 4 {
            let used = start - data.len();
            take(&mut data, (8 - used % 8) % 8)?;
        }
        entries.push(Entry {
            name: name.clone(),
            mode,
            tree: (mode == 0o040000).then(|| stat[40..].to_owned()),
        });
    }
    while !data.is_empty() {
        let tag = take(&mut data, 4)?;
        let n = number(take(&mut data, 4)?);
        let payload = take(&mut data, n)?;
        match tag {
            b"link" => link = Some(payload),
            b"sdir" => {}
            _ if tag[0].is_ascii_uppercase() => {}
            _ => return Err(invalid("unsupported required Git index extension")),
        }
    }
    Ok((entries, link))
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
    let mut set = vec![];
    while !words.is_empty() {
        let control = u64::from_be_bytes(take(&mut words, 8)?.try_into().unwrap());
        let run = ((control >> 1) & 0xffff_ffff) as usize * 64;
        if offset + run > bits.next_multiple_of(64) {
            return Err(invalid("split bitmap run out of range"));
        }
        if control & 1 != 0 {
            set.extend(offset..(offset + run).min(bits))
        }
        offset += run;
        for _ in 0..control >> 33 {
            let mut word = u64::from_be_bytes(take(&mut words, 8)?.try_into().unwrap());
            while word != 0 {
                let i = offset + word.trailing_zeros() as usize;
                if i >= bits {
                    return Err(invalid("split bitmap bit out of range"));
                }
                set.push(i);
                word &= word - 1;
            }
            offset += 64;
        }
    }
    Ok(set)
}

pub(super) fn read(path: &Path, hash_len: usize) -> io::Result<Tracked> {
    let Some(bytes) = optional_bytes(path)? else {
        return Ok(Tracked::default());
    };
    let (mut entries, link) = decode(&bytes, hash_len)?;
    if let Some(mut link) = link {
        let id = take(&mut link, hash_len)?;
        if id.iter().any(|&n| n != 0) {
            let name = format!("sharedindex.{}", super::hex(id));
            let shared = std::fs::read(path.with_file_name(name))?;
            if !shared.ends_with(id) {
                return Err(invalid("split index shared ID mismatch"));
            }
            let (mut base, nested) = decode(&shared, hash_len)?;
            if nested.is_some() {
                return Err(invalid("recursive split index"));
            }
            let mut replacements = entries.into_iter();
            let mut deleted = vec![];
            if !link.is_empty() {
                deleted = bitmap(&mut link, base.len())?;
                for slot in bitmap(&mut link, base.len())? {
                    let replacement = replacements
                        .next()
                        .ok_or_else(|| invalid("missing split replacement"))?;
                    if !replacement.name.is_empty() || deleted.binary_search(&slot).is_ok() {
                        return Err(invalid("invalid split replacement"));
                    }
                    base[slot].mode = replacement.mode;
                    base[slot].tree = replacement.tree;
                }
            }
            if !link.is_empty() {
                return Err(invalid("trailing split extension bytes"));
            }
            entries = base
                .into_iter()
                .enumerate()
                .filter_map(|(i, e)| deleted.binary_search(&i).is_err().then_some(e))
                .chain(replacements)
                .collect();
        }
    }
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
