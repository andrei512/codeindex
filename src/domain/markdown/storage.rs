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

impl Parsed {
    pub fn write_to(&self, mut w: impl Write) -> io::Result<()> {
        w.write_all(b"MDOC0002")?;
        put(&mut w, self.sections.len())?;
        for s in &self.sections {
            text(&mut w, &s.anchor)?;
            text(&mut w, &s.title)?;
            for n in [
                s.level,
                s.parent.map_or(0, |id| id + 1),
                s.start,
                s.end,
                s.line,
            ] {
                put(&mut w, n)?;
            }
        }
        text(&mut w, &self.path)?;
        put(&mut w, self.destinations.len())?;
        for l in &self.destinations {
            text(&mut w, &l.target)?;
            text(&mut w, &l.anchor)?;
            for n in [l.offset, l.line, l.section] {
                put(&mut w, n)?;
            }
        }
        Ok(())
    }

    pub fn read_from(mut r: impl Read) -> io::Result<Self> {
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
        for id in 0..count {
            let section = Section {
                anchor: string(&mut r)?,
                title: string(&mut r)?,
                level: get(&mut r)?,
                parent: get(&mut r)?.checked_sub(1),
                start: get(&mut r)?,
                end: get(&mut r)?,
                line: get(&mut r)?,
            };
            let valid = if let Some(parent) = section.parent.and_then(|n| sections.get(n)) {
                !section.anchor.is_empty()
                    && section.level <= 6
                    && parent.level < section.level
                    && section.start >= parent.start
                    && section.end <= parent.end
                    && sections.last().is_some_and(|prev| {
                        section.start >= prev.start && section.line >= prev.line
                    })
            } else {
                id == 0
                    && section.parent.is_none()
                    && section.level == 0
                    && section.start == 0
                    && section.line == 1
                    && section.anchor.is_empty()
            };
            if !valid || section.end < section.start {
                return Err(invalid());
            }
            sections.push(section);
        }
        let path = string(&mut r)?;
        let mut links = Vec::<Link>::new();
        for _ in 0..get(&mut r)? {
            let link = Link {
                target: string(&mut r)?,
                anchor: string(&mut r)?,
                offset: get(&mut r)?,
                line: get(&mut r)?,
                section: get(&mut r)?,
            };
            if !sections.get(link.section).is_some_and(|s| {
                s.start <= link.offset && link.offset < s.end && s.line <= link.line
            }) || links.last().is_some_and(|old| old.offset >= link.offset)
            {
                return Err(invalid());
            }
            links.push(link);
        }
        let mut parsed = Self {
            sections,
            destinations: links,
            links: Vec::new(),
            path: String::new(),
        };
        parsed.relocate(&path);
        Ok(parsed)
    }
}
