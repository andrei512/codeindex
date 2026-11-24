use super::*;
use std::{
    os::unix::fs::symlink,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let path = loop {
            let path = env::temp_dir().join(format!(
                "codeindex-input-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("fixture directory: {e}"),
            }
        };
        Self(path.canonicalize().unwrap())
    }
    fn put(&self, path: &str, bytes: &[u8]) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn v2_index(paths: &[&str]) -> Vec<u8> {
    let mut data = b"DIRC\x00\x00\x00\x02".to_vec();
    data.extend_from_slice(&(paths.len() as u32).to_be_bytes());
    for path in paths {
        let start = data.len();
        data.resize(start + 60, 0);
        data[start + 24..start + 28].copy_from_slice(&0o100644u32.to_be_bytes());
        data.extend_from_slice(&(path.len() as u16).to_be_bytes());
        data.extend_from_slice(path.as_bytes());
        data.push(0);
        data.resize(start + (data.len() - start).next_multiple_of(8), 0);
    }
    data.resize(data.len() + 20, 0);
    data
}

#[test]
fn linked_worktree_tracks_ignored_files_and_uses_common_excludes() {
    let f = Fixture::new();
    f.put("checkout/.git", b"gitdir: ../common/worktrees/a\n");
    f.put("common/worktrees/a/commondir", b"../..\n");
    f.put("common/worktrees/a/index", &v2_index(&["drop/keep"]));
    f.put("global-rules", b"*.global\n");
    f.put(
        "common/config",
        format!(
            "[core]\nexcludesFile = \"{}\"\n",
            f.0.join("global-rules").display()
        )
        .as_bytes(),
    );
    f.put("common/info/exclude", b"*.local\n");
    f.put(
        "checkout/.gitignore",
        b"drop/\n/root.txt\na/**/out?.[ch]\n\\!literal\nspace\\ \n",
    );
    f.put("checkout/a/.gitignore", b"!keep.global\n");
    for path in [
        "drop/keep",
        "drop/other",
        "notes",
        "root.txt",
        "a/root.txt",
        "a/out1.c",
        "a/z/out2.h",
        "a/keep.global",
        "a/no.global",
        "a/no.local",
        "!literal",
        "space ",
    ] {
        f.put(&format!("checkout/{path}"), b"working bytes");
    }
    symlink("missing", f.0.join("checkout/link")).unwrap();
    symlink("../common", f.0.join("checkout/dir-link")).unwrap();
    let (root, found) = inventory(&f.0.join("checkout/a")).unwrap();
    assert_eq!(
        found,
        [
            ".gitignore",
            "a/.gitignore",
            "a/keep.global",
            "a/root.txt",
            "dir-link",
            "drop/keep",
            "link",
            "notes"
        ]
    );
    assert_eq!(
        capture(&root, "drop/keep").unwrap().as_deref(),
        Some("working bytes")
    );
    assert!(capture(&root, "link").unwrap().is_none());
    assert!(capture(&root, "dir-link/config").is_err());
    assert!(capture(&root, "../common/config").is_err());
    assert_eq!(locate(&root.join("a")).unwrap(), root);
}

#[test]
fn conditional_config_and_observed_control_version() {
    let f = Fixture::new();
    f.put(".git/config",format!("[includeIf \"gitdir:{}/.git\"]\npath = branch\n[remote \"origin\"]\nurl = https://example.org/parent/repo\n",f.0.display()).as_bytes());
    f.put(".git/HEAD", b"ref: refs/heads/feature/topic\n");
    f.put(
        ".git/branch",
        b"[includeIf \"onbranch:feature/\"]\npath = conditional\n",
    );
    f.put(
        ".git/conditional",
        b"[includeIf \"hasconfig:remote.*.url:https://example.org/**\"]\npath = matched\n",
    );
    f.put(
        ".git/matched",
        format!(
            "[core]\nexcludesFile = {}\n",
            f.0.join(".git/rules").display()
        )
        .as_bytes(),
    );
    f.put(".git/rules", b"should-ignore\n");
    f.put(".gitignore", b"/cache/\n");
    f.put("should-ignore", b"absent");
    f.put("visible", b"initial");
    let observed = observe(&f.0).unwrap();
    assert_eq!(
        observed.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
        [".gitignore", "visible"]
    );
    assert_eq!(
        observed[0].captured.as_ref().unwrap().as_deref(),
        Some("/cache/\n")
    );
    let before = observed[1].stamp;
    f.put("visible", b"updated and longer");
    assert!(capture_observed(&f.0, "visible", before).is_err());
}

#[test]
fn sparse_index_reads_only_materialized_trees() {
    let f = Fixture::new();
    f.put(".gitignore", b"*.ignore\n");
    let mut idx = v2_index(&["out/"]);
    idx[12 + 24..12 + 28].copy_from_slice(&0o040000u32.to_be_bytes());
    idx[52..72].fill(0xab);
    f.put(".git/index", &idx);
    assert_eq!(inventory(&f.0).unwrap().1, [".gitignore"]);
    let tree = [
        120, 156, 43, 41, 74, 77, 85, 48, 182, 100, 48, 52, 48, 48, 51, 49, 81, 200, 78, 45, 40,
        209, 203, 76, 207, 203, 47, 74, 101, 48, 192, 2, 0, 49, 39, 11, 178,
    ];
    f.put(&format!(".git/objects/ab/{}", "ab".repeat(19)), &tree);
    f.put("out/kept.ignore", b"working copy");
    f.put("out/extra.ignore", b"untracked");
    assert_eq!(
        inventory(&f.0).unwrap().1,
        [".gitignore", "out/kept.ignore"]
    );
}

#[test]
fn blob_hashes_include_the_git_header_and_support_both_object_formats() {
    for (length, sha1, sha256) in [
        (
            0,
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
            "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813",
        ),
        (
            48,
            "3b3abca28de4b8ce6ea9dac5da512120ed7c5a0c",
            "8fc63468a87c9f8894b375f6ed954d63c39a84ea262a0e4189815ff45ca47152",
        ),
        (
            49,
            "48c294fdd488d5f0ac50290c1d1752841cb6bd35",
            "a30154d7bd76104132da6574e19aa7fa9f0e4ba984b3aac09d0709aa442a1fc5",
        ),
        (
            56,
            "355e203863c368f0404d7fc7ae4761122c117dd2",
            "62ecade9a3c81d7186d91e112660a28c2e8ad9b156f296fcff4960feadc91df1",
        ),
        (
            2048,
            "1e1744b1e5f114cdd5604229ec085e6457014df6",
            "ad1e36f2555d0eb62740b245e93734c70b1d4c72948b751ad6710c0ae9c47a93",
        ),
    ] {
        let bytes = vec![b'x'; length];
        for (algorithm, expected) in [(false, sha1), (true, sha256)] {
            assert_eq!(hex(raw_blob_id(&bytes, algorithm).as_bytes()), expected);
        }
    }
}

#[test]
fn commit_lookup_uses_pinned_commit_in_loose_and_packed_stores() {
    use std::process::Command;
    for format in ["sha1", "sha256"] {
        let f = Fixture::new();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(&f.0)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.org",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet", &format!("--object-format={format}")]);
        f.put("README.md", b"root file");
        f.put("nested/file.rs", b"committed\n");
        f.put("stable/deep/file.txt", b"identical in both commits");
        symlink("file.rs", f.0.join("nested/link")).unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "first version"]);
        let head = head_commit(&f.0).unwrap().unwrap();
        f.put("nested/file.rs", b"staged\n");
        git(&["add", "."]);
        f.put("nested/file.rs", b"working\n");
        for packed in [false, true] {
            if packed {
                git(&["gc", "--quiet"]);
            }
            let mut tree = CommitTree::open(&f.0, &head).unwrap();
            assert_eq!(uses_sha256(&f.0).unwrap(), format == "sha256");
            assert_eq!(
                tree.blob("nested/file.rs").unwrap(),
                Some(raw_blob_id(b"committed\n", format == "sha256"))
            );
            assert!(tree.blob("README.md").unwrap().is_some());
            for absent in ["nested", "nested/link", "missing", "/README.md", "/nested/file.rs", "nested/file.rs/child"] {
                assert_eq!(tree.blob(absent).unwrap(), None);
            }
        }
        fs::remove_dir_all(f.0.join("nested")).unwrap();
        f.put("nested", b"new file replacing directory");
        f.put("new/subdir/add", b"added");
        git(&["add", "-A"]);
        git(&["commit", "-qm", "replace directory"]);
        let next = head_commit(&f.0).unwrap().unwrap();
        git(&["gc", "--quiet"]);
        let mut latest = CommitTree::open(&f.0, &next).unwrap();
        assert_eq!(
            latest.changed_paths(&head).unwrap(),
            std::collections::HashMap::from([
                (
                    "nested".into(),
                    Some(raw_blob_id(
                        b"new file replacing directory",
                        format == "sha256"
                    ))
                ),
                ("nested/file.rs".into(), None),
                (
                    "new/subdir/add".into(),
                    Some(raw_blob_id(b"added", format == "sha256"))
                ),
            ])
        );
        assert!(latest.changed_paths(&next).unwrap().is_empty());
    }
}

