use super::{Link, Parsed, Section};
use std::io::{self, Read, Write};

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid Markdown extent")
}
fn get(r: &mut impl Read) -> io::Result<usize> {
    let mut bytes = [0; 8];
    r.read_exact(&mut bytes)?;
    usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| invalid())
}
fn put(w: &mut impl Write, n: usize) -> io::Result<()> {
    w.write_all(&(n as u64).to_le_bytes())
}

fn string(r: &mut impl Read) -> io::Result<String> {
    let n = get(r)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while out.len() < n {
        let count = (n - out.len()).min(buf.len());
        r.read_exact(&mut buf[..count])?;
        out.try_reserve(count).map_err(|_| invalid())?;
        out.extend_from_slice(&buf[..count]);
    }
    String::from_utf8(out).map_err(|_| invalid())
}
fn text(w: &mut impl Write, s: &str) -> io::Result<()> {
    put(w, s.len())?;
    w.write_all(s.as_bytes())
}

fn write_section(w: &mut impl Write, s: &Section) -> io::Result<()> {
    text(w, &s.anchor)?;
    text(w, &s.title)?;
    for n in [
        s.level,
        s.parent.map_or(0, |id| id + 1),
        s.start,
        s.end,
        s.line,
    ] {
        put(w, n)?;
    }
    Ok(())
}

fn write_link(w: &mut impl Write, l: &Link) -> io::Result<()> {
    text(w, &l.target)?;
    text(w, &l.anchor)?;
    for n in [l.offset, l.line, l.section] {
        put(w, n)?;
    }
    Ok(())
}

impl Parsed {
    pub fn write_to(&self, mut w: impl Write) -> io::Result<()> {
        w.write_all(b"MDOC0002")?;
        put(&mut w, self.sections.len())?;
        for s in &self.sections {
            write_section(&mut w, s)?;
        }
        text(&mut w, &self.path)?;
        put(&mut w, self.destinations.len())?;
        for l in &self.destinations {
            write_link(&mut w, l)?;
        }
        Ok(())
    }

    pub fn read_from(mut r: impl Read) -> io::Result<Self> {
        let (sections, path) = read_headings(&mut r)?;
        let links = link_records(&mut r, &sections)?.collect::<io::Result<Vec<_>>>()?;
        let mut parsed = Self {
            sections,
            destinations: links,
            links: Vec::new(),
            path,
        };
        parsed.resolve_links();
        Ok(parsed)
    }
}

fn read_link(r: &mut impl Read, sections: &[Section], previous: Option<usize>) -> io::Result<Link> {
    let link = Link {
        target: string(r)?,
        anchor: string(r)?,
        offset: get(r)?,
        line: get(r)?,
        section: get(r)?,
    };
    if !sections.get(link.section).is_some_and(|s| {
        s.start <= link.offset && link.offset < s.end && s.line <= link.line
    }) || previous.is_some_and(|offset| offset >= link.offset)
    {
        return Err(invalid());
    }
    Ok(link)
}

fn link_records<'a>(
    r: &'a mut impl Read,
    sections: &'a [Section],
) -> io::Result<impl Iterator<Item = io::Result<Link>> + 'a> {
    let count = get(r)?;
    let mut previous = None;
    Ok((0..count).map(move |_| {
        let link = read_link(r, sections, previous)?;
        previous = Some(link.offset);
        Ok(link)
    }))
}

/// Reverse postings refer to qualified links. Raw links that escape the
/// repository still occupy wire records but do not consume a posting ordinal.
pub(crate) fn read_selected_links(
    mut r: impl Read,
    mut wanted: impl Iterator<Item = usize>,
) -> io::Result<(Vec<Section>, Vec<Link>)> {
    let (sections, path) = read_headings(&mut r)?;
    let path = std::path::Path::new(&path);
    let mut records = link_records(&mut r, &sections)?
        .filter_map(|record| match record {
            Ok(link) => super::target::qualify(path, &link.target)
                .map(|target| Ok(Link { target, ..link })),
            Err(error) => Some(Err(error)),
        })
        .enumerate();
    let mut hits = Vec::new();
    for wanted_ordinal in &mut wanted {
        loop {
            let (ordinal, record) = records.next().ok_or_else(invalid)?;
            let link = record?;
            if ordinal == wanted_ordinal {
                hits.push(link);
                break;
            }
        }
    }
    drop(records);
    Ok((sections, hits))
}

/// The wire prefix shared by full-document and heading-only queries.
pub(crate) fn read_headings(mut r: impl Read) -> io::Result<(Vec<Section>, String)> {
    let mut magic = [0; 8];
    r.read_exact(&mut magic)?;
    if &magic != b"MDOC0002" {
        return Err(invalid());
    }
    let count = get(&mut r)?;
    if count == 0 {
        return Err(invalid());
    }
    let mut sections = Vec::<Section>::new();
    for _ in 0..count {
        let section = Section::read(&mut r)?;
        if !section.valid_after(&sections) {
            return Err(invalid());
        }
        sections.push(section);
    }
    let path = string(&mut r)?;
    let filename = path.rsplit('/').next().unwrap_or("");
    if sections[0].title != filename {
        filename.clone_into(&mut sections[0].title);
    }
    Ok((sections, path))
}

impl Section {
    fn read(r: &mut impl Read) -> io::Result<Self> {
        Ok(Section {
            anchor: string(r)?,
            title: string(r)?,
            level: get(r)?,
            parent: get(r)?.checked_sub(1),
            start: get(r)?,
            end: get(r)?,
            line: get(r)?,
        })
    }

    fn valid_after(&self, previous: &[Section]) -> bool {
        if self.end < self.start {
            return false;
        }
        let Some(last) = previous.last() else {
            return self.is_root();
        };
        let Some(parent) = self.parent.and_then(|id| previous.get(id)) else {
            return false;
        };
        self.start >= last.start && self.line >= last.line && self.within(parent)
    }

    fn is_root(&self) -> bool {
        self.parent.is_none() && self.level == 0 && self.start == 0
            && self.line == 1 && self.anchor.is_empty()
    }

    fn within(&self, parent: &Self) -> bool {
        !self.anchor.is_empty() && self.level <= 6 && parent.level < self.level
            && self.start >= parent.start && self.end <= parent.end
    }
}
