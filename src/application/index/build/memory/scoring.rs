//! Plain-folder scores release each file after measurement. Only sources with
//! policy markers are retained for reporting. Configured C translation units
//! use the shared capture instead because include paths can name any file.
use super::*;

pub(super) struct Request<'a> {
    pub rows: std::ops::Range<usize>,
    pub test_rules: bool,
    pub inline: bool,
    pub policy: &'a code::score::policy::Project,
}

struct ProjectInput {
    file: ObservedPath,
    capture: std::sync::OnceLock<Option<String>>,
}

impl ProjectInput {
    fn text(&self, read: &impl Fn(&str, [u64; 7]) -> Option<String>) -> Option<&str> {
        match &self.file.captured {
            Some(body) => body.as_deref(),
            None => self
                .capture
                .get_or_init(|| read(&self.file.path, self.file.stamp))
                .as_deref(),
        }
    }
}

fn scorable(path: &str) -> bool {
    Language::for_path(path).is_some() || markdown::is_markdown(path) || path.ends_with(".t")
}

pub(super) fn selected_rows(paths: &[ObservedPath], scope: &str) -> std::ops::Range<usize> {
    crate::application::index::scope_range(paths, scope, |file| &file.path)
}

impl Request<'_> {
    pub fn needs_project(&self, paths: &[ObservedPath]) -> bool {
        paths.iter().any(|o| c_project::database(&o.path))
            && paths[self.rows.clone()].iter().any(|o| code::CProject::recognizes(&o.path))
    }

    pub fn measure(
        &self,
        path: &str,
        text: &str,
        project: Option<&code::CProject>,
    ) -> code::score::Saved {
        let policy = self.policy.resolve(path);
        let measure = || code::score::source(path, text, project, self.test_rules);
        if policy.needs_measurement(None) {
            policy.measure(measure)
        } else {
            measure()
        }
    }

    pub fn with_project(
        self,
        root: &Path,
        paths: Vec<ObservedPath>,
        capture: impl Fn(&str, [u64; 7]) -> io::Result<Option<String>> + Sync,
    ) -> io::Result<(Vec<Entry>, Directory, Vec<u8>)> {
        // A C include can name any observed file. Open it when requested and
        // retain its bytes for later translation units and file scoring.
        let inputs: Vec<_> = paths
            .into_iter()
            .map(|file| ProjectInput {
                file,
                capture: Default::default(),
            })
            .collect();
        // The resolver represents missing headers with None. Preserve the
        // first I/O failure so a partial capture cannot yield a score.
        let failure = std::sync::OnceLock::new();
        let read = |path: &str, stamp| {
            capture(path, stamp).unwrap_or_else(|error| {
                let _ = failure.set(error);
                None
            })
        };
        let project = code::CProject::new(
            root,
            inputs
                .iter()
                .filter(|i| c_project::database(&i.file.path))
                .map(|i| (i.file.path.as_str(), i.text(&read).unwrap_or(""))),
            |name| {
                let id = inputs
                    .binary_search_by(|i| i.file.path.as_str().cmp(name))
                    .ok()?;
                let input = &inputs[id];
                Some((input.file.path.as_str(), input.text(&read).unwrap_or("")))
            },
        );
        let mut rows = Vec::new();
        let mut marked = Vec::new();
        for input in &inputs[self.rows.clone()] {
            let file = &input.file;
            let mut row = entry(file.path.clone(), file.stamp, None);
            if scorable(&file.path)
                && let Some(text) = input.text(&read)
            {
                self.score_row(&mut row, text, Some(&project), &mut marked);
            }
            rows.push(row);
        }
        drop(project);
        if let Some(error) = failure.into_inner() {
            return Err(error);
        }
        Ok(scored_rows(rows, marked))
    }

    fn score_row(
        &self,
        row: &mut Entry,
        text: &str,
        project: Option<&code::CProject>,
        marked: &mut Vec<u8>,
    ) {
        row.score = self.measure(row.path(), text, project);
        if self.inline && text.contains(code::score::policy::MARKER) {
            let start = marked.len();
            marked.extend_from_slice(text.as_bytes());
            row.bytes = Some(start as u64..marked.len() as u64);
            row.score_marker = true;
        }
    }

    pub fn local(
        self,
        paths: Vec<ObservedPath>,
        capture: impl Fn(&str, [u64; 7]) -> io::Result<Option<String>>,
    ) -> io::Result<(Vec<Entry>, Directory, Vec<u8>)> {
        let mut rows = Vec::new();
        let mut marked = Vec::new();
        for file in paths.into_iter().skip(self.rows.start).take(self.rows.len()) {
            let mut row = entry(file.path, file.stamp, None);
            let path = row.path();
            if scorable(path) {
                let text = match file.captured {
                    Some(text) => text,
                    None => capture(path, file.stamp)?,
                };
                if let Some(text) = text {
                    self.score_row(&mut row, &text, None, &mut marked);
                }
            }
            rows.push(row);
        }
        Ok(scored_rows(rows, marked))
    }
}

