//! Decode file catalogs. Each version adds fields to the preceding row layout.
use super::{
    CATALOG_MAGIC, Entry, Observation,
    binary::{Decoder, bad},
};
use std::{io, ops::Range};

/// Repository-relative paths are sorted, and a regular file cannot also be a directory.
/// Keep its original ordinal when selecting one file or a directory's descendants.
pub(crate) fn scope_range<T>(rows: &[T], scope: &str, path: impl Fn(&T) -> &str) -> Range<usize> {
    if scope.is_empty() {
        return 0..rows.len();
    }
    let first = match rows.binary_search_by(|row| path(row).cmp(scope)) {
        Ok(id) => return id..id + 1,
        Err(first) => first,
    };
    let prefix = format!("{scope}{}", std::path::MAIN_SEPARATOR);
    let start = first + rows[first..].partition_point(|row| path(row) < prefix.as_str());
    let count = rows[start..].partition_point(|row| path(row).starts_with(&prefix));
    start..start + count
}

fn read_score(d: &mut Decoder) -> io::Result<crate::domain::code::score::Saved> {
    match d.take(1)? {
        [0] => return Ok(None),
        [1] => {}
        _ => return Err(bad()),
    }
    use crate::domain::code::score::{Finding, Rule, Score};
    let lines = d.num()?;
    let count = d.num()?;
    // Every finding contains one rule byte and three 64-bit fields.
    if count > d.0.len() / 25 {
        return Err(bad());
    }
    let mut findings = Vec::with_capacity(count);
    for _ in 0..count {
        let rule = *Rule::ALL.get(d.take(1)?[0] as usize).ok_or_else(bad)?;
        let line = d.num()?;
        let value = d.num()?;
        let unit = d.string()?.to_owned();
        findings.push(Finding {
            rule,
            line,
            value,
            unit,
        });
    }
    Ok(Some(std::sync::Arc::new(Score { lines, findings })))
}
fn read_line_ends(d: &mut Decoder, source_length: u64) -> io::Result<Vec<usize>> {
    let words = d.words()?;
    let mut ends = Vec::with_capacity(words.len());
    let mut previous = 0;
    for &word in words {
        let end = u64::from_le_bytes(word);
        if end <= previous || end > source_length {
            return Err(bad());
        }
        ends.push(usize::try_from(end).map_err(|_| bad())?);
        previous = end;
    }
    Ok(ends)
}

// Packed catalogs encode a length. Legacy catalogs encode an absolute end.
fn catalog_range(d: &mut Decoder, start: u64, packed: bool) -> io::Result<Range<u64>> {
    let word = d.num()? as u64;
    let end = if packed {
        start.checked_add(word).ok_or_else(bad)?
    } else {
        if word < start {
            return Err(bad());
        }
        word
    };
    Ok(start..end)
}

#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Format {
    Legacy,
    Packed,
    Hash,
    Score,
    TestScore,
    Marker,
}

impl Format {
    fn read(d: &mut Decoder) -> io::Result<Self> {
        let format = match d.0.get(..8) {
            Some(magic) if magic == CATALOG_MAGIC => Self::Marker,
            Some(b"CIXFILE5") => Self::TestScore,
            Some(b"CIXFILE4") => Self::Score,
            Some(b"CIXFILE3") => Self::Hash,
            Some(b"CIXFILE2") => Self::Packed,
            _ => return Ok(Self::Legacy),
        };
        d.take(8)?;
        Ok(format)
    }
}

fn read_blob(d: &mut Decoder) -> io::Result<crate::domain::BlobId> {
    let len = d.take(1)?[0];
    if !matches!(len, 20 | 32) {
        return Err(bad());
    }
    Ok(crate::domain::BlobId::from_raw(d.take(len as usize)?))
}

struct Catalog<'a> {
    d: Decoder<'a>,
    format: Format,
    size: usize,
    source_len: u64,
    source_end: u64,
}

