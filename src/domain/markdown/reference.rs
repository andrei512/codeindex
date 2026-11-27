//! Source spelling, boundaries and lookup keys of Markdown reference labels.
pub(super) fn key(s: &str) -> std::borrow::Cow<'_, str> {
    // A normalized ASCII label can be used to probe the definition map
    // directly, including common multiword labels.
    if canonical_ascii_label(s.as_bytes()) {
        return s.into();
    }
    let mut normalized = String::with_capacity(s.len());
    // CommonMark folds spaces, tabs and line endings in link labels. Preserve
    // the whitespace normalization of the footnote extension.
    let footnote = s.starts_with('^');
    let separator =
        |c: char| matches!(c, ' ' | '\t' | '\r' | '\n') || (footnote && c.is_whitespace());
    for word in s.split(separator).filter(|word| !word.is_empty()) {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    if normalized.is_ascii() {
        normalized.make_ascii_lowercase();
        normalized.into()
    } else {
        normalized
            .to_lowercase()
            .to_uppercase()
            .to_lowercase()
            .into()
    }
}

fn canonical_ascii_label(bytes: &[u8]) -> bool {
    if bytes.iter().all(u8::is_ascii_lowercase) {
        return true;
    }
    bytes.split(|&b| b == b' ').all(|word| {
        !word.is_empty()
            && word
                .iter()
                .all(|b| b.is_ascii_graphic() && !b.is_ascii_uppercase())
    })
}

const MAX_LABEL_CHARACTERS: usize = 999;
pub(super) const MAX_LABEL_BYTES: usize = 4 * MAX_LABEL_CHARACTERS;

/// The reference grammar counts source characters, including backslash escapes.
/// Most labels fit the limit by byte length alone.
fn label_length(label: &str) -> bool {
    label.len() <= MAX_LABEL_CHARACTERS
        || (label.len() <= MAX_LABEL_BYTES && label.chars().count() <= MAX_LABEL_CHARACTERS)
}

/// A reference label ends at the first unescaped bracket. An opening bracket
/// there cannot begin a nested label.
pub(super) fn next_label_bracket(source: &str, footnote: bool) -> Option<(usize, u8)> {
    // Footnote identifiers retain their existing extension grammar. Ordinary
    // labels cannot reach a delimiter beyond 999 four-byte scalars.
    let prefix = if footnote {
        source
    } else {
        &source[..source.floor_char_boundary(MAX_LABEL_BYTES + 1)]
    };
    let bytes = prefix.as_bytes();
    let mut pos = 0;
    while let Some(skip) = bytes[pos..]
        .iter()
        .position(|b| matches!(b, b']' | b'[' | b'\\'))
    {
        pos += skip;
        let mark = bytes[pos];
        if mark != b'\\' {
            return Some((pos, mark));
        }
        pos += 1 + usize::from(bytes.get(pos + 1).is_some_and(u8::is_ascii_punctuation));
    }
    None
}

/// Footnotes retain their extension grammar; ordinary labels need non-separator text.
pub(super) fn complete(label: &str) -> bool {
    label.starts_with('^') || ordinary_label(label)
}

fn ordinary_label(label: &str) -> bool {
    label_length(label)
        && label
            .bytes()
            .any(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
}

/// Inline link text may contain code and HTML. Its following reference identifier
/// ends at the first raw, unescaped bracket, just as a definition label does.
pub(super) fn lookup<'a>(
    visible: &str,
    suffix: &str,
    definitions: &'a std::collections::HashMap<String, String>,
) -> Option<(&'a str, usize)> {
    let (written, used) = explicit(suffix).unwrap_or(("", 0));
    let id = if written.is_empty() { visible } else { written };
    if id.starts_with('^') || !label_length(id) {
        return None;
    }
    let address = definitions
        .get(id)
        .filter(|_| id.is_ascii())
        .or_else(|| match key(id) {
            std::borrow::Cow::Owned(folded) => definitions.get(&folded),
            std::borrow::Cow::Borrowed(_) => None,
        })?;
    Some((address, used))
}

fn explicit(suffix: &str) -> Option<(&str, usize)> {
    let body = suffix.strip_prefix('[')?;
    let (end, b']') = next_label_bracket(body, false)? else {
        return None;
    };
    let label = &body[..end];
    if !label.is_empty() && !ordinary_label(label) {
        return None;
    }
    Some((label, end + 2))
}
