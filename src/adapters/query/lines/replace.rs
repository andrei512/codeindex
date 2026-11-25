use super::{Lines, Pattern};
use std::ops::Range;
impl Lines<'_> {
    pub(super) fn replacement(&self, template: &str, hay: &str, span: Range<usize>) -> Vec<u8> {
        let mut result = Vec::new();
        let mut captures = None;
        let mut rest = template;
        while let Some((before, tail)) = rest.split_once('$') {
            result.extend_from_slice(before.as_bytes());
            if let Some(tail) = tail.strip_prefix('$') {
                result.push(b'$');
                rest = tail;
                continue;
            }
            let (key, after) = if let Some(braced) = tail.strip_prefix('{')
                && let Some(pair) = braced.split_once('}')
            {
                pair
            } else {
                tail.split_at(
                    tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(tail.len()),
                )
            };
            if key.is_empty() {
                result.push(b'$');
                rest = tail;
                continue;
            }
            if key == "0" {
                result.extend_from_slice(hay[span.clone()].as_bytes());
            } else {
                let groups = captures.get_or_insert_with(|| {
                    self.patterns.iter().find_map(|p| match p {
                        Pattern::Regex(re) => re.groups(hay, span.clone()).map(|g| (p, g)),
                        Pattern::Pcre(re) => re
                            .captures(hay, span.start)
                            .next()
                            .filter(|g| g.first() == Some(&Some(span.clone())))
                            .map(|g| (p, g)),
                        _ => None,
                    })
                });
                if let Some((re, g)) = groups
                    && let Some(id) = (match re {
                        Pattern::Regex(re) => re.capture_id(key),
                        Pattern::Pcre(re) => re.id(key),
                        _ => None,
                    })
                    && let Some(Some(r)) = g.get(id)
                {
                    result.extend_from_slice(hay[r.clone()].as_bytes());
                }
            }
            rest = after;
        }
        result.extend_from_slice(rest.as_bytes());
        result
    }
}
