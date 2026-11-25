use std::ffi::{CString, c_char, c_int};

unsafe extern "C" {
    fn fnmatch(pattern: *const c_char, name: *const c_char, flags: c_int) -> c_int;
}

enum Segment {
    Recursive,
    Pattern(CString),
}

pub(super) struct Rule {
    base: usize,
    negated: bool,
    directory: bool,
    basename: bool,
    segments: Vec<Segment>,
}

impl Rule {
    pub(super) fn parse(bytes: &[u8], base: usize, fold: bool) -> Option<Self> {
        let mut bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        while bytes.last() == Some(&b' ') {
            let escapes = bytes[..bytes.len() - 1]
                .iter()
                .rev()
                .take_while(|&&b| b == b'\\')
                .count();
            if escapes % 2 == 1 {
                break;
            }
            bytes = &bytes[..bytes.len() - 1];
        }
        if bytes.is_empty() || bytes.starts_with(b"#") {
            return None;
        }
        let negated = bytes.starts_with(b"!");
        if negated {
            bytes = &bytes[1..]
        }
        let directory = bytes.ends_with(b"/");
        if directory {
            bytes = bytes[..bytes.len()-1].strip_suffix(b"\\").unwrap_or(&bytes[..bytes.len()-1]);
        }
        let basename = !bytes.contains(&b'/');
        bytes = bytes.strip_prefix(b"/").unwrap_or(bytes);
        if bytes.is_empty() {
            return None;
        }
        let parts: Vec<_> = bytes.split(|&b| b == b'/').collect();
        let mut segments = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            // A trailing /** requires at least one entry below the directory.
            if parts.len() > 1
                && part.len() >= 2
                && part.iter().all(|&b| b == b'*')
                && i + 1 < parts.len()
            {
                segments.push(Segment::Recursive);
            } else {
                if parts.len() > 1
                    && i + 1 == parts.len()
                    && part.len() >= 2
                    && part.iter().all(|&b| b == b'*')
                {
                    segments.push(Segment::Recursive);
                }
                let pattern = if fold {
                    part.to_ascii_lowercase()
                } else {
                    part.to_vec()
                };
                segments.push(Segment::Pattern(CString::new(pattern).ok()?));
            }
        }
        Some(Self {
            base,
            negated,
            directory,
            basename,
            segments,
        })
    }

    pub(super) fn decision(&self, names: &[CString], is_directory: bool) -> Option<bool> {
        if self.directory && !is_directory {
            return None;
        }
        let names = if self.basename {
            names.last().map(std::slice::from_ref)?
        } else {
            &names[self.base..]
        };
        let (mut i, mut j) = (0, 0);
        let mut star = None;
        while j < names.len() {
            match self.segments.get(i) {
                Some(Segment::Recursive) => {
                    i += 1;
                    star = Some((i, j));
                }
                Some(Segment::Pattern(p))
                    if unsafe { fnmatch(p.as_ptr(), names[j].as_ptr(), 0) } == 0 =>
                {
                    i += 1;
                    j += 1;
                }
                _ => {
                    if let Some((next, used)) = star {
                        j = used + 1;
                        i = next;
                        star = Some((next, j));
                    } else {
                        return None;
                    }
                }
            }
        }
        (i == self.segments.len()).then_some(!self.negated)
    }
}