impl Catalog<'_> {
    fn position(&self) -> u64 {
        (self.size - self.d.0.len()) as u64
    }

    fn source(&mut self, flags: u8) -> io::Result<Option<Range<u64>>> {
        match flags {
            0 => return Ok(None),
            1 => {}
            3 if self.format == Format::Marker => {}
            _ => return Err(bad()),
        }
        let packed = self.format >= Format::Packed;
        let start = if packed {
            self.source_end
        } else {
            self.d.num()? as u64
        };
        let range = catalog_range(&mut self.d, start, packed)?;
        if range.end > self.source_len {
            return Err(bad());
        }
        self.source_end = range.end;
        Ok(Some(range))
    }

    fn score(&mut self, since: Format) -> io::Result<crate::domain::code::score::Saved> {
        if self.format < since {
            return Ok(None);
        }
        read_score(&mut self.d)
    }

    fn entry(&mut self, previous: Option<&Entry>) -> io::Result<Entry> {
        let start = self.position();
        let path = self.d.string()?;
        if previous.is_some_and(|entry| entry.path() >= path) {
            return Err(bad());
        }
        let stamp = super::binary::stamp(self.d.take(56)?.try_into().unwrap());
        let flags = self.d.take(1)?[0];
        let bytes = self.source(flags)?;
        let blob = if self.format >= Format::Hash && bytes.is_some() {
            Some(read_blob(&mut self.d)?)
        } else {
            None
        };
        let line_ends = read_line_ends(&mut self.d, bytes.as_ref().map_or(0, |r| r.end - r.start))?;
        let score = self.score(Format::Score)?;
        let test_score = self.score(Format::TestScore)?;
        let markers = self.format == Format::Marker;
        Ok(Entry {
            score_marker: flags == 3 || (!markers && score.is_some()),
            test_score,
            score,
            blob,
            catalog_row: if markers {
                start..self.position()
            } else {
                0..0
            },
            original_stamp: None,
            observation: Observation {
                path: path.to_owned(),
                stamp,
            },
            bytes,
            line_ends,
            evidence: 0..0,
        })
    }

    fn finish(mut self, files: &mut [Entry]) -> io::Result<()> {
        let packed = self.format >= Format::Packed;
        if packed && self.source_end != self.source_len {
            return Err(bad());
        }
        if packed || !self.d.0.is_empty() {
            let mut from = self.d.num()? as u64;
            for f in files {
                f.evidence = catalog_range(&mut self.d, from, packed)?;
                from = f.evidence.end;
            }
        }
        self.d.end()
    }
}

