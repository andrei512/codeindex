// An unfinished tag or comment may be followed by thousands of candidate
// openers. Reuse each delimiter search until the scanner passes its result.
#[derive(Default)]
struct Lookahead(usize);

impl Lookahead {
    fn after(
        &mut self,
        text: &str,
        start: usize,
        find: impl FnOnce(&str) -> Option<usize>,
    ) -> Option<usize> {
        if self.0 <= start {
            self.0 = find(&text[start..]).map_or(usize::MAX, |n| start + n);
        }
        (self.0 != usize::MAX).then_some(self.0)
    }
}

#[derive(Default)]
pub(super) struct InlineHtml {
    tag_end: Lookahead,
    comment_end: Lookahead,
    address_marker: Lookahead,
    nested_open: Lookahead,
}

impl InlineHtml {
    pub(super) fn end(&mut self, s: &str, pos: usize, title: Option<&mut String>) -> Option<usize> {
        if s[pos..].starts_with("<!--")
            && let Some(end) = self.comment_end.after(s, pos, |tail| tail.find("-->"))
        {
            return Some(end + 3);
        }
        let end = self.tag_end.after(s, pos, |tail| tail.find('>'))?;
        let inside = &s[pos + 1..end];
        let tag = inside.strip_prefix('/').unwrap_or(inside).as_bytes();
        if tag.first().is_some_and(u8::is_ascii_alphabetic) && valid_tag_name(tag) {
            return Some(end + 1);
        }
        if !self.autolink(s, pos, end) {
            return None;
        }
        if let Some(title) = title {
            title.push_str(inside);
        }
        Some(end + 1)
    }

    fn autolink(&mut self, text: &str, start: usize, end: usize) -> bool {
        let Some(marker) = self
            .address_marker
            .after(text, start, |tail| tail.find([':', '@']))
            .filter(|&marker| marker < end)
        else {
            return false;
        };
        let nested_tag = self
            .nested_open
            .after(text, start + 1, |tail| tail.find('<'))
            .is_some_and(|next| next < end);
        !nested_tag && is_autolink(&text.as_bytes()[start + 1..end], marker - start - 1)
    }
}

fn is_autolink(address: &[u8], marker: usize) -> bool {
    let head = &address[..marker];
    let tail = &address[marker + 1..];
    if address[marker] == b':' {
        return is_uri(head, tail);
    }
    !head.is_empty()
        && head
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(b))
        && tail.split(|&b| b == b'.').all(is_domain_label)
}

fn is_uri(scheme: &[u8], body: &[u8]) -> bool {
    (2..=32).contains(&scheme.len())
        && scheme[0].is_ascii_alphabetic()
        && scheme
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(b))
        && body.iter().all(|&b| b > b' ' && b != 127)
}

fn is_domain_label(label: &[u8]) -> bool {
    (1..=63).contains(&label.len())
        && label[0].is_ascii_alphanumeric()
        && label[label.len() - 1].is_ascii_alphanumeric()
        && label
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

// The first word permits alphanumerics and dashes followed by trailing slashes.
fn valid_tag_name(tag: &[u8]) -> bool {
    let rest = &tag[tag
        .iter()
        .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'-')
        .count()..];
    let suffix = &rest[rest.iter().take_while(|&&b| b == b'/').count()..];
    suffix.first().is_none_or(u8::is_ascii_whitespace)
}
