use super::entities;
use std::collections::{HashMap, HashSet};

pub(super) fn key(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .to_uppercase()
        .to_lowercase()
}

fn entity(s: &str) -> Option<(usize, String)> {
    let stop = s.as_bytes().iter().take(34).position(|&c| c == b';')?;
    let name = s.get(1..stop)?;
    let decoded = if let Some(value) = name.strip_prefix('#') {
        let code = if let Some(hex) = value.strip_prefix(['x', 'X']) {
            if hex.len() > 6 {
                return None;
            }
            u32::from_str_radix(hex, 16).ok()?
        } else {
            if value.len() > 7 || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            value.parse().ok()?
        };
        char::from_u32(code)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{fffd}')
            .to_string()
    } else {
        entities::entity(name)?.to_owned()
    };
    Some((stop + 1, decoded))
}

pub(super) fn unescape(s: &str) -> String {
    if !s.contains(['\\', '&']) {
        return s.to_owned();
    }
    let mut result = String::new();
    let mut pos = 0;
    while pos < s.len() {
        if s.as_bytes()[pos] == b'&' {
            if let Some((n, text)) = entity(&s[pos..]) {
                result.push_str(&text);
                pos += n;
                continue;
            }
        }
        if s.as_bytes()[pos] == b'\\'
            && s.as_bytes()
                .get(pos + 1)
                .is_some_and(u8::is_ascii_punctuation)
        {
            pos += 1;
        }
        let c = s[pos..].chars().next().unwrap();
        result.push(c);
        pos += c.len_utf8();
    }
    result
}

fn run(b: &[u8], pos: usize) -> usize {
    b[pos..].iter().take_while(|&&x| x == b[pos]).count()
}
fn code_end(b: &[u8], mut p: usize, n: usize) -> Option<usize> {
    while p < b.len() {
        p += b[p..].iter().position(|&x| x == b'`')?;
        let run = run(b, p);
        if run == n {
            return Some(p);
        }
        p += run;
    }
    None
}

// Returns the destination and its exclusive end, before any optional title.
pub(super) fn destination(s: &str) -> Option<(&str, usize)> {
    let b = s.as_bytes();
    let angle = b.first() == Some(&b'<');
    let start = usize::from(angle);
    let mut p = start;
    let mut depth = 0;
    while p < b.len() {
        match b[p] {
            b'\\' if b.get(p + 1).is_some_and(u8::is_ascii_punctuation) => p += 2,
            b'>' if angle => return Some((&s[start..p], p + 1)),
            b'<' | b'\n' if angle => return None,
            c if !angle && c.is_ascii_whitespace() => break,
            b'(' if !angle => {
                depth += 1;
                p += 1;
            }
            b')' if !angle && depth == 0 => break,
            b')' if !angle => {
                depth -= 1;
                p += 1;
            }
            _ => p += 1,
        }
    }
    (!angle && depth == 0).then_some((&s[start..p], p))
}

fn inline_destination(s: &str) -> Option<(String, usize)> {
    let trimmed = s.strip_prefix('(')?.trim_start_matches(char::is_whitespace);
    let (url, n) = destination(trimmed)?;
    let rest = trimmed[n..].trim_start_matches(char::is_whitespace);
    let rest = if trimmed[n..].len() > rest.len() && rest.starts_with(['"', '\'', '(']) {
        let closer = match rest.as_bytes()[0] {
            b'(' => b')',
            c => c,
        };
        let mut p = 1;
        while p < rest.len() && rest.as_bytes()[p] != closer {
            if rest.as_bytes()[p] == b'\\'
                && rest
                    .as_bytes()
                    .get(p + 1)
                    .is_some_and(u8::is_ascii_punctuation)
            {
                p += 1;
            }
            p += 1;
        }
        rest.get(p + 1..)?.trim_start_matches(char::is_whitespace)
    } else {
        rest
    };
    rest.starts_with(')')
        .then(|| (unescape(url), s.len() - rest.len() + 1))
}

fn tag_end(s: &str) -> Option<usize> {
    if s.starts_with("<!--") {
        return s.find("-->").map(|i| i + 3);
    }
    let n = s.find('>')?;
    let body = &s[1..n];
    let tag = body.trim_start_matches('/');
    let id = tag.split_ascii_whitespace().next()?.trim_end_matches('/');
    if !tag.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    (id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        || (!body.contains(char::is_whitespace) && body.contains([':', '@'])))
    .then_some(n + 1)
}

fn paired_brackets(s: &str) -> HashMap<usize, usize> {
    let b = s.as_bytes();
    let mut stack = Vec::new();
    let mut pairs = HashMap::new();
    let mut p = 0;
    while p < b.len() {
        match b[p] {
            b'\\' if b.get(p + 1).is_some_and(u8::is_ascii_punctuation) => p += 2,
            b'`' => {
                let n = run(b, p);
                p = code_end(b, p + n, n).map_or(p + n, |end| end + n);
            }
            b'<' => p += tag_end(&s[p..]).unwrap_or(1),
            b'[' => {
                stack.push(p);
                p += 1;
            }
            b']' => {
                if let Some(open) = stack.pop() {
                    pairs.insert(open, p);
                }
                p += 1;
            }
            _ => p += 1,
        }
    }
    pairs
}

