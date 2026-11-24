//! A reader retains the open segment across another process's publication and cleanup.
use codeindex::adapters::snapshot::Pin;
use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn held_reader() {
    let Some(root) = env::var_os("CODEINDEX_INTEGRATION_READER") else {
        return;
    };
    let pin = Pin::refresh(Path::new(&root)).unwrap();
    let mut out = std::io::stdout().lock();
    writeln!(out, "READER_READY").unwrap();
    out.flush().unwrap();
    drop(out);
    let mut go = [0];
    std::io::stdin().read_exact(&mut go).unwrap();
    assert_eq!(pin.source(0).unwrap(), "first");
}

#[test]
fn reader_can_finish_after_its_segment_has_been_unlinked() {
    use std::io::{BufRead, BufReader};
    let dir = env::temp_dir().join(format!("codeindex-read-pin-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let _guard = Temp(dir.clone());
    let root = dir.join("repo");
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("a.txt"), "first").unwrap();
    let mut child = Command::new(env::current_exe().unwrap())
        .args(["--exact", "held_reader", "--nocapture"])
        .env("HOME", &dir)
        .env("PATH", "")
        .env("CODEINDEX_INTEGRATION_READER", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(stdout.read_line(&mut line).unwrap() > 0);
        if line.contains("READER_READY") {
            break;
        }
    }
    fs::write(root.join("a.txt"), "second generation").unwrap();
    let query = Command::new(env!("CARGO_BIN_EXE_cx"))
        .args(["--cx", "content", "second"])
        .current_dir(&root)
        .env("HOME", &dir)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    assert!(
        String::from_utf8(query.stdout)
            .unwrap()
            .contains("\"generation\":2")
    );
    let store = fs::read_dir(dir.join(".codeindex/repos"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(!store.join("segment-0000000000000001").exists());
    child.stdin.take().unwrap().write_all(b"q").unwrap();
    assert!(child.wait().unwrap().success());
}
