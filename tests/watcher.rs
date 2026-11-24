//! The native publisher must do useful work while a client is absent, and a
//! query starting immediately after a write must also see that write.
#![cfg(target_os = "macos")]

use std::{
    fs::{self, File},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cx(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_cx"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn scheduled_publication_and_strict_nested_queries() {
    let home = Fixture(std::env::temp_dir().join(format!("cx-watch-test-{}", std::process::id())));
    fs::create_dir(&home.0).unwrap();
    let root = home.0.join("outer");
    fs::create_dir_all(root.join(".git")).unwrap();
    let path = root.join("x.txt");
    fs::write(&path, "first").unwrap();
    assert!(cx(&home.0, &root, &["--cx", "path-exact", "a.md"]).contains("\"generation\":1"));

    let store = fs::read_dir(home.0.join(".codeindex/repos"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(&path, "second").unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let mut bytes = [0; 8];
        File::open(store.join("current"))
            .unwrap()
            .read_exact_at(&mut bytes, 16)
            .unwrap();
        if u64::from_le_bytes(bytes) >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watcher failed to publish with no client"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    fs::write(&path, "third").unwrap();
    assert!(cx(&home.0, &root, &["--cx", "content", "third"]).contains("x.txt"));
    let nested = root.join("inner");
    fs::create_dir_all(nested.join(".git")).unwrap();
    fs::write(nested.join("y.txt"), "inner only").unwrap();
    assert!(cx(&home.0, &nested, &["--cx", "content", "inner only"]).contains("y.txt"));
    assert!(cx(&home.0, &root, &["--cx", "content", "inner only"]).contains("\"hits\":[]"));
}

#[test]
fn different_executable_retires_owner_before_reelection() {
    use codeindex::adapters::repository::cache_key;
    use std::os::unix::fs::MetadataExt;
    let home =
        Fixture(std::env::temp_dir().join(format!("cx-watch-upgrade-{}", std::process::id())));
    fs::create_dir(&home.0).unwrap();
    let root = home.0.join("repo");
    fs::create_dir_all(root.join(".git")).unwrap();
    let root = root.canonicalize().unwrap();
    let home_dir = home.0.canonicalize().unwrap();
    let cache = home_dir.join(".codeindex/repos").join(cache_key(&root));
    let socket = PathBuf::from(format!(
        "/tmp/cx-{}",
        fs::metadata(&home_dir).unwrap().uid()
    ))
    .join(cache_key(&cache));
    let run = |bin: &Path, cfg: &str| {
        let out = Command::new(bin)
            .args(["--cx", "path-exact", "a.md"])
            .env("HOME", &home_dir)
            .env("GIT_CONFIG_GLOBAL", home_dir.join(cfg))
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let before = Path::new(env!("CARGO_BIN_EXE_cx"));
    let after = home_dir.join("upgraded-cx");
    fs::copy(before, &after).unwrap();
    let endpoint_id = || {
        let m = fs::metadata(&socket).unwrap();
        (m.ino(), m.ctime(), m.ctime_nsec())
    };
    run(before, "config");
    let first = endpoint_id();
    run(before, "other-config");
    assert_eq!(endpoint_id(), first);
    run(&after, "config");
    let replacement = endpoint_id();
    assert_ne!(replacement, first);
    run(&after, "config");
    assert_eq!(endpoint_id(), replacement);
}
