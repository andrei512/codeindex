use super::*;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn writer_process() {
    if let Some(cwd) = env::var_os("CODEINDEX_TEST_WRITER_CWD") {
        Pin::refresh(Path::new(&cwd)).unwrap();
    }
}

struct Fixture {
    home: PathBuf,
    root: PathBuf,
    cache: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let home = env::temp_dir().join(format!(
            "codeindex-publication-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&home).unwrap();
        let root = home.join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        let root = root.canonicalize().unwrap();
        let cache = home
            .join(".codeindex/repos")
            .join(repository::cache_key(&root));
        fs::write(root.join("a.md"), "old").unwrap();
        Self { home, root, cache }
    }
    fn writer(&self, stage: Option<Publication>) -> Command {
        let mut cmd = Command::new(env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "adapters::snapshot::store::publication_tests::writer_process",
        ])
        .env("HOME", &self.home)
        .env("PATH", "")
        .env("CODEINDEX_TEST_WRITER_CWD", &self.root)
        .env_remove("CODEINDEX_TEST_CRASH_STAGE");
        if let Some(stage) = stage {
            cmd.env("CODEINDEX_TEST_CRASH_STAGE", format!("{stage:?}"));
        }
        cmd
    }
    fn pin(&self) -> Option<Pin> {
        Pin::open(&self.cache, &self.root).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

// Inspect the serialized selector map independently of the production range reader.
fn owners_of<'a>(spans: &'a [u8], range: &std::ops::Range<u64>) -> impl Iterator<Item = u64> + 'a {
    let range = range.clone();
    let mut start = 0;
    spans.chunks_exact(24).filter_map(move |row| {
        let end = u64::from_le_bytes(row[..8].try_into().unwrap());
        let overlaps = start < range.end && range.start < end;
        start = end;
        overlaps.then(|| u64::from_le_bytes(row[8..16].try_into().unwrap()))
    })
}

#[test]
fn private_cache_permissions_are_repaired_once() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Fixture::new();
    lease::prepare_cache(&repo.cache).unwrap();
    fs::set_permissions(&repo.cache, fs::Permissions::from_mode(0o755)).unwrap();
    lease::prepare_cache(&repo.cache).unwrap();
    let before = fs::metadata(&repo.cache).unwrap();
    assert_eq!(before.mode() & 0o7777, 0o700);
    lease::prepare_cache(&repo.cache).unwrap();
    let after = fs::metadata(&repo.cache).unwrap();
    assert_eq!((before.ctime(), before.ctime_nsec()), (after.ctime(), after.ctime_nsec()));
}

#[test]
fn an_external_include_change_refreshes_an_unchanged_checkout() {
    use crate::application::index::symbol_lookup::SymbolLookup;
    let repo = Fixture::new();
    let sdk = repo.home.canonicalize().unwrap().join("sdk");
    let first = sdk.join("first");
    let last = sdk.join("last");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&last).unwrap();
    let header = "profile.h";
    fs::write(last.join(header), "/* no project switch */\n").unwrap();
    fs::write(repo.root.join("main.c"),
        "#include <profile.h>\n#ifndef PROJECT_OPTION\nint available(void) { return 1; }\n#endif\n").unwrap();
    fs::write(repo.root.join("compile_commands.json"), format!(
        r#"[{{"directory":".","file":"main.c","arguments":["clang","-isysroot","{}","-isystem","{}","-isystem","{}","-c","main.c"]}}]"#,
        sdk.display(), first.display(), last.display())).unwrap();
    let check = |generation, present| {
        let output = repo.writer(None).env("CX_TEST_TRUSTED_SDK_ROOT", &sdk).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let pin = repo.pin().unwrap();
        assert_eq!(pin.generation, generation);
        let mut symbols = SymbolLookup::open(pin.reader(manifest::SYMBOLS).unwrap(), pin.files.len()).unwrap();
        let mut found = false;
        symbols.visit_named("available", 1, |_| { found = true; Ok(()) }).unwrap();
        assert_eq!(found, present, "generation {generation}");
        assert!(pin.files.iter().all(|f| !f.path().ends_with(header)));
    };
    check(1, true);
    check(1, true);
    // A formerly missing earlier include wins without touching the checkout.
    fs::write(first.join(header), "#define PROJECT_OPTION 1\n").unwrap();
    check(2, false);
    check(2, false);
    fs::remove_file(first.join(header)).unwrap();
    check(3, true);
    fs::write(last.join(header), "#define PROJECT_OPTION 1\n").unwrap();
    // A simultaneous metadata-only source edit still requires new SDK bindings.
    let main = repo.root.join("main.c");
    fs::write(&main, fs::read(&main).unwrap()).unwrap();
    check(4, false);
    check(4, false);
}

