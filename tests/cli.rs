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
            .map(|_| scope.spawn(|| repo.query(&["--cx", "content", "needle"])))
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
    assert!(repo.query(&["-k", "needle"]).contains("guide.md:3:🙂 needle"));
    assert!(
        repo.query(&["--cx", "section", "guide.md#child", "--parent"])
            .contains("\"start\":8")
    );
    assert!(
        repo.query(&["--cx", "backlinks", "guide.md#setup"])
            .contains("refs.md#")
    );
    repo.write(".gitignore", "*.log\n");
    repo.write("ignored.log", "needle");
    repo.write("guide.md", "# Gone\n");
    fs::remove_file(repo.root.join("refs.md")).unwrap();
    let new = repo.query(&["--cx", "content", "needle"]);
    assert!(new.contains("\"generation\":2"));
    assert!(new.contains("\"hits\":[]"));
    assert!(
        repo.query(&["--cx", "backlinks", "guide.md#setup"])
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
    let first = repo.query(&["--cx", "definition", "two.ts", "2", "22"]);
    assert!(first.contains("\"path\":\"one.ts\""), "{first}");
    let calls = repo.query(&["--cx", "callers", "one.ts", "1", "18"]);
    assert!(calls.contains("\"caller\":\"caller\""), "{calls}");
    repo.write(
        "two.ts",
        "import {greet} from './three';\nfunction caller() { greet(); }",
    );
    assert!(
        repo.query(&["--cx", "callers", "one.ts", "1", "18"])
            .contains("\"result\":[]")
    );
    let usage = repo
        .invoke(&repo.dir, &["--cx", "definition", "x", "0", "1"])
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
}

#[test]
fn relationships_classify_inputs_without_touching_absent_source() {
    let repo = Repo::new();
    repo.write("binary.rs", "a\0b");
    for path in ["binary.rs", "missing.rs"] {
        let answer = repo.query(&["--cx", "definition", path, "1", "1"]);
        assert!(answer.contains("\"operation_status\":\"unindexed\""));
        assert!(answer.contains("\"indexed_document\":false"));
    }
    repo.write("style.css", "p { color: red }");
    let answer = repo.query(&["--cx", "callers", "style.css", "1", "1"]);
    assert!(answer.contains("\"operation_status\":\"unsupported\""));
    assert!(answer.contains("\"result\":[]"));
}

#[test]
fn embedded_regions_use_host_coordinates_and_keep_html_symbols_scoped() {
    let repo = Repo::new();
    repo.write("page.html","<script>let count = 1; function inc() { count++; } inc();</script>\n<style>.chip {color:red} .chip {color:blue}</style>\n<script type=\"application/json\">{\"skip\":\"function nope(){}\"}</script>");
    let def = repo.query(&["--cx", "definition", "page.html", "1", "42"]);
    assert!(def.contains("\"operation_status\":\"resolved\""), "{def}");
    assert!(def.contains("\"col\":13"), "{def}");
    assert!(
        repo.query(&["--cx", "references", "page.html", "2", "9"])
            .contains("\"col\":26")
    );
    assert!(repo.query(&["--cx", "symbol", "inc"]).contains("\"hits\":[]"));
}

#[test]
fn css_custom_properties_bind_to_their_rule_or_root() {
    let repo = Repo::new();
    repo.write("vars.css",":root { --accent: red; }\n.a { color: var(--accent); }\n.b { --accent: blue; color: var(--accent); }\n");
    assert!(
        repo.query(&["--cx", "definition", "vars.css", "2", "19"])
            .contains("\"line\":1,\"col\":9")
    );
    assert!(
        repo.query(&["--cx", "definition", "vars.css", "3", "36"])
            .contains("\"line\":3,\"col\":6")
    );
    assert!(repo.query(&["--cx", "symbol", "--accent"]).contains("\"hits\":[]"));
}

