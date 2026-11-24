//! Keep unchanged document records and reverse-address lists in the predecessor.
use super::{
    Entry, Local, Previous,
    retained::{self, Run},
};
use crate::{
    application::index::{
        binary::{Decoder, bad, put, string},
        layout::{ExtentSink, MD},
    },
    domain::markdown::{self, Parsed},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    ops::Range,
};

type Key = (String, String);
type Site = (usize, usize);
#[derive(Default)]
struct Changes {
    remove: Vec<Site>,
    add: Vec<Site>,
}
fn read(p: &dyn Previous, r: Range<u64>) -> io::Result<Vec<u8>> {
    let mut b = Vec::new();
    p.copy_bytes(r, &mut b)?;
    Ok(b)
}
fn table(d: &mut Decoder, limit: u64) -> io::Result<Vec<u64>> {
    let n = d.num()?;
    if n == 0 {
        return Err(bad());
    }
    let raw = d.take(n.checked_mul(8).ok_or_else(bad)?)?;
    let values = raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| u64::from_le_bytes(*b))
        .collect::<Vec<_>>();
    if !values.is_sorted() || values[n - 1] > limit {
        return Err(bad());
    }
    Ok(values)
}
fn address(p: &dyn Previous, at: u64, end: u64) -> io::Result<Key> {
    let mut at = at;
    let mut field = || -> io::Result<String> {
        let n = u64::from_le_bytes(
            read(p, at..at + 8)?
                .as_slice()
                .try_into()
                .map_err(|_| bad())?,
        );
        at += 8;
        let stop = at.checked_add(n).filter(|&s| s <= end).ok_or_else(bad)?;
        let text = String::from_utf8(read(p, at..stop)?).map_err(io::Error::other)?;
        at = stop;
        Ok(text)
    };
    Ok((field()?, field()?))
}
fn keys(m: Option<&Parsed>) -> impl Iterator<Item = (&str, &str)> {
    m.into_iter()
        .flat_map(|m| m.links.iter())
        .map(|l| (l.target.as_str(), l.anchor.as_str()))
}
fn update_links(
    id: usize,
    old: Option<&Parsed>,
    new: Option<&Parsed>,
    changes: &mut BTreeMap<Key, Changes>,
) {
    let mut old = keys(old);
    let mut new = keys(new);
    for ordinal in 0usize.. {
        let a = old.next();
        let b = new.next();
        if a.is_none() && b.is_none() {
            break;
        }
        if a == b {
            continue;
        }
        if let Some((path, anchor)) = a {
            changes
                .entry((path.into(), anchor.into()))
                .or_default()
                .remove
                .push((id, ordinal))
        }
        if let Some((path, anchor)) = b {
            changes
                .entry((path.into(), anchor.into()))
                .or_default()
                .add
                .push((id, ordinal))
        }
    }
}
struct RecordEdit {
    old: Vec<u8>,
    previous: Option<Parsed>,
    next: Vec<u8>,
}

