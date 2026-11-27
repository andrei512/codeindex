use std::ffi::{CStr, CString, c_char, c_int};

unsafe extern "C" {
    fn fnmatch(pattern: *const c_char, name: *const c_char, flags: c_int) -> c_int;
}

enum Segment {
    Recursive,
    Literal(CString),
    Pattern(CString),
}

impl Segment {
    fn component(part: &[u8], fold: bool) -> Option<Self> {
        let literal =
            part.is_ascii() && !part.iter().any(|c| matches!(c, b'?' | b'*' | b'[' | b'\\'));
        let bytes = if fold {
            part.to_ascii_lowercase()
        } else {
            part.to_vec()
        };
        let pattern = CString::new(bytes).ok()?;
        Some(if literal {
            Self::Literal(pattern)
        } else {
            Self::Pattern(pattern)
        })
    }

    fn matches(&self, name: &CStr) -> bool {
        match self {
            Self::Literal(text) => text.as_bytes() == name.to_bytes(),
            Self::Pattern(pattern) => unsafe { fnmatch(pattern.as_ptr(), name.as_ptr(), 0) == 0 },
            Self::Recursive => false,
        }
    }
}

enum Matcher {
    Basename(Segment),
    Path(Vec<Segment>),
}

fn recursive_component(part: &[u8]) -> bool {
    part.len() >= 2 && part.iter().all(|&b| b == b'*')
}

impl Matcher {
    fn path(bytes: &[u8], fold: bool) -> Option<Self> {
        let mut parts = bytes.split(|&b| b == b'/').enumerate().peekable();
        let mut segments = Vec::new();
        while let Some((i, part)) = parts.next() {
            let last = parts.peek().is_none();
            let allow_recursive = i != 0 || !last;
            if allow_recursive && recursive_component(part) {
                segments.push(Segment::Recursive);
                // A trailing /** also needs one component below the directory.
                if !last {
                    continue;
                }
            }
            segments.push(Segment::component(part, fold)?);
        }
        Some(Self::Path(segments))
    }
}

pub(super) struct Rule {
    base: usize,
    negated: bool,
    directory: bool,
    matcher: Matcher,
}

impl Rule {
    pub(super) fn parse(bytes: &[u8], base: usize, fold: bool) -> Option<Self> {
        let bytes = trim_rule(bytes);
        if bytes.is_empty() || bytes.starts_with(b"#") {
            return None;
        }
        let negated = bytes.starts_with(b"!");
        let bytes = bytes.strip_prefix(b"!").unwrap_or(bytes);
        let trailing_slash = bytes.strip_suffix(b"/");
        let directory = trailing_slash.is_some();
        let bytes = trailing_slash
            .map(|part| part.strip_suffix(b"\\").unwrap_or(part))
            .unwrap_or(bytes);
        let basename = !bytes.contains(&b'/');
        let bytes = bytes.strip_prefix(b"/").unwrap_or(bytes);
        if bytes.is_empty() {
            return None;
        }
        let matcher = if basename {
            Matcher::Basename(Segment::component(bytes, fold)?)
        } else {
            Matcher::path(bytes, fold)?
        };
        Some(Self {
            base,
            negated,
            directory,
            matcher,
        })
    }

    pub(super) fn decision(&self, names: &[CString], is_directory: bool) -> Option<bool> {
        let (name, parents) = names.split_last()?;
        self.decision_at(parents, name, is_directory)
    }

    pub(super) fn decision_at(
        &self,
        parents: &[CString],
        name: &CStr,
        is_directory: bool,
    ) -> Option<bool> {
        if self.directory && !is_directory {
            return None;
        }
        let segments = match &self.matcher {
            Matcher::Basename(segment) => return segment.matches(name).then_some(!self.negated),
            Matcher::Path(segments) => segments,
        };
        let parents = parents.get(self.base..)?;
        Self::matches_path(segments, parents, name).then_some(!self.negated)
    }

    fn matches_path(segments: &[Segment], parents: &[CString], name: &CStr) -> bool {
        let count = parents.len() + 1;
        let (mut i, mut j) = (0, 0);
        let mut star = None;
        while j < count {
            match segments.get(i) {
                Some(Segment::Recursive) => {
                    i += 1;
                    star = Some((i, j));
                }
                Some(segment)
                    if segment.matches(parents.get(j).map_or(name, CString::as_c_str)) =>
                {
                    i += 1;
                    j += 1;
                }
                _ => {
                    let Some((next, used)) = star else { return false };
                    j = used + 1;
                    i = next;
                    star = Some((next, j));
                }
            }
        }
        i == segments.len()
    }
}

fn trim_rule(bytes: &[u8]) -> &[u8] {
    let mut bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    while let Some(prefix) = bytes.strip_suffix(b" ") {
        let escapes = prefix.iter().rev().take_while(|&&b| b == b'\\').count();
        if escapes % 2 == 1 {
            break;
        }
        bytes = prefix;
    }
    bytes
}