#[test]
fn markdown_record_patch_matches_fresh_bytes_after_moving_and_retargeting_links() {
    let repo = Fixture::new();
    let bodies = [
        "# Page\n\n[link](a.md#first)\n",
        "# Page\n\n\n[link](a.md#first)\n",
        "# Page\n\n\n[link](a.md#other)\n",
        "# Page\n\n\n[link](a.md#other)\n\n## Details\n",
    ];
    for body in bodies {
        let clean = Fixture::new();
        for fixture in [&repo, &clean] {
            fs::write(fixture.root.join("b.md"), body).unwrap();
            assert!(fixture.writer(None).output().unwrap().status.success());
        }
        assert_eq!(repo.pin().unwrap().product(manifest::MD).unwrap(),
                   clean.pin().unwrap().product(manifest::MD).unwrap());
    }
}

#[test]
fn mixed_markdown_edits_match_all_cold_products() {
    let repo = Fixture::new();
    for (step, (first, last)) in [
        ("# Same\n[x](a.md#x)\n", &b"[x](a.md#x)"[..]),
        ("# Same\n[x](a.md#x)\n", b"[x](a.md#x)"),
        ("# Move\n[x](a.md#x)\n", b"[x](a.md#x)"),
        ("# More\n[x](a.md#x) [x](a.md#x)\n", b"\0binary"),
        ("# Back\n[x](a.md#y)\n", b"[z](a.md#z) [x](a.md#x)"),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(repo.root.join("b.md"), first).unwrap();
        fs::write(repo.root.join("c.txt"), format!("edit {step}")).unwrap();
        fs::write(repo.root.join("d.md"), "# Unchanged\n").unwrap();
        fs::write(repo.root.join("e.txt"), format!("edit {step}")).unwrap();
        fs::write(repo.root.join("f.md"), last).unwrap();
        assert!(repo.writer(None).output().unwrap().status.success());
        let clean = Fixture::new();
        let built = clean
            .writer(None)
            .env("CODEINDEX_TEST_WRITER_CWD", &repo.root)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let fresh_cache = clean
            .home
            .join(".codeindex/repos")
            .join(repository::cache_key(&repo.root));
        let fresh = Pin::open(&fresh_cache, &repo.root).unwrap().unwrap();
        let next = repo.pin().unwrap();
        assert_eq!(next.generation, step as u64 + 1);
        for product in 1..=manifest::N {
            assert_eq!(
                next.product(product).unwrap(),
                fresh.product(product).unwrap(),
                "step {step}, product {product}"
            );
        }
    }
}

#[test]
fn rust_location_patch_matches_a_clean_graph_across_reopen_and_revert() {
    let repo = Fixture::new();
    let path = repo.root.join("lib.rs");
    let helpers: String = (0..160)
        .map(|i| format!("fn helper{i}() {{ helper0(); }}\n"))
        .collect();
    fs::write(repo.root.join("other.rs"), &helpers).unwrap();
    let original = "fn target() {}\nfn caller() { target(); }\n";
    fs::write(&path, original).unwrap();
    assert!(repo.writer(None).output().unwrap().status.success());
    let first = repo.pin().unwrap();
    let comment = format!("{original}// location only λ\n");
    for (text, location_only) in [
        (comment.as_str(), true),
        (original, true),
        ("fn target() {}\nfn caller() { target(); }\nfn added() {}\n", false),
        ("fn target() {}\nfn caller() { absent(); }\nfn added() {}\n", false),
    ] {
        fs::write(&path, text).unwrap();
        assert!(repo.writer(None).output().unwrap().status.success());
        let edited = repo.pin().unwrap();
        let clean = Fixture::new();
        fs::write(clean.root.join("other.rs"), &helpers).unwrap();
        fs::write(clean.root.join("lib.rs"), text).unwrap();
        assert!(clean.writer(None).output().unwrap().status.success());
        let expected = clean.pin().unwrap();
        assert_graph_patch(&edited, &expected, location_only);
    }
    assert_eq!(first.source(1).unwrap(), original);
}

