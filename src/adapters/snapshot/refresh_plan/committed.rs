//! Establish whether the bytes indexed for a path are present at HEAD.
use super::{BlobId, CommitTree, Path, Row};
use std::collections::HashMap;

pub(super) struct Provenance<'a> {
    root: &'a Path,
    head: Option<&'a str>,
    previous_head: Option<&'a str>,
    comparison: Option<Comparison>,
}

impl<'a> Provenance<'a> {
    pub(super) fn new(
        root: &'a Path,
        head: Option<&'a str>,
        previous_head: Option<&'a str>,
    ) -> Self {
        Self {
            root,
            head,
            previous_head,
            comparison: None,
        }
    }

    pub(super) fn committed(
        &mut self,
        path: &str,
        blob: Option<BlobId>,
        previous: Option<&Row>,
    ) -> bool {
        let (Some(head), Some(blob)) = (self.head, blob) else {
            return false;
        };
        let prior = previous.filter(|row| row.blob == Some(blob));
        if self.previous_head == Some(head)
            && let Some(row) = prior
        {
            return row.committed;
        }
        let comparison = self
            .comparison
            .get_or_insert_with(|| Comparison::open(self.root, head, self.previous_head));
        comparison.contains(path, blob, prior)
    }
}

struct Comparison {
    tree: Option<CommitTree>,
    changes: Option<HashMap<String, Option<BlobId>>>,
}

fn refresh_changed_paths(
    changes: &HashMap<String, Option<BlobId>>,
    observed: &[super::ObservedPath],
    rows: &mut [Row],
) {
    let comparisons = observed.len().checked_ilog2().unwrap_or(0) as usize + 1;
    if changes.len().saturating_mul(comparisons) < rows.len() {
        refresh_sparse_changes(changes, observed, rows);
        return;
    }
    let updates = rows.iter_mut().zip(observed).filter_map(|(row, file)| {
        Some((row, *changes.get(&file.path)?))
    });
    for (row, blob) in updates {
        update_commit(row, blob);
    }
}

fn refresh_sparse_changes(
    changes: &HashMap<String, Option<BlobId>>,
    observed: &[super::ObservedPath],
    rows: &mut [Row],
) {
    let updates = changes.iter().filter_map(|(path, blob)| {
        let id = observed.binary_search_by(|file| file.path.as_str().cmp(path)).ok()?;
        Some((id, *blob))
    });
    for (id, blob) in updates {
        update_commit(&mut rows[id], blob);
    }
}

fn update_commit(row: &mut Row, blob: Option<BlobId>) {
    row.committed = row.blob.is_some() && row.blob == blob;
}

impl Comparison {
    fn open(root: &Path, head: &str, previous: Option<&str>) -> Self {
        let mut tree = CommitTree::open(root, head).ok();
        let changes = previous
            .filter(|&old| old != head)
            .and_then(|old| tree.as_mut()?.changed_paths(old).ok());
        Self { tree, changes }
    }

    /// Source content is identical to the previous checkpoint. A commit diff
    /// already identifies every path whose committed bit can have changed.
    fn refresh_rows(&mut self, observed: &[super::ObservedPath], rows: &mut [Row]) {
        if let Some(changes) = &self.changes {
            refresh_changed_paths(changes, observed, rows);
            return;
        }
        for (row, file) in rows.iter_mut().zip(observed) {
            row.committed = row
                .blob
                .is_some_and(|blob| self.contains(&file.path, blob, None));
        }
    }

    fn contains(&mut self, path: &str, blob: BlobId, prior: Option<&Row>) -> bool {
        if let Some(new) = self.changes.as_ref().and_then(|changes| changes.get(path)) {
            return *new == Some(blob);
        }
        if let Some(row) = prior.filter(|_| self.changes.is_some()) {
            return row.committed;
        }
        self.tree
            .as_mut()
            .and_then(|tree| tree.blob(path).ok().flatten())
            == Some(blob)
    }
}

impl super::Checkpoint {
    /// The metadata proof established identical path order and content for
    /// every row. Only stamps and, when HEAD moves, commit provenance can change.
    pub(in crate::adapters::snapshot) fn refresh_metadata(
        &self,
        root: &Path,
        (head, previous_head): (Option<&str>, Option<&str>),
        observed: &[super::ObservedPath],
        unchanged: usize,
    ) -> Self {
        debug_assert_eq!(self.rows.len(), observed.len());
        let mut next = self.clone();
        for (row, file) in next.rows[unchanged..]
            .iter_mut()
            .zip(&observed[unchanged..])
        {
            row.stamp = file.stamp;
        }
        if head != previous_head {
            Self::refresh_provenance(root, (head, previous_head), observed, &mut next.rows);
        }
        next
    }

    fn refresh_provenance(
        root: &Path,
        (head, previous_head): (Option<&str>, Option<&str>),
        observed: &[super::ObservedPath],
        rows: &mut [Row],
    ) {
        match head {
            Some(head) if rows.iter().any(|row| row.blob.is_some()) => {
                Comparison::open(root, head, previous_head).refresh_rows(observed, rows);
            }
            None => rows.iter_mut().for_each(|row| row.committed = false),
            Some(_) => {}
        }
    }
}
