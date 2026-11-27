use super::{super::Outcome, Lines};
use crate::adapters::repository::search_paths::{SearchRoot, Traversal};
use std::borrow::Cow;
use std::{fs, io, time::SystemTime};

#[derive(Default, Clone, Copy)]
pub(super) enum Sort {
    #[default]
    None,
    Path,
    Accessed,
    Modified,
    Created,
}
impl Sort {
    pub(super) fn timestamp(self) -> Option<fn(&fs::Metadata) -> io::Result<SystemTime>> {
        match self {
            Self::None | Self::Path => None,
            Self::Modified => Some(fs::Metadata::modified),
            Self::Accessed => Some(fs::Metadata::accessed),
            Self::Created => Some(fs::Metadata::created),
        }
    }
}

#[derive(Default)]
pub(super) struct Scope {
    pub sort: Sort,
    pub reverse: bool,
    pub depth: Option<usize>,
    pub size: Option<u64>,
}
impl Scope {
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

pub(super) struct EntryMetadata {
    entry: fs::DirEntry,
    cached: Option<fs::Metadata>,
}

impl EntryMetadata {
    pub(super) fn get(&mut self) -> io::Result<&fs::Metadata> {
        match self.cached {
            Some(ref value) => Ok(value),
            None => Ok(self.cached.insert(self.entry.metadata()?)),
        }
    }
}

impl Lines<'_> {
    pub(super) fn ordered_paths<T: AsRef<str>>(
        &self,
        origin: SearchRoot<'_>,
        outcome: &mut Outcome,
        record: impl Fn(Cow<'_, str>, &mut EntryMetadata) -> io::Result<T>,
    ) -> io::Result<Vec<T>> {
        let mut paths = if let Some(timestamp) = self.scope.sort.timestamp() {
            let mut keyed = self.collect_paths(origin, outcome, |name, metadata| {
                let key = timestamp(metadata.get()?).unwrap_or(SystemTime::UNIX_EPOCH);
                Ok((key, record(name, metadata)?))
            })?;
            keyed.sort_unstable_by(|(a, left), (b, right)| {
                a.cmp(b).then_with(|| left.as_ref().cmp(right.as_ref()))
            });
            keyed.into_iter().map(|(_, path)| path).collect()
        } else {
            let mut paths = self.collect_paths(origin, outcome, record)?;
            paths.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
            paths
        };
        if self.scope.reverse && !matches!(self.scope.sort, Sort::None) {
            paths.reverse();
        }
        Ok(paths)
    }

    fn collect_paths<T>(
        &self,
        origin: SearchRoot<'_>,
        outcome: &mut Outcome,
        record: impl Fn(Cow<'_, str>, &mut EntryMetadata) -> io::Result<T>,
    ) -> io::Result<Vec<T>> {
        let mut found = Vec::new();
        self.visit_until(origin, outcome, |name, source| {
            found.push(record(name, source)?);
            Ok(false)
        })?;
        Ok(found)
    }

    pub(super) fn visit_until(
        &self,
        origin: SearchRoot<'_>,
        outcome: &mut Outcome,
        mut visit: impl FnMut(Cow<'_, str>, &mut EntryMetadata) -> io::Result<bool>,
    ) -> io::Result<()> {
        let errors = self.walk.visit_files(
            origin,
            Traversal {
                hidden: self.hidden,
                depth: self.scope.depth,
                reverse_path: (self.stops_at_first_hit() && matches!(self.scope.sort, Sort::Path))
                    .then_some(self.scope.reverse),
            },
            self.globs.for_operand(origin.cwd, origin.operand),
            |name, entry, target, included| {
                if !included
                    && (self.globs.positive
                        || !self.types.accepts(name.rsplit('/').next().unwrap_or(&name)))
                {
                    return Ok(false);
                }
                let mut source = EntryMetadata {
                    entry,
                    cached: target,
                };
                if let Some(limit) = self.scope.size
                    && source.get()?.len() > limit
                {
                    return Ok(false);
                }
                visit(name, &mut source)
            },
        )?;
        use std::fmt::Write;
        for error in errors {
            writeln!(outcome.diagnostics, "cx: {error}").unwrap();
        }
        Ok(())
    }
}