#[test]
fn capture_root_rejects_stale_replacements_and_symlinked_ancestors() {
    let fixture = Fixture::new();
    fixture.put("sub/file", b"original");
    let root = CaptureRoot::new(&fixture.0).unwrap();
    let expected = capture::stamp(&fs::symlink_metadata(fixture.0.join("sub/file")).unwrap());
    assert_eq!(root.capture("sub/file", expected).unwrap().unwrap(), "original");

    // Even a symlink to the *original* directory must not be traversed.
    fs::rename(fixture.0.join("sub"), fixture.0.join("moved")).unwrap();
    symlink("moved", fixture.0.join("sub")).unwrap();
    assert!(root.capture("sub/file", expected).is_err());

    fs::remove_file(fixture.0.join("sub")).unwrap();
    fixture.put("sub/file", b"original");
    assert!(root.capture("sub/file", expected).is_err());
    assert!(root.capture("../sub/file", expected).is_err());

    symlink("sub/file", fixture.0.join("link")).unwrap();
    let link_stamp = capture::stamp(&fs::symlink_metadata(fixture.0.join("link")).unwrap());
    assert!(root.capture("link", link_stamp).unwrap().is_none());
    assert!(root.capture("link", expected).is_err());
}

#[test]
fn parallel_directories_inherit_only_ancestor_rules_and_keep_tracked_exceptions() {
    let f = Fixture::new();
    let padding: Vec<_> = (0..256).map(|i| format!("unused/{i:03}")).collect();
    let mut tracked = vec!["component", "hidden/tracked.tmp"];
    tracked.extend(padding.iter().map(String::as_str));
    let mut idx = v2_index(&tracked);
    idx[12 + 24..12 + 28].copy_from_slice(&0o160000u32.to_be_bytes());
    f.put(".git/index", &idx);
    f.put(".gitignore", b"*.tmp\nhidden/\n");
    f.put("component/untracked", b"");
    f.put("nested/.git/config", b"");
    f.put("nested/file", b"");
    f.put("hidden/tracked.tmp", b"");
    f.put("hidden/.gitignore", b"!untracked.tmp\n");
    f.put("hidden/untracked.tmp", b"");
    let mut expected = vec![".gitignore".to_owned(), "hidden/tracked.tmp".to_owned()];
    for n in 0..12 {
        let base = format!("branch-{n:02}/deep/nested");
        f.put(&format!("{base}/visible"), b"");
        f.put(&format!("{base}/child/keep.tmp"), b"");
        expected.push(format!("{base}/visible"));
        if n % 2 == 0 {
            f.put(&format!("{base}/.gitignore"), b"!keep.tmp\n");
            expected.push(format!("{base}/.gitignore"));
            expected.push(format!("{base}/child/keep.tmp"));
        }
    }
    expected.sort_unstable();
    let got = observe(&f.0).unwrap();
    for path in &got {
        if path.path.ends_with("/.gitignore") {
            assert_eq!(path.captured.as_ref().unwrap().as_deref(), Some("!keep.tmp\n"));
        }
    }
    assert_eq!(got.into_iter().map(|p| p.path).collect::<Vec<_>>(), expected);
}
