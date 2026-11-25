use crate::domain::code::Names;
use super::{tests::MemorySink, *};
use std::{ops::Range, sync::Mutex};

struct Generation {
    files: Vec<Entry>,
    wire: MemorySink,
    dir: Directory,
    copied: Mutex<Vec<Range<u64>>>,
}
impl Generation {
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
    fn copy_bytes(&self, range: Range<u64>, out: &mut dyn io::Write) -> io::Result<()> {
        self.copied.lock().unwrap().push(range.clone());
        out.write_all(self.bytes(range))
    }
    fn product(&self, product: usize) -> io::Result<Vec<u8>> {
        Ok(self.bytes(self.extent(product)).to_vec())
    }
    fn local(&self, id: usize, decode_code: bool,_decode_markdown:bool) -> io::Result<Local> {
        assert!(
            !decode_code,
            "Markdown edit decoded retained language evidence"
        );
        let file = &self.files[id];
        assert!(!markdown::is_markdown(file.path()));
        Ok(Local {
            score: file.score.clone(),
            code: Analysis::default(),
            lf: file.line_ends.clone(),
            partial_line: false,
            markdown: None,
        })
    }
}

#[test]
fn markdown_edit_forwards_sources_and_evidence_and_borrows_graph() {
    let make = |old: Option<&Generation>, edit: bool| {
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
        let mut obs: Vec<_> = contents
            .iter()
            .map(|(path, text)| {
                let mut stamp = [0; 7];
                stamp[2] = text.len() as u64;
                stamp[3] = u64::from(edit && *path == "c.md");
                ObservedPath {
                    path: (*path).into(),
                    stamp,
                    captured: None,
                }
            })
            .collect();
        let read = |_: &Path, path: &str, _: [u64; 7]| {
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
        let Output {dir, ..} = build_with_allowance(
            Path::new("."),
            &mut obs,
            &read,
            old.map(|g| g as &dyn Previous),
            true,
            &|_, _, text, id| Ok((id,id.and_then(|i|old?.files[i].blob).or_else(||text.map(|s|crate::adapters::repository::raw_blob_id(s.as_bytes(), false))))),
            &mut wire,
            1000,
            None,
            None,
        )
        .unwrap();
        let file_extent = dir.0[FILES - 1].clone();
        let source_extent = dir.0[SOURCE - 1].clone();
        let files = crate::application::index::files(
            &wire.0[file_extent.start as usize..file_extent.end as usize],
            source_extent.end - source_extent.start,
        )
        .unwrap();
        Generation {
            files,
            wire,
            dir,
            copied: Mutex::new(Vec::new()),
        }
    };
    let first = make(None, false);
    let next = make(Some(&first), true);
    let fresh = make(None, true);
    assert_eq!(next.wire.0, fresh.wire.0);
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
