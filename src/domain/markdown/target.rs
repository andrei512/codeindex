use std::path::{Component, Path, PathBuf};

fn decode(value: &str) -> Option<std::borrow::Cow<'_, str>> {
    let bytes = value.as_bytes();
    let mut pos = value.find('%').unwrap_or(bytes.len());
    let prefix = &bytes[..pos];
    if prefix.contains(&0) {
        return None;
    }
    if pos == bytes.len() {
        return Some(value.into());
    }
    let mut result = Vec::with_capacity(bytes.len());
    result.extend_from_slice(prefix);
    while pos < bytes.len() {
        let (n, byte) = decode_byte(&bytes[pos..])?;
        if byte == 0 {
            return None;
        }
        result.push(byte);
        pos += n;
    }
    String::from_utf8(result).ok().map(Into::into)
}

fn decode_byte(bytes: &[u8]) -> Option<(usize, u8)> {
    let (&first, tail) = bytes.split_first()?;
    if first != b'%' {
        return Some((1, first));
    }
    let [a, b, ..] = tail else { return None };
    let hi = char::from(*a).to_digit(16)?;
    let lo = char::from(*b).to_digit(16)?;
    Some((3, (hi * 16 + lo) as u8))
}

fn has_scheme(path: &str) -> bool {
    let Some((prefix, _)) = path.split_once(':') else {
        return false;
    };
    let mut chars = prefix.bytes();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-'))
}

pub(super) fn local_address(url: &str) -> Option<(String, String)> {
    let (address, fragment) = url.split_once('#').unwrap_or((url, ""));
    let address = address.split_once('?').map_or(address, |(path, _)| path);
    let address = decode(address)?;
    let external = has_scheme(&address) || address.starts_with("//");
    if external || address.contains('\\') {
        return None;
    }
    let anchor = decode(fragment)?;
    Some((address.into_owned(), anchor.into_owned()))
}

pub(super) fn qualify(file: &Path, address: &str) -> Option<String> {
    if address.is_empty() {
        return Some(file.to_str()?.to_owned());
    }
    let mut target = if address.starts_with('/') {
        PathBuf::new()
    } else {
        file.parent()?.to_owned()
    };
    for part in Path::new(address.trim_start_matches('/')).components() {
        append_component(&mut target, part)?;
    }
    target.into_os_string().into_string().ok()
}

fn append_component(target: &mut PathBuf, part: Component<'_>) -> Option<()> {
    match part {
        Component::CurDir => {}
        Component::Normal(name) => target.push(name),
        Component::ParentDir if target.pop() => {}
        _ => return None,
    }
    Some(())
}

fn escaped_marker(marker: &str) -> &'static str {
    match marker {
        "%" => "%25",
        "#" => "%23",
        _ => unreachable!("only percent and fragment markers require escaping"),
    }
}

pub fn format_handle(path: &str, anchor: &str) -> String {
    fn append(out: &mut String, s: &str) {
        let mut copied = 0;
        for (at, marker) in s.match_indices(['%', '#']) {
            out.push_str(&s[copied..at]);
            out.push_str(escaped_marker(marker));
            copied = at + 1;
        }
        out.push_str(&s[copied..]);
    }
    let mut handle = String::with_capacity(path.len() + 1 + anchor.len());
    append(&mut handle, path);
    handle.push('#');
    append(&mut handle, anchor);
    handle
}

pub fn parse_handle(input: &str) -> Option<(String, Option<String>)> {
    let (path, fragment) = input
        .split_once('#')
        .map_or((input, None), |(p, a)| (p, Some(a)));
    if path.is_empty() {
        return None;
    }
    fn unescape(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let mut copied = 0;
        for (at, _) in s.match_indices("%2") {
            let decoded = match s.as_bytes().get(at + 2) {
                Some(b'3') => '#',
                Some(b'5') => '%',
                _ => continue,
            };
            result.push_str(&s[copied..at]);
            result.push(decoded);
            copied = at + 3;
        }
        result.push_str(&s[copied..]);
        result
    }
    Some((unescape(path), fragment.map(unescape)))
}
