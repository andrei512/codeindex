use super::{tests::MemorySink, *};
use crate::domain::code::Names;
use std::{ops::Range, sync::Mutex};

struct Generation {
    files: Vec<Entry>,
    wire: MemorySink,
    dir: Directory,
    copied: Mutex<Vec<Range<u64>>>,
}
impl Generation {
    fn from_wire(dir: Directory, wire: MemorySink) -> Self {
        let source_extent = &dir.0[SOURCE - 1];
        let file_extent = &dir.0[FILES - 1];
        let files = crate::application::index::files(
            &wire.0[file_extent.start as usize..file_extent.end as usize],
            source_extent.end - source_extent.start,
        ).unwrap();
        Self { files, wire, dir, copied: Mutex::new(Vec::new()) }
    }

    fn bytes(&self, range: Range<u64>) -> &[u8] {
        &self.wire.0[range.start as usize..range.end as usize]
    }
}

impl Previous for Generation {
    fn entries(&self) -> &[Entry] {
        &self.files
    }
    fn extent(&self, product: usize) -> Range<u64> {
        self.dir.0[product - 1].clone()
    }
    fn same_bytes(&self, range: Range<u64>, expected: &[u8]) -> io::Result<bool> {
        self.copied.lock().unwrap().push(range.clone());
        Ok(self.bytes(range) == expected)
    }
    fn copy_bytes(&self, range: Range<u64>, out: &mut dyn io::Write) -> io::Result<()> {
        self.copied.lock().unwrap().push(range.clone());
        out.write_all(self.bytes(range))
    }
    fn product(&self, product: usize) -> io::Result<Vec<u8>> {
        Ok(self.bytes(self.extent(product)).to_vec())
    }
    fn local(&self, id: usize, decode_code: bool, _decode_markdown: bool) -> io::Result<Local> {
        assert!(
            !decode_code,
            "Markdown edit decoded retained language evidence"
        );
        let file = &self.files[id];
        assert!(!markdown::is_markdown(file.path()));
        Ok(Local {
            score: file.score.clone(),
            test_score: file.test_score.clone(),
            code: Analysis::default(),
            line_ends: file.line_ends.clone(),
            byte_len: file
                .bytes
                .as_ref()
                .map_or(0, |r| (r.end - r.start) as usize),
            markdown: None,
        })
    }
}

fn markdown_generation(
    old: Option<&Generation>,
    edit: bool,
    touched: bool,
    raced: bool,
) -> io::Result<Generation> {
    let body = if edit {
        "# New\nneedle\n"
    } else {
        "# Old\nquote\n"
    };
    let contents = [
        ("a.ts", "export function work() {}\n"),
        ("b.ts", "import {work} from './a'; work();\n"),
        ("c.md", body),
    ];
    let mut obs: Vec<_> = contents.into_iter()
        .map(|(path, text)| markdown_observation(path, text, (edit, touched)))
        .collect();
    let read = |_: usize, file: &ObservedPath, _: Option<usize>| {
        let path = file.path.as_str();
        assert!(
            old.is_none() || path == "c.md",
            "read unchanged body {path}"
        );
        Ok(Some(
            contents
                .iter()
                .find(|(name, _)| *name == path)
                .unwrap()
                .1
                .to_owned(),
        ))
    };
    let mut wire = MemorySink::default();
    let Output { dir, .. } = Inputs {
        root: Path::new("."),
        read: &read,
        predecessor: Predecessor {
            previous: old.map(|g| g as &dyn Previous),
            forward_known: true,
            verified_prefix: usize::from(old.is_some() && touched),
        },
        identify: &|slot, _, text, id| {
            Ok((
                if raced && slot == 0 {
                    None
                } else {
                    id.unchanged()
                },
                id.unchanged().and_then(|i| old?.files[i].blob).or_else(|| {
                    text.map(|s| crate::adapters::repository::raw_blob_id(s.as_bytes(), false))
                }),
            ))
        },
    }
    .with_allowance(&mut obs, &mut wire, Lanes::default(), 1000)?;
    Ok(Generation::from_wire(dir, wire))
}

fn markdown_observation(path: &str, text: &str, (edit, touched): (bool, bool)) -> ObservedPath {
    let mut stamp = [0; 7];
    stamp[2] = text.len() as u64;
    stamp[3] = u64::from(match path {
        "c.md" => edit,
        "a.ts" => touched,
        _ => false,
    });
    ObservedPath { path: path.into(), stamp, captured: None }
}

#[test]
fn markdown_edit_forwards_sources_and_evidence_and_borrows_graph() {
    let first = markdown_generation(None, false, false, false).unwrap();
    // The observer predicted identical binding inputs. Simulate the identity
    // provider invalidating that exact row while capture forwards its old facts.
    let raced = markdown_generation(Some(&first), true, false, true);
    assert_eq!(
        raced.err().unwrap().to_string(),
        "a verified binding input changed during capture"
    );
    for touched in [false, true] {
        let next = markdown_generation(Some(&first), true, touched, false).unwrap();
        let fresh = markdown_generation(None, true, touched, false).unwrap();
        assert_eq!(next.wire.0, fresh.wire.0);
    }
    let copied = first.copied.into_inner().unwrap();
    assert!(copied.contains(&first.dir.0[SYMBOLS - 1]));
    assert!(copied.contains(&first.dir.0[REL - 1]));
    assert!(copied.contains(&(first.files[0].evidence.start..first.files[1].evidence.end)));
}

#[test]
fn binder_reads_source_and_project_configuration_bytes() {
    for path in [
        "main.ts",
        "main.rs",
        "sub/tsconfig.json",
        "Cargo.toml",
        "sub/Cargo.toml",
        "sub/.cargo/config.toml",
        ".cargo/config",
        "package.json",
        "sub/package.json",
        "pyproject.toml",
    ] {
        assert!(Names::consumes_text(path), "{path}");
    }
    for path in [
        "guide.md",
        "logo.png",
        "sub/config.toml",
        "sub/pyproject.toml",
        "jsconfig.json",
        "pyrightconfig.json",
    ] {
        assert!(!Names::consumes_text(path), "{path}");
    }
}
