//! Read the two package-owner fields with JSON.parse's last-key-wins rule.
//! Unrelated values are validated and skipped without building an object tree.
pub(super) enum Manifest<'a> {
    Named(&'a str, &'a str),
    Unnamed,
    Unknown,
}

struct Cursor<'a>(&'a str);

impl<'a> Cursor<'a> {
    fn byte(&mut self) -> Option<u8> {
        self.0 = self.0.trim_start_matches([' ', '\n', '\r', '\t']);
        self.0.bytes().next()
    }

    fn take(&mut self, b: u8) -> Option<()> {
        if self.byte()? != b {
            return None;
        }
        self.0 = &self.0[1..];
        Some(())
    }

    fn string(&mut self) -> Option<&'a str> {
        self.take(b'"')?;
        let s = self.0;
        let mut bytes = s.bytes().enumerate();
        while let Some((i, b)) = bytes.next() {
            match b {
                b'"' => {
                    self.0 = &s[i + 1..];
                    return Some(&s[..i]);
                }
                b'\\' => skip_escape(&mut bytes)?,
                0..=31 => return None,
                _ => {}
            }
        }
        None
    }

    fn number(&mut self) -> Option<()> {
        let bytes = self.0.as_bytes();
        let mut i = usize::from(bytes.first() == Some(&b'-'));
        if *bytes.get(i)? == b'0' {
            i += 1
        } else {
            digits(bytes, &mut i)?
        }
        if bytes.get(i) == Some(&b'.') {
            i += 1;
            digits(bytes, &mut i)?
        }
        if matches!(bytes.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(bytes.get(i), Some(b'-' | b'+')) {
                i += 1;
            }
            digits(bytes, &mut i)?;
        }
        self.0 = &self.0[i..];
        Some(())
    }

    // Both the root object and nested containers require separators between
    // entries. Consuming a comma commits the caller to parsing one more entry.
    fn entries(&mut self, close: u8, mut entry: impl FnMut(&mut Self) -> Option<()>) -> Option<()> {
        if self.byte()? != close {
            loop {
                entry(self)?;
                if self.byte()? == close {
                    break;
                }
                self.take(b',')?;
            }
        }
        self.take(close)
    }

    fn value(&mut self, depth: usize) -> Option<Option<&'a str>> {
        if depth > 128 {
            return None;
        }
        match self.byte()? {
            b'"' => return self.string().map(Some),
            open @ (b'{' | b'[') => {
                self.container(open, depth + 1)?;
            }
            b'-' | b'0'..=b'9' => self.number()?,
            _ => {
                self.0 = ["null", "true", "false"]
                    .into_iter()
                    .find_map(|k| self.0.strip_prefix(k))?
            }
        }
        Some(None)
    }

    fn container(&mut self, open: u8, depth: usize) -> Option<()> {
        let object = open == b'{';
        let close = if object { b'}' } else { b']' };
        self.take(open)?;
        self.entries(close, |cursor| {
            if object {
                cursor.string()?;
                cursor.take(b':')?;
            }
            cursor.value(depth).map(|_| ())
        })
    }
}

fn skip_escape(bytes: &mut impl Iterator<Item = (usize, u8)>) -> Option<()> {
    match bytes.next()?.1 {
        b'u' => {
            for _ in 0..4 {
                if !bytes.next()?.1.is_ascii_hexdigit() {
                    return None;
                }
            }
            Some(())
        }
        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => Some(()),
        _ => None,
    }
}

fn digits(bytes: &[u8], i: &mut usize) -> Option<()> {
    let start = *i;
    while bytes.get(*i).is_some_and(u8::is_ascii_digit) {
        *i += 1
    }
    (*i != start).then_some(())
}

fn fields(text: &str) -> Option<Manifest<'_>> {
    let mut cursor = Cursor(text);
    cursor.take(b'{')?;
    let mut name = None;
    let mut version = None;
    cursor.entries(b'}', |cursor| {
        let key = cursor.string()?;
        if key.contains('\\') {
            return None;
        }
        cursor.take(b':')?;
        let value = cursor.value(0)?;
        match key {
            "name" => name = value,
            "version" => version = value,
            _ => {}
        }
        Some(())
    })?;
    if cursor.byte().is_some() {
        return None;
    }
    let Some(name) = name else {
        return Some(Manifest::Unnamed);
    };
    let version = version.unwrap_or("HEAD");
    if name.contains('\\') || version.contains('\\') {
        return None;
    }
    Some(Manifest::Named(name, version))
}

pub(super) fn manifest(text: &str) -> Manifest<'_> {
    fields(text).unwrap_or(Manifest::Unknown)
}