fn scored_rows(rows: Vec<Entry>, marked: Vec<u8>) -> (Vec<Entry>, Directory, Vec<u8>) {
    let mut dir = Directory::default();
    dir.0[SOURCE - 1] = 0..marked.len() as u64;
    (rows, dir, marked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_c_score_reads_shared_headers_once_and_propagates_capture_errors() {
        check_shared_header(false);
        check_shared_header(true);
    }

    fn check_shared_header(fail: bool) {
        use std::collections::HashMap;
        use std::sync::Mutex;
        let policy = code::score::policy::Project::default();
        let request = |paths: &[ObservedPath]| Request {
            rows: selected_rows(paths, "src"),
            test_rules: true,
            inline: true,
            policy: &policy,
        };
        let paths = || {
            [
                "compile_commands.json",
                "data/unrelated.txt",
                "include/shared.h",
                "src/a.c",
                "src/b.c",
            ]
            .map(|p| ObservedPath {
                path: p.into(),
                stamp: [0; 7],
                captured: None,
            })
            .into()
        };
        let reads = Mutex::new(HashMap::new());
        let (source, header) = (
            "#include \"shared.h\"\nint f(void) { return VALUE; }",
            "#define VALUE 3",
        );
        let paths: Vec<_> = paths();
        let result = request(&paths).with_project(Path::new("/project"), paths, |p, _| {
            *reads.lock().unwrap().entry(p.to_owned()).or_insert(0) += 1;
            capture_fixture(p, source, header, fail)
        });
        if fail {
            assert_eq!(result.err().unwrap().to_string(), "header changed");
        } else {
            let (rows, _, bytes) = result.unwrap();
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().all(|r| r.score.is_some()));
            assert!(bytes.is_empty());
        }
        let reads = reads.into_inner().unwrap();
        assert_eq!(reads.len(), 4);
        assert!(reads.values().all(|&count| count == 1));
    }

    fn capture_fixture(path: &str, source: &str, header: &str, fail: bool) -> io::Result<Option<String>> {
        let text = match path {
            "compile_commands.json" => r#"[{"directory":"/project","file":"src/a.c","arguments":["clang","-Iinclude","src/a.c"]},{"directory":"/project","file":"src/b.c","arguments":["clang","-Iinclude","src/b.c"]}]"#,
            "src/a.c" | "src/b.c" => source,
            "include/shared.h" if fail => return Err(io::Error::other("header changed")),
            "include/shared.h" => header,
            _ => panic!("unrequested capture: {path}"),
        };
        Ok(Some(text.into()))
    }

    #[test]
    fn scoped_score_opens_only_required_files_and_retains_only_markers() {
        let policy = code::score::policy::Project::default();
        let paths = [
            "compile_commands.json",
            "elsewhere/header.inc",
            "src/empty.txt",
            "src/f.rs",
            "src/g.rs",
        ]
        .map(|path| ObservedPath {
            path: path.into(),
            stamp: [0; 7],
            captured: None,
        });
        let request = Request {
            rows: selected_rows(&paths, "src"),
            test_rules: true,
            inline: true,
            policy: &policy,
        };
        assert!(!request.needs_project(&paths));
        let marked = "// cx-score: ignore-file all -- generated\nfn g() {}\n";
        let (rows, _, bytes) = request
            .local(paths.into(), |path, _| {
                Ok(Some(
                    match path {
                        "src/f.rs" => "fn f() {}",
                        "src/g.rs" => marked,
                        _ => panic!("unexpected read of {path}"),
                    }
                    .into(),
                ))
            })
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows[1].score.is_some() && rows[1].bytes.is_none());
        assert!(rows[2].score_marker);
        assert_eq!(bytes, marked.as_bytes());
    }
}
