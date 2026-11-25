use std::{fs, path::Path, time::SystemTime};

#[derive(Default, Clone, Copy)]
pub(super) enum Sort {
    #[default]
    None,
    Path,
    Accessed,
    Modified,
    Created,
}
#[derive(Default)]
pub(super) struct Scope {
    pub sort: Sort,
    pub reverse: bool,
    pub depth: Option<usize>,
    pub size: Option<u64>,
}
impl Scope {
    pub fn accepts(&self, suffix: &Path, size: u64) -> bool {
        self.depth
            .is_none_or(|limit| suffix.components().count() <= limit)
            && self.size.is_none_or(|limit| size <= limit)
    }
    pub fn order<T>(&self, entries: &mut [T], root: &Path, name: impl Fn(&T) -> &str) {
        match self.sort {
            Sort::None => return,
            Sort::Path => entries.sort_by(|a, b| name(a).cmp(name(b))),
            kind => entries.sort_by_cached_key(|entry| {
                fs::metadata(root.join(name(entry)))
                    .and_then(|m| match kind {
                        Sort::Modified => m.modified(),
                        Sort::Created => m.created(),
                        _ => m.accessed(),
                    })
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            }),
        }
        if self.reverse {
            entries.reverse();
        }
    }
    pub fn set_sort(&mut self, value: &str, reverse: bool) -> Result<(), String> {
        self.sort = match value {
            "none" => Sort::None,
            "path" => Sort::Path,
            "accessed" => Sort::Accessed,
            "modified" => Sort::Modified,
            "created" => Sort::Created,
            _ => return Err(format!("unrecognized sort kind: {value}")),
        };
        self.reverse = reverse;
        Ok(())
    }
}
pub(super) fn size(value: &str) -> Result<u64, String> {
    let (digits, factor) = match value.as_bytes().last() {
        Some(b'K' | b'k') => (&value[..value.len() - 1], 1024),
        Some(b'M' | b'm') => (&value[..value.len() - 1], 1024 * 1024),
        Some(b'G' | b'g') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .ok_or_else(|| format!("invalid size: {value}"))
}