fn assert_graph_patch(edited: &Pin, expected: &Pin, location_only: bool) {
    for product in [manifest::SYMBOLS, manifest::REL] {
        assert_eq!(edited.product(product).unwrap(), expected.product(product).unwrap());
    }
    if location_only {
        let rel = &edited.dir.0[manifest::REL - 1];
        assert!(owners_of(&edited.segment.map.encode(), rel).all(|key| key != edited.segment_id));
    }
}

#[test]
fn script_graph_reuse_preserves_names_coordinates_and_targets() {
    let repo = Fixture::new();
    let declarations: String = (0..160)
        .map(|n| format!("def helper{n}():\n    return 1\n"))
        .collect();
    fs::write(repo.root.join("helpers.py"), &declarations).unwrap();
    let path = repo.root.join("caller.py");
    let caller = "from helpers import helper1, helper2\nhelper1()\n";
    fs::write(&path, caller).unwrap();
    assert!(repo.writer(None).status().unwrap().success());
    let baseline = repo.pin().unwrap();
    let edit = format!("{caller}# new trailing comment\n");
    fs::write(&path, &edit).unwrap();
    assert!(repo.writer(None).status().unwrap().success());
    let next = repo.pin().unwrap();
    let spans = next.segment.map.encode();
    for product in [manifest::SYMBOLS, manifest::REL] {
        let range = &next.dir.0[product - 1];
        assert!(owners_of(&spans, range).all(|key| key != next.segment_id));
        assert_eq!(next.product(product).unwrap(), baseline.product(product).unwrap());
    }

    // Same-length call-site rename, then shifted coordinates. Both must
    // produce exactly the graph a new index would build for the edited tree.
    for changed in [edit.replace("helper1()", "helper2()"), format!("\n{caller}")] {
        fs::write(&path, &changed).unwrap();
        assert!(repo.writer(None).status().unwrap().success());
        let actual = repo.pin().unwrap();
        let fresh = Fixture::new();
        fs::write(fresh.root.join("helpers.py"), &declarations).unwrap();
        fs::write(fresh.root.join("caller.py"), &changed).unwrap();
        assert!(fresh.writer(None).status().unwrap().success());
        let expected = fresh.pin().unwrap();
        for product in [manifest::SYMBOLS, manifest::REL] {
            assert_eq!(actual.product(product).unwrap(), expected.product(product).unwrap());
        }
    }
}

#[test]
fn selective_migration_process() {
    let Some(root) = env::var_os("CX_TEST_MIGRATION_ROOT") else {
        return;
    };
    let id: u16 = env::var("CX_TEST_MIGRATION_RULE").unwrap().parse().unwrap();
    let cwd = Path::new(&root);
    let mut previous = Pin::refresh(cwd).unwrap();
    let old_keys: Vec<_> = previous.segment.keys().collect();
    let products: Vec<_> = (1..=manifest::N)
        .map(|p| previous.product(p).unwrap())
        .collect();
    previous.migration = super::super::registry::fixture_upgrade(id);
    let next = Pin::refresh_with_prior(cwd, Some(previous)).unwrap();
    assert_eq!(next.generation, 2);
    let affected = super::super::registry::fixture_upgrade(id);
    let spans = next.segment.map.encode();
    for kind in [
        manifest::SOURCE,
        manifest::PATHGRAM,
        manifest::CONTENT,
        manifest::RANK,
        manifest::SYMBOLS,
        manifest::REL,
        manifest::MD,
    ] {
        assert_eq!(
            next.product(kind).unwrap(),
            products[kind - 1],
            "fresh product {kind} for migration {id}"
        );
        let range = &next.dir.0[kind - 1];
        let seen_new = owners_of(&spans, range).any(|key| !old_keys.contains(&key));
        assert_eq!(
            seen_new,
            affected.requires_rebuild(kind),
            "physical ownership for rule {id}, product {kind}"
        );
    }
}