impl RecordEdit {
    fn load(p: &dyn Previous, id: usize, old: Option<&Range<u64>>, next: Option<&Parsed>) -> io::Result<Self> {
        let mut before = Vec::new();
        let previous = if let Some(range) = old {
            p.copy_bytes(range.clone(), &mut before)?;
            let mut d = Decoder(&before);
            if d.num()? != id { return Err(bad()); }
            let mut data = d.0;
            let parsed = Parsed::read_from(&mut data)?;
            if !data.is_empty() { return Err(bad()); }
            Some(parsed)
        } else { None };
        let mut after = Vec::new();
        if let Some(parsed) = next {
            put(&mut after, id)?;
            parsed.write_to(&mut after)?;
        }
        Ok(Self { old: before, previous, next: after })
    }
}
pub(super) fn write(
    w: &mut impl ExtentSink,
    p: &dyn Previous,
    files: &[Entry],
    locals: &[Local],
    reuse: &[Option<usize>],
) -> io::Result<()> {
    let md = p.extent(MD);
    let base = md.start;
    let relevant = |f: &Entry| f.bytes.is_some() && markdown::is_markdown(f.path());
    let mut edited = files.iter().zip(p.entries()).zip(reuse).enumerate()
        .filter(|(_, ((new, old), prior))| prior.is_none() && (relevant(new) || relevant(old)));
    let Some((id, _)) = edited.next() else {
        return retained::range(p, w, md);
    };
    // Equal-size records keep every directory offset. Equal destination sequences
    // keep every (file, link ordinal) reverse posting, even when link spans move.
    let mut prepared = None;
    if edited.next().is_none()
        && let Some(new) = locals[id].markdown.as_ref()
        && let Some(record) = p.markdown_record(id)?
    {
        let edit = RecordEdit::load(p, id, Some(&record), Some(new))?;
        if edit.old == edit.next {
            return retained::range(p, w, md);
        }
        if edit.old.len() == edit.next.len() && keys(edit.previous.as_ref()).eq(keys(Some(new))) {
            let mut run = Run::new(Some(p), w);
            run.borrow(base..record.start)?;
            run.replacement(record.start, &edit.old, &edit.next)?;
            run.borrow(record.end..md.end)?;
            return run.flush();
        }
        prepared = Some((id, edit));
    }
    let trailer = md
        .end
        .checked_sub(8)
        .filter(|&at| at >= base)
        .ok_or_else(bad)?;
    let tables = u64::from_le_bytes(
        read(p, trailer..md.end)?
            .as_slice()
            .try_into()
            .map_err(|_| bad())?,
    );
    if tables > trailer - base {
        return Err(bad());
    }
    let old_footer = read(p, base + tables..md.end)?;
    let mut directory = Decoder(&old_footer[..old_footer.len() - 8]);
    let docs = table(&mut directory, tables)?;
    let reverse = table(&mut directory, tables)?;
    directory.end()?;
    if docs[0] != 8
        || reverse[0] != docs[docs.len() - 1] + 8
        || reverse[reverse.len() - 1] != tables
        || docs.len() != p.entries().iter().filter(|f| relevant(f)).count() + 1
    {
        return Err(bad());
    }
    let mut run = Run::new(Some(p), w);
    let start = run.offset();
    let mut next_docs = Vec::new();
    let mut next_destinations = Vec::new();
    let n = files.iter().filter(|f| relevant(f)).count();
    run.bytes(&(n as u64).to_le_bytes())?;
    let mut old_slot = 0;
    let mut changes = BTreeMap::new();
    for (id, ((file, old), local)) in files.iter().zip(p.entries()).zip(locals).enumerate() {
        let old_range = relevant(old).then(|| {
            let r = base + docs[old_slot]..base + docs[old_slot + 1];
            old_slot += 1;
            r
        });
        if relevant(file) {
            next_docs.push(run.offset() - start)
        }
        if reuse[id].is_some() {
            if let Some(r) = old_range {
                run.borrow(r)?
            }
            continue;
        }
        let new = local.markdown.as_ref();
        let edit = if let Some((_, edit)) = prepared.take_if(|(file, _)| *file == id) {
            edit
        } else {
            RecordEdit::load(p, id, old_range.as_ref(), new)?
        };
        update_links(id, edit.previous.as_ref(), new, &mut changes);
        if new.is_some() {
            if let Some(r) = old_range {
                run.replacement(r.start, &edit.old, &edit.next)?
            } else {
                run.bytes(&edit.next)?
            }
        }
    }
    next_docs.push(run.offset() - start);
    if changes.is_empty() {
        let old_at = *docs.last().unwrap();
        let new_at = run.offset() - start;
        run.borrow(base + old_at..base + tables)?;
        next_destinations.extend(reverse.iter().map(|s| new_at + (s - old_at)));
    } else {
        let count = reverse.len() - 1;
        let mut cache = BTreeMap::<usize, Key>::new();
        let mut updates = Vec::new();
        let mut lower = 0usize;
        for (key, changes) in changes {
            let mut lo = lower;
            let mut hi = count;
            while lo < hi {
                let i = lo + (hi - lo) / 2;
                if let std::collections::btree_map::Entry::Vacant(e) = cache.entry(i) {
                    e.insert(address(p, base + reverse[i], base + reverse[i + 1])?);
                }
                if cache[&i] < key { lo = i + 1 } else { hi = i }
            }
            lower = lo;
            let exists = cache.get(&lo) == Some(&key);
            let mut sites = BTreeSet::new();
            let bytes = if exists {
                read(p, base + reverse[lo]..base + reverse[lo + 1])?
            } else {
                vec![]
            };
            if exists {
                let mut d = Decoder(&bytes);
                d.string()?;
                d.string()?;
                for _ in 0..d.num()? {
                    if !sites.insert((d.num()?, d.num()?)) {
                        return Err(bad());
                    }
                }
                d.end()?;
            }
            for site in changes.remove {
                if !sites.remove(&site) {
                    return Err(bad());
                }
            }
            for site in changes.add {
                if !sites.insert(site) {
                    return Err(bad());
                }
            }
            let mut out = Vec::new();
            if !sites.is_empty() {
                string(&mut out, &key.0)?;
                string(&mut out, &key.1)?;
                put(&mut out, sites.len())?;
                for (f, l) in sites {
                    put(&mut out, f)?;
                    put(&mut out, l)?
                }
            }
            updates.push((lo, exists, bytes, out));
        }
        let new_count = count
            + updates
                .iter()
                .filter(|(_, exists, _, new)| !*exists && !new.is_empty())
                .count()
            - updates
                .iter()
                .filter(|(_, exists, _, new)| *exists && new.is_empty())
                .count();
        run.bytes(&(new_count as u64).to_le_bytes())?;
        let mut next = 0usize;
        for (slot, exists, old, new) in updates {
            while next < slot {
                next_destinations.push(run.offset() - start);
                run.borrow(base + reverse[next]..base + reverse[next + 1])?;
                next += 1;
            }
            if !new.is_empty() {
                next_destinations.push(run.offset() - start);
                run.replacement(base + reverse[slot], &old, &new)?
            }
            if exists {
                next += 1
            }
        }
        while next < count {
            next_destinations.push(run.offset() - start);
            run.borrow(base + reverse[next]..base + reverse[next + 1])?;
            next += 1;
        }
        next_destinations.push(run.offset() - start);
    }
    let tables_at = run.offset() - start;
    let mut out = Vec::new();
    for rows in [next_docs, next_destinations] {
        put(&mut out, rows.len())?;
        for offset in rows {
            out.extend_from_slice(&offset.to_le_bytes())
        }
    }
    out.extend_from_slice(&tables_at.to_le_bytes());
    run.replacement(base + tables, &old_footer, &out)?;
    run.flush()
}
