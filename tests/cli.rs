use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Repo {
    dir: PathBuf,
    root: PathBuf,
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
impl Repo {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "codeindex-cli-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let root = dir.join("repo");
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        Self { dir, root }
    }
    fn write(&self, name: &str, s: &str) {
        fs::write(self.root.join(name), s).unwrap();
    }
    fn invoke(&self, cwd: &Path, args: &[&str]) -> io::Result<Output> {
        Command::new(env!("CARGO_BIN_EXE_cx"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.dir)
            .env("PATH", self.dir.join("no-programs"))
            .output()
    }
    fn query(&self, args: &[&str]) -> String {
        let out = self.invoke(&self.root, args).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}
#[test]
fn concurrent_first_queries_publish_once_and_updates_retire_rows() {
    let repo = Repo::new();
    repo.write(
        "guide.md",
        "# Guide\n## Setup\n🙂 needle\n### Child\nagain\n",
    );
    repo.write("refs.md", "[Setup](guide.md#setup)\n");
    let answers = std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| repo.query(&["content", "needle"])))
            .collect();
        jobs.into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        answers
            .iter()
            .filter(|a| a.contains("\"reused\":false"))
            .count(),
        1
    );
    for a in answers {
        assert!(a.contains("\"generation\":1"));
        assert!(a.contains("\"byte_column\":6"));
    }
    assert!(repo.query(&["search", "needle"]).contains("🙂 [needle]"));
    assert!(
        repo.query(&["section", "guide.md#child", "--parent"])
            .contains("\"start\":8")
    );
    assert!(
        repo.query(&["backlinks", "guide.md#setup"])
            .contains("refs.md#")
    );
    repo.write(".gitignore", "*.log\n");
    repo.write("ignored.log", "needle");
    repo.write("guide.md", "# Gone\n");
    fs::remove_file(repo.root.join("refs.md")).unwrap();
    let new = repo.query(&["content", "needle"]);
    assert!(new.contains("\"generation\":2"));
    assert!(new.contains("\"hits\":[]"));
    assert!(
        repo.query(&["backlinks", "guide.md#setup"])
            .contains("\"hits\":[]")
    );
}
#[test]
fn callers_follow_the_written_import_across_processes() {
    let repo = Repo::new();
    repo.write("one.ts", "export function greet() {}\n");
    repo.write(
        "two.ts",
        "import {greet} from './one';\nfunction caller() { greet(); }",
    );
    repo.write("three.ts", "export function greet() {}\ngreet();");
    let first = repo.query(&["definition", "two.ts", "2", "22"]);
    assert!(first.contains("\"path\":\"one.ts\""), "{first}");
    let calls = repo.query(&["callers", "one.ts", "1", "18"]);
    assert!(calls.contains("\"caller\":\"caller\""), "{calls}");
    repo.write(
        "two.ts",
        "import {greet} from './three';\nfunction caller() { greet(); }",
    );
    assert!(
        repo.query(&["callers", "one.ts", "1", "18"])
            .contains("\"result\":[]")
    );
    let usage = repo
        .invoke(&repo.dir, &["definition", "x", "0", "1"])
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
}

#[test]
fn relationships_classify_inputs_without_touching_absent_source() {
    let repo = Repo::new();
    repo.write("binary.rs", "a\0b");
    for path in ["binary.rs", "missing.rs"] {
        let answer = repo.query(&["definition", path, "1", "1"]);
        assert!(answer.contains("\"operation_status\":\"unindexed\""));
        assert!(answer.contains("\"indexed_document\":false"));
    }
    repo.write("style.css", "p { color: red }");
    let answer = repo.query(&["callers", "style.css", "1", "1"]);
    assert!(answer.contains("\"operation_status\":\"unsupported\""));
    assert!(answer.contains("\"result\":[]"));
}