#[test]
fn producer_migrations_preserve_unrelated_product_bytes() {
    for rule in [3, 4, 6, 8] {
        let f = Fixture::new();
        fs::write(
            f.root.join("a.md"),
            "# Setup\n[other](b.md#help)\nbody needle",
        )
        .unwrap();
        fs::write(f.root.join("b.md"), "## Help\nneedle").unwrap();
        let code = (0..32)
            .map(|n| format!("fn caller{n}() {{ target(); }}\n"))
            .collect::<String>();
        fs::write(f.root.join("x.rs"), format!("fn target() {{}}\n{code}")).unwrap();
        for n in 0..16 {
            fs::write(
                f.root.join(format!("guide-{n}.md")),
                format!("# Chapter {n}\n[Setup](a.md#setup)"),
            )
            .unwrap();
        }
        let out = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "adapters::snapshot::store::publication_tests::selective_migration_process",
                "--nocapture",
            ])
            .env("HOME", &f.home)
            .env("PATH", "")
            .env("CX_TEST_MIGRATION_ROOT", &f.root)
            .env("CX_TEST_MIGRATION_RULE", rule.to_string())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn replaying_a_removed_marker_keeps_the_durable_edit() {
    let repo = Fixture::new();
    assert!(repo.writer(None).output().unwrap().status.success());
    fs::write(repo.root.join("a.md"), "edited").unwrap();
    assert!(repo.writer(None).output().unwrap().status.success());
    let generation = repo.pin().unwrap().generation;
    // A power loss may undo the final unlink after all data and retirements persisted.
    private(&repo.cache.join(PUBLISHING)).unwrap();
    assert!(repo.writer(None).output().unwrap().status.success());
    let recovered = repo.pin().unwrap();
    assert_eq!(recovered.generation, generation);
    assert_eq!(recovered.source(0).unwrap(), "edited");
    assert!(!repo.cache.join(PUBLISHING).exists());
}

#[test]
fn recovery_preserves_all_referenced_parts_across_reader_generations() {
    let f = Fixture::new();
    let body = "retained source and evidence\n".repeat(1024);
    fs::write(f.root.join("b.txt"), &body).unwrap();
    assert!(f.writer(None).output().unwrap().status.success());
    let first = f.pin().unwrap();
    fs::write(f.root.join("a.md"), "new text").unwrap();
    assert_eq!(
        f.writer(Some(Publication::Replaced))
            .output()
            .unwrap()
            .status
            .code(),
        Some(86)
    );
    assert!(f.writer(None).output().unwrap().status.success());
    let second = f.pin().unwrap();
    assert!(second.segment.keys().count() > 1);
    assert_eq!(second.source(1).unwrap(), body);
    fs::remove_file(f.root.join("b.txt")).unwrap();
    assert!(f.writer(None).output().unwrap().status.success());
    let latest = f.pin().unwrap();
    assert_eq!(latest.files.len(), 1);
    assert_eq!(first.source(0).unwrap(), "old");
    assert_eq!(second.source(1).unwrap(), body);
}