pub(crate) fn files(bytes: &[u8], source_len: u64) -> io::Result<Vec<Entry>> {
    let mut d = Decoder(bytes);
    let format = Format::read(&mut d)?;
    let count = d.num()?;
    // Smallest legacy row: path length, seven stamp words, source flag, line count.
    if count > d.0.len() / (8 + 7 * 8 + 1 + 8) {
        return Err(bad());
    }
    let mut files = Vec::with_capacity(count);
    let mut catalog = Catalog {
        d,
        format,
        size: bytes.len(),
        source_len,
        source_end: 0,
    };
    for _ in 0..count {
        let entry = catalog.entry(files.last())?;
        files.push(entry);
    }
    catalog.finish(&mut files)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(buffer: &mut Vec<u8>, value: u64) {
        buffer.extend_from_slice(&value.to_le_bytes());
    }

    struct Fixture {
        bytes: Vec<u8>,
        start: usize,
        flag: usize,
        line: usize,
        end: usize,
    }

    fn catalog_row(version: u8) -> Fixture {
        let mut bytes = Vec::new();
        if version > 1 {
            bytes.extend(format!("CIXFILE{version}").bytes());
        }
        word(&mut bytes, 1);
        let start = bytes.len();
        word(&mut bytes, 1);
        bytes.push(b'x');
        bytes.extend([0; 56]);
        let flag = bytes.len();
        bytes.push(if version == 6 { 3 } else { 1 });
        if version == 1 {
            word(&mut bytes, 2);
        }
        word(&mut bytes, 4);
        if version >= 3 {
            bytes.push(20);
            bytes.extend([7; 20]);
        }
        word(&mut bytes, 1);
        let line = bytes.len();
        word(&mut bytes, 2);
        if version >= 4 {
            bytes.push(1);
            word(&mut bytes, 2);
            word(&mut bytes, 0);
        }
        if version >= 5 {
            bytes.push(0);
        }
        let end = bytes.len();
        Fixture {
            bytes,
            start,
            flag,
            line,
            end,
        }
    }

    impl Fixture {
        fn with_evidence(version: u8) -> Self {
            let mut fixture = catalog_row(version);
            word(&mut fixture.bytes, 5);
            word(&mut fixture.bytes, if version == 1 { 9 } else { 4 });
            fixture
        }
    }

    impl Fixture {
        fn catalog_range(&self, version: u8) -> std::ops::Range<u64> {
            if version == 6 { self.start as u64..self.end as u64 } else { 0..0 }
        }
    }

    #[test]
    fn catalog_versions_preserve_ranges_scores_and_evidence() {
        // The original format records absolute source and evidence ends.
        // Later layouts pack lengths, then add hashes, scores and reusable rows.
        for (version, source_start, hashed, scored) in [
            (1, 2, false, false),
            (2, 0, false, false),
            (3, 0, true, false),
            (4, 0, true, true),
            (5, 0, true, true),
            (6, 0, true, true),
        ] {
            let fixture = Fixture::with_evidence(version);
            let row = files(&fixture.bytes, 4).unwrap().pop().unwrap();
            assert_eq!(row.path(), "x");
            assert_eq!(row.bytes, Some(source_start..4));
            assert_eq!(row.line_ends, [2]);
            assert_eq!(row.evidence, 5..9);
            assert_eq!(row.score_marker, scored);
            assert_eq!(row.score.is_some(), scored);
            assert!(row.test_score.is_none());
            assert_eq!(row.blob.is_some(), hashed);
            assert_eq!(row.catalog_row, fixture.catalog_range(version));
        }
    }

    #[test]
    fn truncated_catalogs_accept_only_legacy_rows_without_evidence() {
        for version in 1..=6 {
            let fixture = Fixture::with_evidence(version);
            for cut in 0..fixture.bytes.len() {
                let row = files(&fixture.bytes[..cut], 4);
                if version == 1 && cut == fixture.end {
                    assert_eq!(row.unwrap()[0].evidence, 0..0);
                } else {
                    assert!(row.is_err(), "version {version} cut {cut}");
                }
            }
        }
    }

    #[test]
    fn catalog_rejects_invalid_flags_lines_and_extents() {
        for version in 1..=6 {
            let mut fixture = Fixture::with_evidence(version);
            for (offset, invalid) in [(fixture.flag, 2), (fixture.line, 5)] {
                let mut broken = fixture.bytes.clone();
                broken[offset] = invalid;
                assert!(files(&broken, 4).is_err());
            }
            assert!(files(&fixture.bytes, 3).is_err());
            fixture.bytes.push(0);
            assert!(files(&fixture.bytes, 4).is_err());
        }
    }
}

#[cfg(test)]
mod scope_tests {
    use super::scope_range;
    use std::{cell::Cell, path::Path};

    #[test]
    fn scope_matches_paths_by_component_and_skips_unrelated_prefix_neighbors() {
        let mut paths: Vec<_> = (0..4096).map(|i| format!("src-{i}/lib.rs")).collect();
        paths.extend(["src/a.rs", "src/sub/b.rs", "src0/c.rs", "é/main.rs"].map(String::from));
        paths.sort();
        for scope in ["", "sr", "src", "src/a.rs", "src/sub", "src/missing", "é"] {
            let probes = Cell::new(0);
            let range = scope_range(&paths, scope, |path| {
                probes.set(probes.get() + 1);
                path
            });
            let expected: Vec<_> = paths.iter().filter(|p| Path::new(p).starts_with(scope)).collect();
            assert!(paths[range].iter().eq(expected));
            assert!(probes.get() < 64, "scope {scope} read {} catalog keys", probes.get());
        }
    }
}