#[test]
fn embedded_regions_use_host_coordinates_and_keep_html_symbols_scoped() {
    let repo = Repo::new();
    repo.write("page.html","<script>let count = 1; function inc() { count++; } inc();</script>\n<style>.chip {color:red} .chip {color:blue}</style>\n<script type=\"application/json\">{\"skip\":\"function nope(){}\"}</script>");
    let def = repo.query(&["definition", "page.html", "1", "42"]);
    assert!(def.contains("\"operation_status\":\"resolved\""), "{def}");
    assert!(def.contains("\"col\":13"), "{def}");
    assert!(
        repo.query(&["references", "page.html", "2", "9"])
            .contains("\"col\":26")
    );
    assert!(repo.query(&["symbol", "inc"]).contains("\"hits\":[]"));
}

#[test]
fn css_custom_properties_bind_to_their_rule_or_root() {
    let repo = Repo::new();
    repo.write("vars.css",":root { --accent: red; }\n.a { color: var(--accent); }\n.b { --accent: blue; color: var(--accent); }\n");
    assert!(
        repo.query(&["definition", "vars.css", "2", "19"])
            .contains("\"line\":1,\"col\":9")
    );
    assert!(
        repo.query(&["definition", "vars.css", "3", "36"])
            .contains("\"line\":3,\"col\":6")
    );
    assert!(repo.query(&["symbol", "--accent"]).contains("\"hits\":[]"));
}