#[test]
fn regex_queries_match_paths_and_text_independently() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join("src")).unwrap();
    repo.write("src/task12.bin", "abc\0xyz");
    repo.write("note.txt", "src/task34.bin\nsecond line\n");
    let paths = repo.query(&["--cx", "path-regex", r"^src/task[0-9]+\.bin$"]);
    assert!(
        paths.contains(r#""hits":[{"path":"src/task12.bin"}]"#),
        "{paths}"
    );
    let text = repo.query(&["--cx", "regex", r"task\d+\.bin\nsecond"]);
    assert!(
        text.contains(r#""path":"note.txt","line":1,"byte_column":5"#),
        "{text}"
    );
    assert!(
        repo.query(&["--cx", "path-regex", r"(?i)NOTE"])
            .contains(r#""hits":[{"path":"note.txt"}]"#)
    );
    assert_eq!(
        repo.invoke(&repo.dir, &["--cx", "path-regex", "["])
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn agent_help_is_available_before_repository_discovery() {
    let repo = Repo::new();
    for args in [&["--help"][..], &["--cx"], &["-h"]] {
        let out = repo.invoke(&repo.dir, args).unwrap();
        assert!(out.status.success());
        assert!(
            String::from_utf8(out.stdout)
                .unwrap()
                .contains("Rank passages by BM25")
        );
    }
    let intro = repo.invoke(&repo.dir, &["--cx", "intro"]).unwrap();
    assert!(intro.status.success());
    assert_eq!(intro.stdout, include_bytes!("../src/adapters/cli/intro.txt"));
    let out = repo.invoke(&repo.dir, &["--cx", "help", "definition"]).unwrap();
    assert!(out.status.success());
    let info = String::from_utf8(out.stdout).unwrap();
    assert!(info.contains("Example: cx --cx definition") && info.contains("UTF-16"));
    assert_eq!(
        repo.invoke(&repo.dir, &["--cx", "help", "typo"])
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
    repo.query(&["--cx", "path-exact", "guide.md"]);
    repo.write("00.md", "Added SharedName café\n");
    repo.write("help.md", "## New\nUniqueTerm SharedName parser\n");
    fs::rename(repo.root.join("z.md"), repo.root.join("refs.md")).unwrap();
    fs::remove_file(repo.root.join("util.ts")).unwrap();
    repo.write("util.ts", "export function replacement() {}\n");
    repo.query(&["--cx", "path-exact", "guide.md"]);

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
    fresh(&["--cx", "path-exact", "guide.md"]);
    for args in [
        &["-k", "SharedName café parser"][..],
        &["-k", "UniqueTerm"],
        &["--cx", "regex", "Shared(Name|ness)"],
        &["--cx", "content", "OldTerm"],
        &["--cx", "path", ".md"],
        &["--cx", "path-regex", "^(00|guide)"],
        &["--cx", "symbol", "helper"],
        &["--cx", "definition", "caller.ts", "2", "2"],
        &["--cx", "section", "help.md#new"],
        &["--cx", "backlinks", "guide.md#entry"],
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
fn project_config_change_rebinds_an_untouched_import() {
    let repo = Repo::new();
    repo.write("one.ts", "export function render() {}\n");
    repo.write("two.ts", "export function render() {}\n");
    repo.write("caller.ts", "import {render} from '@impl';\nrender();\n");
    for name in ["one", "two"] {
        let config = format!(r#"{{"compilerOptions":{{"paths":{{"@impl":["./{name}"]}}}}}}"#);
        repo.write("tsconfig.json", &config);
        let result = repo.query(&["--cx", "definition", "caller.ts", "2", "2"]);
        assert!(result.contains(&format!(r#""path":"{name}.ts""#)), "{result}");
    }
}

#[test]
fn same_path_markdown_updates_retain_coordinates_and_remove_retired_reverse_keys() {
    let repo=Repo::new();
    repo.write("alpha.md","[x](page.md#here) [x](page.md#here) [y](old.md)");
    repo.write("beta.md","[z](page.md#here)");
    repo.query(&["--cx", "path-exact", "guide.md"]);
    repo.write("alpha.md","first paragraph\n\n[x](page.md#here) [x](page.md#here) [y](old.md)");
    let current=repo.query(&["--cx", "backlinks","page.md#here"]);
    assert_eq!(current.matches("\"line\":3,").count(),2,"{current}");
    repo.write("alpha.md","[s](new.md) [y](other.md)");
    let current=repo.query(&["--cx", "backlinks","page.md#here"]);
    assert!(!current.contains("alpha.md") && current.contains("beta.md"),"{current}");
    assert!(repo.query(&["--cx", "backlinks","old.md"]).contains("\"hits\":[]"));
    assert!(repo.query(&["--cx", "backlinks","new.md"]).contains("alpha.md"));
    repo.write("beta.md","\0");
    assert!(repo.query(&["--cx", "backlinks","page.md#here"]).contains("\"hits\":[]"));
    repo.write("beta.md","[z](new.md)");
    let current=repo.query(&["--cx", "backlinks","new.md"]);
    assert!(current.contains("alpha.md") && current.contains("beta.md"),"{current}");
}

#[test]
fn symbol_column_can_be_used_for_callers_after_non_ascii_text() {
    let repo = Repo::new();
    repo.write("one.ts", "/*🙂*/ function greet() {}\nfunction start() { greet(); }");
    let hits = repo.query(&["--cx", "symbol", "greet"]);
    assert!(hits.contains(r#""line":1,"byte_column":19,"col":17"#), "{hits}");
    let named = repo.query(&["--cx", "callers", "greet"]);
    assert!(named.contains(r#""caller":"start""#), "{named}");
    let calls = repo.query(&["--cx", "callers", "one.ts", "1", "17"]);
    assert!(calls.contains(r#""caller":"start""#), "{calls}");
}

#[test]
fn named_rust_methods_distinguish_owners_and_reject_ambiguous_names() {
    let repo = Repo::new();
    repo.write("Cargo.toml", "[package]\nname='demo'\nversion='0.1.0'\n");
    fs::create_dir(repo.root.join("src")).unwrap();
    repo.write("src/lib.rs", "pub struct A; pub struct B;\nimpl A { pub fn go() {} }\nimpl B { pub fn go() {} }\npub fn first() { A::go(); }\npub fn second() { B::go(); }");
    let hits = repo.query(&["--cx", "callers", "A::go"]);
    assert!(hits.contains(r#""caller":"first""#), "{hits}");
    assert!(!hits.contains(r#""caller":"second""#), "{hits}");
    let refs = repo.query(&["--cx", "references", "B::go"]);
    assert!(refs.contains(r#""line":5"#), "{refs}");
    assert_eq!(refs, repo.query(&["--reference", "B::go", "--json"]));
    assert_eq!(repo.query(&["--cx", "symbol", "go"]), repo.query(&["--symbol", "go", "--json"]));
    assert_eq!(repo.query(&["--cx", "references", "src/lib.rs", "5", "22"]),
               repo.query(&["--reference", "src/lib.rs", "5", "22", "--json"]));
    for name in ["go", "Missing::go"] {
        let out = repo.invoke(&repo.root, &["--cx", "callers", name]).unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("cx --symbol go"));
    }
}

#[test]
fn bare_search_is_line_oriented_scoped_and_composes_context() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join("src")).unwrap();
    repo.write("src/a.txt", "skip\r\nbefore\r\nfoo() foo()\r\nbetween\r\nfoo()\r\nafter");
    repo.write("else.txt", "foo()\n");
    let hits = repo.query(&["-C", "1", "^foo", "src"]);
    assert_eq!(hits, "src/a.txt-before\r\nsrc/a.txt:foo() foo()\r\nsrc/a.txt-between\r\nsrc/a.txt:foo()\r\nsrc/a.txt-after\n");
    let nested = repo.invoke(&repo.root.join("src"), &["-F", "foo()"]).unwrap();
    assert!(nested.status.success());
    assert_eq!(nested.stdout, b"a.txt:foo() foo()\r\na.txt:foo()\r\n");
    let miss = repo.invoke(&repo.root, &["-F", "foo.*"]).unwrap();
    assert_eq!(miss.status.code(), Some(1));
    assert!(miss.stdout.is_empty() && miss.stderr.is_empty());
    repo.write("flags.txt", "--help\nstatus\nsearch");
    assert_eq!(repo.query(&["search", "flags.txt"]), "search\n");
    for flag in ["-k", "--keywords"] {
        let hits = repo.query(&[flag, "search"]);
        assert!(hits.starts_with("flags.txt:") && hits.contains(":search"), "{hits}");
        assert_eq!(repo.invoke(&repo.root, &[flag]).unwrap().status.code(), Some(2));
    }
    assert_eq!(repo.query(&["--", "--help"]), "flags.txt:--help\n");
    assert_eq!(repo.query(&["status"]), "flags.txt:status\n");
    for args in [&["-C", "no", "foo"][..], &["("], &["--cx", "callers"]] {
        assert_eq!(repo.invoke(&repo.root, args).unwrap().status.code(), Some(2));
    }
}

#[test]
fn ignore_case_preserves_text_and_byte_positions() {
    let repo = Repo::new();
    repo.write("hits.txt", "🙂 KEy[1]\nkey1\nother\n");
    for flags in [&["-iF"][..], &["-Fi"], &["-F", "-i"], &["--ignore-case", "-F"]] {
        let mut args = flags.to_vec();
        args.push("key[1]");
        assert_eq!(repo.query(&args), "hits.txt:🙂 KEy[1]\n");
    }
    assert_eq!(repo.query(&["-i", "^KEY[1]$"]), "hits.txt:key1\n");
    assert_eq!(repo.invoke(&repo.root, &["-i", "(?-i:KEY)"]).unwrap().status.code(), Some(1));
}

#[test]
fn public_queries_share_source_locations_and_keep_diagnostics_off_stdout() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join("sub")).unwrap();
    repo.write("api.ts", "/*🙂*/ function greet() {}\nfunction start() { greet(); greet(); }\n");
    repo.write("guide.md", "# Operations\n\nRetry the request with a delay.\n");
    let run = |args: &[&str]| repo.invoke(&repo.root.join("sub"), args).unwrap();

    let symbol = run(&["--symbol","greet","--column"]);
    assert!(symbol.status.success());
    assert_eq!(symbol.stdout, "../api.ts:1:19:/*🙂*/ function greet() {}\n".as_bytes());
    let refs = run(&["--reference","../api.ts","1","19","--column"]);
    assert!(refs.status.success());
    assert_eq!(refs.stdout, b"../api.ts:2:20:function start() { greet(); greet(); }\n");
    assert!(String::from_utf8(refs.stderr).unwrap().contains("partial"));
    let json = run(&["--reference","../api.ts","1","19","--json"]);
    assert!(json.status.success());
    assert!(String::from_utf8(json.stdout).unwrap().contains("\"col\":29"));
    let unused = run(&["--reference","start"]);
    assert_eq!(unused.status.code(),Some(1));
    assert!(unused.stdout.is_empty());
    assert!(String::from_utf8(unused.stderr).unwrap().contains("partial"));
    let rank = run(&["-k","Retry delay"]);
    assert!(rank.status.success());
    assert_eq!(rank.stdout,b"../guide.md:3:Retry the request with a delay.\n");
    let context=run(&["-k","Retry delay","-B1"]);
    assert_eq!(context.stdout,b"../guide.md-2-\n../guide.md:3:Retry the request with a delay.\n");
}

#[test]
fn indexed_limits_count_sites_before_grouping_and_exclude_context() {
    let repo = Repo::new();
    let fifty = "call(); ".repeat(50);
    for (count, extra) in [(50, ""), (51, "call(); ")] {
        repo.write("app.ts", &format!("function call() {{}}\nfunction run() {{ {fifty}{extra}}}\n"));
        let output = repo.invoke(&repo.root,&["--reference","call","-B1"]).unwrap();
        assert!(output.status.success());
        let lines = String::from_utf8(output.stdout).unwrap();
        assert_eq!(lines.lines().count(), 2);
        assert!(lines.starts_with("app.ts-1-function call() {}\napp.ts:2:function run()"));
        let notices=String::from_utf8(output.stderr).unwrap();
        assert_eq!(notices.contains("truncated"),count>50);
        assert!(notices.contains("partial"));
    }
}

#[test]
fn indexed_query_differentiates_empty_partial_and_invalid() {
    let repo=Repo::new();
    repo.write("a.ts","function render() {}\n");
    repo.write("b.ts","function render() {}\n");
    let missing=repo.invoke(&repo.root,&["--reference","unknown"]).unwrap();
    assert_eq!(missing.status.code(),Some(1));
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8(missing.stderr).unwrap().contains("unresolved name"));
    let ambiguous=repo.invoke(&repo.root,&["--reference","render"]).unwrap();
    assert_eq!(ambiguous.status.code(),Some(2));
    assert!(ambiguous.stdout.is_empty());
    assert!(String::from_utf8(ambiguous.stderr).unwrap().contains("cx --symbol render --column"));
    let legacy = repo.invoke(&repo.root, &["--cx", "callers", "render"]).unwrap();
    assert_eq!(legacy.status.code(), Some(2));
    assert!(String::from_utf8(legacy.stderr).unwrap().contains("cx --symbol render --json"));
    let none=repo.invoke(&repo.root,&["--symbol","unknown","--json"]).unwrap();
    assert_eq!(none.status.code(),Some(1));
    assert!(String::from_utf8(none.stdout).unwrap().contains("\"hits\":[]"));
    let invalid=repo.invoke(&repo.root,&["-k","query","--json","--column"]).unwrap();
    assert_eq!(invalid.status.code(),Some(2));
}

#[test]
fn plain_folder_scans_operands_and_stdin_without_creating_a_cache() {
    use std::{io::Write, os::unix::fs::symlink, process::Stdio};
    let repo = Repo::new();
    fs::remove_dir_all(repo.root.join(".git")).unwrap();
    repo.write("lines.txt", "before\nA.b\naXb\nafter\n");
    repo.write("binary", "A.b\0tail");
    repo.write(".hidden", "A.b");
    repo.write(".ignore", "skip.txt\n");
    repo.write("skip.txt", "A.b");
    fs::create_dir(repo.root.join("sub")).unwrap();
    repo.write("sub/skip.txt", "A.b");
    assert_eq!(repo.invoke(&repo.root, &["-F", "A.b", "sub"]).unwrap().status.code(), Some(1));
    symlink("lines.txt", repo.root.join("link")).unwrap();
    assert_eq!(repo.query(&["-niF", "a.b"]), "lines.txt:2:A.b\n");
    assert_eq!(repo.query(&["-C1", "-F", "A.b", "lines.txt"]), "before\nA.b\naXb\n");
    assert_eq!(repo.query(&["-l", "-g", "*.txt", "^a.b$"]), "lines.txt\n");
    assert_eq!(repo.invoke(&repo.root, &["missing"]).unwrap().status.code(), Some(1));
    assert_eq!(repo.invoke(&repo.root, &["x", "missing.txt"]).unwrap().status.code(), Some(2));
    let mut child = Command::new(env!("CARGO_BIN_EXE_cx")).args(["-n", "yes"])
        .current_dir(&repo.root).env("HOME", &repo.dir)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"no\nyes\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"2:yes\n");
    assert!(!repo.dir.join(".codeindex").exists());
}

#[test]
fn plain_folder_indexed_queries_match_worktree_and_see_edits() {
    let repo = Repo::new();
    let plain = repo.dir.join("plain");
    fs::create_dir(&plain).unwrap();
    for (name, body) in [
        ("lib.ts", "export function retry() { return 1; }\n"),
        ("main.ts", "import { retry } from './lib';\nfunction work() { retry(); }\n"),
        ("guide.md", "# Guide\n## Timing\nRetry delay policy\n"),
        ("link.md", "[timing](guide.md#timing)"),
    ] {
        repo.write(name, body);
        fs::write(plain.join(name), body).unwrap();
    }
    let cases: &[&[&str]] = &[
        &["--symbol", "retry"], &["--reference", "retry"], &["-k", "retry delay"],
        &["--cx", "section", "guide.md#timing"], &["--cx", "backlinks", "guide.md#timing"],
    ];
    let outputs: Vec<_> = cases.iter().map(|args| {
        let out = repo.invoke(&plain, args).unwrap();
        assert!(out.status.success(), "{args:?} {}", String::from_utf8_lossy(&out.stderr));
        assert!(!out.stdout.is_empty(), "{args:?}");
        out.stdout
    }).collect();
    assert!(!repo.dir.join(".codeindex").exists());
    for (args, output) in cases.iter().zip(outputs) {
        assert_eq!(output, repo.query(args).as_bytes(), "{args:?}");
    }
    fs::write(plain.join("lib.ts"), "export function renamed() {}").unwrap();
    assert!(String::from_utf8(repo.invoke(&plain, &["--symbol","renamed"]).unwrap().stdout).unwrap().contains("renamed"));
    assert!(repo.invoke(&plain, &["--symbol","retry"]).unwrap().stdout.is_empty());
}

#[test]
fn score_roundtrips_updates_and_matches_plain_folder() {
    let repo = Repo::new();
    let code = "def inspect(a, b, c, d, e):\n    return a\n";
    repo.write("unit.py", code);
    repo.write("notes.txt", "ignored by scoring\n");
    let old = repo.query(&["--score", "unit.py"]);
    assert!(
        old.contains("9.69/10") && old.contains("unit.py:1: inspect | parameters 5"),
        "{old}"
    );
    assert_eq!(repo.query(&["--score", "unit.py"]), old);
    assert!(
        repo.query(&["--score"])
            .contains("1/2 indexed files scored")
    );
    let plain = repo.dir.join("plain");
    fs::create_dir(&plain).unwrap();
    fs::write(plain.join("unit.py"), code).unwrap();
    assert_eq!(
        repo.invoke(&plain, &["--score", "unit.py"]).unwrap().stdout,
        old.as_bytes()
    );
    for cwd in [&repo.root, &plain] {
        for args in [vec!["--score", "unit.py"], vec!["--score", "--json"]] {
            let expected = repo.invoke(cwd, &args).unwrap();
            let mut baseline_args = args;
            baseline_args.push("--ignore-test-rules");
            let actual = repo.invoke(cwd, &baseline_args).unwrap();
            assert!(actual.status.success());
            assert_eq!(actual.stdout, expected.stdout);
        }
        for args in [
            ["--score", "--roles", "--ignore-test-rules"],
            ["--score", "--ignore-test-rules", "--roles"],
            ["--score", "--ignore-test-rules", "--ignore-test-rules"],
        ] {
            assert_eq!(repo.invoke(cwd, &args).unwrap().status.code(), Some(2));
        }
    }
    repo.write("unit.py", "def inspect(a):\n    return a\n");
    assert!(repo.query(&["--score", "unit.py"]).contains("10.00/10"));
    fs::rename(repo.root.join("unit.py"), repo.root.join("renamed.py")).unwrap();
    assert!(repo.query(&["--score", "renamed.py"]).contains("10.00/10"));
    assert!(
        !repo
            .invoke(&repo.root, &["--score", "unit.py"])
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn persisted_test_score_follows_edits_and_renames() {
    let repo = Repo::new();
    let path = "test_counter.py";
    let text = "def test_total(a, b, actual):\n    n = 0\n    if a:\n        n += 1\n    if b:\n        n += 1\n    assert n == actual\n";
    repo.write(path, text);
    let finding = "conditional test expectation";
    assert!(repo.query(&["--score", path]).contains(finding));
    assert!(!repo.query(&["--score", path, "--ignore-test-rules"]).contains(finding));
    fs::rename(repo.root.join(path), repo.root.join("counter.py")).unwrap();
    assert!(!repo.query(&["--score", "counter.py"]).contains(finding));
    fs::rename(repo.root.join("counter.py"), repo.root.join(path)).unwrap();
    assert!(repo.query(&["--score", path]).contains(finding));
    repo.write(path, &text.replace("assert n == actual", "assert n == 2"));
    assert!(!repo.query(&["--score", path]).contains(finding));
}

#[test]
fn roles_follow_edits_and_work_in_plain_folders() {
    let repo=Repo::new();
    let args=["--score","--roles","checks.rs","--json"];
    let run=|cwd:&Path|{let o=repo.invoke(cwd,&args).unwrap(); assert!(o.status.success(),"{:?}",o.stderr); String::from_utf8(o.stdout).unwrap()};
    let initial="pub fn ordinary() {}\n#[test]\nfn verifies() { assert!(true); }\n";
    repo.write("checks.rs",initial);
    let git=run(&repo.root);
    assert!(git.contains("\"default_role\":\"production\""));
    assert!(git.contains("\"start_line\":2,\"end_line\":3"));
    assert!(git.contains("\"role\":\"test\""));
    let plain=repo.dir.join("plain");
    fs::create_dir(&plain).unwrap();
    fs::write(plain.join("checks.rs"),initial).unwrap();
    assert_eq!(git,run(&plain));
    repo.write("checks.rs","pub fn verifies() {}\n");
    assert!(run(&repo.root).contains("\"regions\":[]"));
}

#[test]
fn suite_length_policy_uses_the_pinned_source_and_keeps_the_original_score() {
    let repo = Repo::new();
    let path = "suite.Tests.ps1";
    let suite = "It 'case' {\n  Assert-True\n}\n".repeat(30);
    repo.write(path, &suite);
    assert!(!repo.query(&["--score", path]).contains("unit lines"));
    assert!(repo.query(&["--score", "--ignore-test-rules", path]).contains("<file> | unit lines 90"));
    let large = format!("{suite}BeforeAll {{\n{}}}\n", "  Invoke-Step\n".repeat(75));
    repo.write(path, &large);
    assert!(repo.query(&["--score", path]).contains("BeforeAll { | unit lines 77"));
    let plain = repo.dir.join("plain-suite");
    fs::create_dir(&plain).unwrap();
    fs::write(plain.join(path), large).unwrap();
    let args = ["--score", "--json", path];
    assert_eq!(repo.query(&args).as_bytes(), repo.invoke(&plain, &args).unwrap().stdout);
}
