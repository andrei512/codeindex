use std::path::{Component, Path, PathBuf};

fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut pos = 0;
    while pos < bytes.len() {
        let byte = if bytes[pos] == b'%' {
            let digits = bytes.get(pos + 1..pos + 3)?;
            let hi = (digits[0] as char).to_digit(16)?;
            let lo = (digits[1] as char).to_digit(16)?;
            pos += 3;
            (hi * 16 + lo) as u8
        } else {
            pos += 1;
            bytes[pos - 1]
        };
        if byte == 0 {
            return None;
        }
        result.push(byte);
    }
    String::from_utf8(result).ok()
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
    if has_scheme(&address) || address.starts_with("//") || address.contains('\\') {
        return None;
    }
    let anchor = decode(fragment)?;
    Some((address, anchor))
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
        match part {
            Component::CurDir => {}
            Component::Normal(name) => target.push(name),
            Component::ParentDir if target.pop() => {}
            _ => return None,
        }
    }
    Some(target.to_str()?.to_owned())
}

pub fn format_handle(path: &str, anchor: &str) -> String {
    let escape = |s: &str| s.replace('%', "%25").replace('#', "%23");
    format!("{}#{}", escape(path), escape(anchor))
}

pub fn parse_handle(input: &str) -> Option<(String, Option<String>)> {
    let (path, fragment) = input
        .split_once('#')
        .map_or((input, None), |(p, a)| (p, Some(a)));
    if path.is_empty() {
        return None;
    }
    let unescape = |s: &str| s.replace("%23", "#").replace("%25", "%");
    Some((unescape(path), fragment.map(unescape)))
}