#[test]
fn regex_queries_match_paths_and_text_independently() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join("src")).unwrap();
    repo.write("src/task12.bin", "abc\0xyz");
    repo.write("note.txt", "src/task34.bin\nsecond line\n");
    let paths = repo.query(&["path-regex", r"^src/task[0-9]+\.bin$"]);
    assert!(
        paths.contains(r#""hits":[{"path":"src/task12.bin"}]"#),
        "{paths}"
    );
    let text = repo.query(&["regex", r"task\d+\.bin\nsecond"]);
    assert!(
        text.contains(r#""path":"note.txt","line":1,"byte_column":5"#),
        "{text}"
    );
    assert!(
        repo.query(&["path-regex", r"(?i)NOTE"])
            .contains(r#""hits":[{"path":"note.txt"}]"#)
    );
    assert_eq!(
        repo.invoke(&repo.dir, &["path-regex", "["])
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn agent_help_is_available_before_repository_discovery() {
    let repo = Repo::new();
    for args in [&["--help"][..], &["help"], &["-h"], &[]] {
        let out = repo.invoke(&repo.dir, args).unwrap();
        assert!(out.status.success());
        assert!(
            String::from_utf8(out.stdout)
                .unwrap()
                .contains("Rank passages by BM25")
        );
    }
    let out = repo.invoke(&repo.dir, &["help", "definition"]).unwrap();
    assert!(out.status.success());
    let info = String::from_utf8(out.stdout).unwrap();
    assert!(info.contains("Example: cx definition") && info.contains("UTF-16"));
    assert_eq!(
        repo.invoke(&repo.dir, &["help", "typo"])
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert!(!repo.dir.join(".codeindex").exists());
}

#[test]
fn incremental_search_outputs_match_an_empty_store_after_mixed_edits() {
    let repo = Repo::new();
    let common = "SharedName café parser helper\n".repeat(70);
    repo.write("guide.md", &format!("# Entry\n{common}"));
    repo.write("help.md", "## Old\nSharedName parser\n");
    repo.write("z.md", "OldTerm [guide](guide.md#entry)\n");
    repo.write("util.ts", "export function helper() { return 1 }\n");
    repo.write("caller.ts", "import {helper} from './util';\nhelper();\n");
    repo.query(&["status"]);
    repo.write("00.md", "Added SharedName café\n");
    repo.write("help.md", "## New\nUniqueTerm SharedName parser\n");
    fs::rename(repo.root.join("z.md"), repo.root.join("refs.md")).unwrap();
    fs::remove_file(repo.root.join("util.ts")).unwrap();
    repo.write("util.ts", "export function replacement() {}\n");
    repo.query(&["status"]);

    let fresh_home = repo.dir.join("fresh-home");
    fs::create_dir(&fresh_home).unwrap();
    let fresh = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_cx"))
            .args(args)
            .current_dir(&repo.root)
            .env("HOME", &fresh_home)
            .env("PATH", repo.dir.join("no-programs"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    fresh(&["status"]);
    for args in [
        &["search", "SharedName café parser"][..],
        &["search", "UniqueTerm"],
        &["regex", "Shared(Name|ness)"],
        &["content", "OldTerm"],
        &["path", ".md"],
        &["path-regex", "^(00|guide)"],
        &["symbol", "helper"],
        &["definition", "caller.ts", "2", "2"],
        &["section", "help.md#new"],
        &["backlinks", "guide.md#entry"],
    ] {
        let actual = repo.query(args);
        let expected = fresh(args);
        let normalize = |mut json: String| {
            if let Some(start) = json.find("\"generation\":") {
                let digits = start + "\"generation\":".len();
                let end = digits
                    + json[digits..]
                        .bytes()
                        .take_while(u8::is_ascii_digit)
                        .count();
                json.replace_range(digits..end, "0");
            }
            json
        };
        assert_eq!(normalize(actual), normalize(expected), "{args:?}");
    }
}

#[test]
fn checkpoint_indexes_disk_and_promotes_identical_bytes_without_a_product_write() {
    let repo = Repo::new();
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
    repo.write("a.md", "version A");
    git(&["add", "a.md"]);
    git(&["commit", "-qm", "A"]);
    let initial = repo.query(&["status"]);
    assert!(initial.contains("\"committed_text_files\":1"), "{initial}");
    repo.write("a.md", "version B");
    git(&["add", "a.md"]);
    repo.write("a.md", "version C");
    assert!(repo.query(&["content", "version C"]).contains("\"path\":\"a.md\""));
    let dirty = repo.query(&["status"]);
    assert!(dirty.contains("\"committed_text_files\":0"), "{dirty}");
    let field = |s: &str, name: &str| {
        s.split(&format!("\"{name}\":"))
            .nth(1)
            .unwrap()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    let products = field(&dirty, "product_generation");
    git(&["add", "a.md"]);
    git(&["commit", "-qm", "C"]);
    let promoted = repo.query(&["status"]);
    assert!(
        promoted.contains("\"committed_text_files\":1"),
        "{promoted}"
    );
    assert_eq!(field(&promoted, "product_generation"), products);
    repo.write("a.md", "version C");
    assert_eq!(
        field(&repo.query(&["status"]), "product_generation"),
        products
    );
}

#[test]
fn project_config_change_rebinds_an_untouched_import() {
    let repo = Repo::new();
    repo.write("one.ts", "export function render() {}\n");
    repo.write("two.ts", "export function render() {}\n");
    repo.write("caller.ts", "import {render} from '@impl';\nrender();\n");
    for name in ["one", "two"] {
        let config = format!(r#"{{"compilerOptions":{{"paths":{{"@impl":["./{name}"]}}}}}}"#);
        repo.write("tsconfig.json", &config);
        let result = repo.query(&["definition", "caller.ts", "2", "2"]);
        assert!(result.contains(&format!(r#""path":"{name}.ts""#)), "{result}");
    }
}

#[test]
fn same_path_markdown_updates_retain_coordinates_and_remove_retired_reverse_keys() {
    let repo=Repo::new();
    repo.write("alpha.md","[x](page.md#here) [x](page.md#here) [y](old.md)");
    repo.write("beta.md","[z](page.md#here)");
    repo.query(&["status"]);
    repo.write("alpha.md","first paragraph\n\n[x](page.md#here) [x](page.md#here) [y](old.md)");
    let current=repo.query(&["backlinks","page.md#here"]);
    assert_eq!(current.matches("\"line\":3,").count(),2,"{current}");
    repo.write("alpha.md","[s](new.md) [y](other.md)");
    let current=repo.query(&["backlinks","page.md#here"]);
    assert!(!current.contains("alpha.md") && current.contains("beta.md"),"{current}");
    assert!(repo.query(&["backlinks","old.md"]).contains("\"hits\":[]"));
    assert!(repo.query(&["backlinks","new.md"]).contains("alpha.md"));
    repo.write("beta.md","\0");
    assert!(repo.query(&["backlinks","page.md#here"]).contains("\"hits\":[]"));
    repo.write("beta.md","[z](new.md)");
    let current=repo.query(&["backlinks","new.md"]);
    assert!(current.contains("alpha.md") && current.contains("beta.md"),"{current}");
}