#[test]
fn checkpoint_promotes_identical_committed_bytes_without_a_product_write() {
    let repo = Fixture::new();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&repo.root)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
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
    git(&["init", "-q"]);
    let write = |text| fs::write(repo.root.join("a.md"), text).unwrap();
    let refresh = || {
        assert!(repo.writer(None).status().unwrap().success());
        repo.pin().unwrap()
    };
    write("version A");
    git(&["add", "a.md"]);
    git(&["commit", "-qm", "A"]);
    assert_eq!(refresh().committed_files(), 1);
    write("version B");
    git(&["add", "a.md"]);
    write("version C");
    let dirty = refresh();
    assert_eq!(dirty.source(0).unwrap(), "version C");
    assert_eq!(dirty.committed_files(), 0);
    git(&["add", "a.md"]);
    git(&["commit", "-qm", "C"]);
    let promoted = refresh();
    assert_eq!(promoted.committed_files(), 1);
    assert_eq!(promoted.product_generation(), dirty.product_generation());
    write("version C");
    assert_eq!(refresh().product_generation(), dirty.product_generation());
}

#[test]
fn metadata_only_crash_keeps_shared_segment_and_old_pin() {
    for stage in [
        Publication::SegmentDurable,
        Publication::ManifestDurable,
        Publication::Replaced,
        Publication::SelectorDurable,
    ] {
        let fixture = Fixture::new();
        assert!(fixture.writer(None).output().unwrap().status.success());
        let old = fixture.pin().unwrap();
        // Rename an identical file over the original so the stamp must change.
        fs::write(fixture.root.join("replacement"), "old").unwrap();
        fs::rename(fixture.root.join("replacement"), fixture.root.join("a.md")).unwrap();
        assert_eq!(
            fixture.writer(Some(stage)).output().unwrap().status.code(),
            Some(86)
        );
        assert!(fixture.writer(None).output().unwrap().status.success());
        let promoted = fixture.pin().unwrap();
        assert_eq!((promoted.generation, promoted.segment_id), (2, 1));
        assert_eq!(promoted.source(0).unwrap(), "old");
        fs::write(fixture.root.join("a.md"), "new").unwrap();
        assert!(fixture.writer(None).output().unwrap().status.success());
        assert!(!fixture.cache.join(segment_name(1)).exists());
        assert_eq!(old.source(0).unwrap(), "old");
        assert_eq!(promoted.source(0).unwrap(), "old");
    }
}

#[test]
fn crash_before_and_after_switch_with_cold_and_existing_cache() {
    for had_old in [false, true] {
        for stage in [
            Publication::SegmentDurable,
            Publication::ManifestDurable,
            Publication::Replaced,
            Publication::SelectorDurable,
        ] {
            let f = Fixture::new();
            if had_old {
                assert!(f.writer(None).output().unwrap().status.success());
            }
            let old = f.pin();
            fs::write(f.root.join("a.md"), "new document").unwrap();
            let failed = f.writer(Some(stage)).output().unwrap();
            assert_eq!(
                failed.status.code(),
                Some(86),
                "{}",
                String::from_utf8_lossy(&failed.stderr)
            );
            assert!(f.cache.join(PUBLISHING).exists());
            let switched =
                matches!(stage, Publication::Replaced | Publication::SelectorDurable);
            let selected = f.pin();
            if switched {
                assert_eq!(selected.unwrap().source(0).unwrap(), "new document")
            } else if had_old {
                assert_eq!(selected.unwrap().source(0).unwrap(), "old")
            } else {
                assert!(selected.is_none())
            }
            // Two competing recoveries must converge on a single complete successor.
            let first = f
                .writer(None)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let second = f
                .writer(None)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            assert!(first.wait_with_output().unwrap().status.success());
            assert!(second.wait_with_output().unwrap().status.success());
            let new = f.pin().unwrap();
            assert_eq!(new.generation, if had_old { 2 } else { 1 });
            assert_eq!(new.source(0).unwrap(), "new document");
            let mut files: Vec<_> = fs::read_dir(&f.cache)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            files.sort();
            let mut expected = vec!["current".to_owned(), "refresh.lock".to_owned()];
            expected.extend(new.segment.keys().map(pieces::name));
            expected.sort();
            assert_eq!(files, expected);
            if let Some(pin) = old {
                assert_eq!(pin.source(0).unwrap(), "old");
            }
        }
    }
}