fn label<'a>(
    s: &'a str,
    p: usize,
    close: usize,
    pairs: &HashMap<usize, usize>,
    defs: &'a HashMap<String, String>,
) -> Option<(String, usize)> {
    let next = close + 1;
    if let Some((url, len)) = inline_destination(&s[next..]) {
        return Some((url, next + len));
    }
    let mut id = &s[p + 1..close];
    let mut end = next;
    if let Some(&stop) = pairs.get(&next) {
        if stop != next + 1 {
            id = &s[next + 1..stop];
        }
        end = stop + 1;
    }
    if id.is_empty() || id.starts_with('^') || id.len() > 4000 {
        return None;
    }
    Some((defs.get(&key(id))?.clone(), end))
}

pub(super) fn scan(
    s: &str,
    defs: &HashMap<String, String>,
    render: bool,
    mut emit: impl FnMut(usize, &str),
) -> String {
    if !render && !s.contains('[') {
        return String::new();
    }
    let pairs = if s.contains('[') {
        paired_brackets(s)
    } else {
        HashMap::new()
    };
    // An inner link deactivates enclosing link openers. Images can still
    // contain links in their description. Compute that decision in closing order.
    let mut inactive = HashSet::new();
    if pairs.len() > 1 {
        let mut order: Vec<_> = pairs.iter().collect();
        order.sort_unstable_by_key(|(_, close)| **close);
        let mut inner = None;
        for (&open, &close) in order {
            if open > 0 && matches!(s.as_bytes()[open - 1], b'!' | b']') {
                continue;
            }
            if inner.is_some_and(|last| last > open) {
                inactive.insert(open);
                continue;
            }
            if label(s, open, close, &pairs, defs).is_some() {
                inner = Some(open);
            }
        }
    }
    let b = s.as_bytes();
    let mut title = String::new();
    let mut pos = 0;
    // On rendering a link, consume its label in place, then skip the destination.
    let mut ends: Vec<(usize, usize)> = Vec::new();
    let mut styles: Vec<(u8, usize)> = Vec::new();
    while pos < b.len() {
        if let Some(&(end, resume)) = ends.last() {
            if end == pos {
                ends.pop();
                pos = resume;
                continue;
            }
        }
        if b[pos] == b'\\' && b.get(pos + 1).is_some_and(u8::is_ascii_punctuation) {
            pos += 1;
            if render {
                title.push(char::from(b[pos]));
            }
            pos += 1;
            continue;
        }
        if b[pos] == b'`' {
            let n = run(b, pos);
            if let Some(end) = code_end(b, pos + n, n) {
                if render {
                    let code = s[pos + n..end].replace('\n', " ");
                    let stripped = if code.starts_with(' ')
                        && code.ends_with(' ')
                        && code.trim_matches(' ').len() > 0
                    {
                        &code[1..code.len() - 1]
                    } else {
                        &code
                    };
                    title.push_str(stripped);
                }
                pos = end + n;
                continue;
            }
            if render {
                title.push_str(&s[pos..pos + n]);
            }
            pos += n;
            continue;
        }
        let image = b[pos] == b'!' && b.get(pos + 1) == Some(&b'[');
        let open = pos + usize::from(image);
        if let Some(&close) = pairs.get(&open).filter(|_| !inactive.contains(&open)) {
            if let Some((url, end)) = label(s, open, close, &pairs, defs) {
                if !image {
                    emit(open, &url);
                }
                if render || image {
                    ends.push((close, end));
                    pos = open + 1;
                } else {
                    pos = end;
                }
                continue;
            }
            if render
                && s[open + 1..close].starts_with('^')
                && defs.contains_key(&key(&s[open + 1..close]))
            {
                title.push_str(&s[open + 2..close]);
                pos = close + 1;
                continue;
            }
        }
        if s.as_bytes()[pos..].starts_with(b"<!--")
            && let Some(n) = s[pos..].find("-->")
        {
            pos += n + 3;
            continue;
        }
        if b[pos] == b'<' {
            if let Some(n) = s[pos..].find('>') {
                let inside = &s[pos + 1..pos + n];
                let tag = inside.trim_start_matches('/');
                if tag.starts_with(|c: char| c.is_ascii_alphabetic())
                    && (tag.split_ascii_whitespace().next().is_some_and(|name| {
                        name.trim_end_matches('/')
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    }) || inside.contains([':', '@']))
                {
                    if render
                        && inside.contains([':', '@'])
                        && !inside.contains(char::is_whitespace)
                    {
                        title.push_str(inside);
                    }
                    pos += n + 1;
                    continue;
                }
            }
        }
        if !render {
            pos += 1;
            continue;
        }
        if b[pos] == b'&' {
            if let Some((n, text)) = entity(&s[pos..]) {
                title.push_str(&text);
                pos += n;
                continue;
            }
        }
        if matches!(b[pos], b'*' | b'_' | b'~') {
            let n = run(b, pos);
            let prev = s[..pos].chars().next_back();
            let next = s[pos + n..].chars().next();
            let intraword = b[pos] == b'_'
                && prev.is_some_and(char::is_alphanumeric)
                && next.is_some_and(char::is_alphanumeric);
            if !intraword
                && prev.is_some_and(|c| !c.is_whitespace())
                && styles.last() == Some(&(b[pos], n))
            {
                styles.pop();
                pos += n;
                continue;
            }
            if !intraword
                && next.is_some_and(|c| !c.is_whitespace())
                && s[pos + n..].contains(&s[pos..pos + n])
            {
                styles.push((b[pos], n));
                pos += n;
                continue;
            }
            title.push_str(&s[pos..pos + n]);
            pos += n;
            continue;
        }
        let c = s[pos..].chars().next().unwrap();
        title.push(if c == '\n' { ' ' } else { c });
        pos += c.len_utf8();
    }
    title
}
