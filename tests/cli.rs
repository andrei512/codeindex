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
fn plain_backlinks_preserve_file_order_and_report_an_extra_hit_only_when_present() {
    let repo = Repo::new();
    let plain = repo.dir.join("plain");
    fs::create_dir(&plain).unwrap();
    for (path, source) in [
        ("z.md", "[last](topic.md#b)".to_owned()),
        ("a.md", "# First\n".to_owned() + &"[one](topic.md#a)\n".repeat(50)),
    ] {
        repo.write(path, &source);
        fs::write(plain.join(path), source).unwrap();
    }
    for (selector, extra) in [("topic.md", true), ("topic.md#a", false), ("topic.md#b", false)] {
        let args = ["--cx", "backlinks", selector];
        let out = repo.invoke(&plain, &args).unwrap();
        assert!(out.status.success());
        let expected = repo.query(&args);
        assert_eq!(out.stdout, expected.as_bytes());
        assert!(expected.contains(&format!("\"truncated\":{extra}")));
        assert_eq!(expected.matches("\"offset\":").count(), if selector == "topic.md#b" { 1 } else { 50 });
    }
}

fn assert_score_flag_behavior(repo: &Repo, cwd: &Path) {
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
        assert_score_flag_behavior(&repo, cwd);
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

#[test]
fn scoring_policy_observes_worktree_edits_and_preserves_search() {
    let repo = Repo::new();
    let score = ["--score", "f.py", "--json"];
    let original = ["--score", "f.py", "--json", "--ignore-score-policy"];
    let source = "def choose(flag):\n    if flag:\n        return 1\n    return 0\n";
    repo.write("f.py", source);
    let baseline = repo.query(&score);
    let symbol = repo.query(&["--symbol", "choose"]);
    repo.write(".cxconfig", "[*.py]\ncomplexity = 2 # small functions\n");
    assert!(repo.query(&score).contains("\"value\":2,\"threshold\":2"));
    assert_eq!(repo.query(&original), baseline);
    repo.write("f.py", &format!("# cx-score: begin complexity -- approved decision\n{source}# cx-score: end complexity\n"));
    let scoped = repo.query(&score);
    assert!(scoped.contains("approved decision") && scoped.contains("\"findings\":[]"), "{scoped}");
    repo.write("f.py", &format!("# cx-score: ignore-file all -- generated\n{source}"));
    let omitted = repo.query(&score);
    assert!(omitted.contains("\"score\":null") && omitted.contains("\"excluded_lines\":4"), "{omitted}");
    repo.write("f.py", "# cx-score: begin unknown -- typo\n");
    let error = repo.invoke(&repo.root, &score).unwrap();
    assert!(!error.status.success());
    assert!(String::from_utf8_lossy(&error.stderr).contains("f.py:1"));
    repo.write("f.py", source);
    repo.write(".cxconfig", "[**]\nfile = off # generated\n");
    assert!(repo.query(&score).contains("\"excluded_files\":1"));
    assert_eq!(repo.query(&["--symbol", "choose"]), symbol);
    fs::remove_file(repo.root.join(".cxconfig")).unwrap();
    assert_eq!(repo.query(&score), baseline);
}

#[test]
fn a_disabled_only_metric_cannot_earn_a_perfect_file_score() {
    let repo = Repo::new();
    repo.write("guide.md", "# Guide\n\nOne line.\n");
    repo.write(".cxconfig", "[*.md]\nlength=off # imported documentation\n");
    let row = repo.query(&["--score", "guide.md", "--json"]);
    assert!(row.contains("\"score\":null") && row.contains("\"excluded_lines\":2"), "{row}");
    assert!(repo.query(&["--score", "guide.md", "--ignore-score-policy"]).contains("10.00/10"));
}

#[test]
fn score_regions_use_the_reported_owner_and_count_coverage_once() {
    let repo = Repo::new();
    repo.write(".cxconfig", "[*.py]\ncomplexity=2 # show simple branches\n");
    repo.write("p.py", "def first(a):\n    # cx-score: begin complexity -- body only\n    if a: return 1\n    # cx-score: end complexity\n    return 0\n# cx-score: begin all -- generated function\ndef second(a):\n    if a: return 1\n    return 0\n# cx-score: end all\n");
    let data = repo.query(&["--score", "p.py", "--json"]);
    assert!(data.contains("\"unit\":\"first\""), "{data}");
    assert!(!data.contains("\"unit\":\"second\""), "{data}");
    assert!(data.contains("\"excluded_lines\":3") && data.contains("\"lines\":3"), "{data}");
    assert!(repo.query(&["--symbol","second"]).contains("def second"));
}

#[test]
fn raised_duplication_threshold_requires_two_eligible_bodies() {
    let repo = Repo::new();
    let compact = "    work(value);\n".repeat(12);
    let expanded = "    work(\n        value\n    );\n".repeat(12);
    let source = format!("fn short() {{\n{compact}}}\nfn long() {{\n{expanded}}}\n");
    let args = ["--score", "f.rs", "--json"];
    repo.write("f.rs", &source);
    let raw = repo.query(&args);
    assert_eq!(raw.matches("similar body (code lines)").count(), 2);
    let config = "[*.rs]\nduplication=20 # only compare substantial units\n";
    repo.write(".cxconfig", config);
    let configured = repo.query(&args);
    assert!(configured.contains("\"findings\":[]"), "{configured}");
    let plain = repo.dir.join("plain");
    fs::create_dir(&plain).unwrap();
    fs::write(plain.join("f.rs"), source).unwrap();
    fs::write(plain.join(".cxconfig"), config).unwrap();
    assert_eq!(repo.invoke(&plain, &args).unwrap().stdout, configured.as_bytes());
}

#[test]
fn markdown_directives_cannot_earn_weight_or_trigger_section_length() {
    let repo = Repo::new();
    repo.write("guide.md", "<!-- cx-score: begin all -- generated -->\n# Header\nContents\n<!-- cx-score: end all -->\n");
    let args = ["--score", "guide.md", "--json"];
    let result = repo.query(&args);
    assert!(result.contains("\"score\":null") && result.contains("\"excluded_lines\":4"), "{result}");
    repo.write(".cxconfig", "[*.md]\nlength=3 # small sections\n");
    repo.write("guide.md", "# Header\n<!-- cx-score: begin depth -- demonstration -->\nContents\n<!-- cx-score: end depth -->\n");
    let result = repo.query(&args);
    assert!(result.contains("\"score\":10") && result.contains("\"lines\":2") && result.contains("\"findings\":[]"), "{result}");
}

#[test]
fn explicit_ignore_paths_use_the_written_operand_from_nested_directories() {
    let repo = Repo::new();
    for root in [&repo.root, &repo.dir.join("plain")] {
        let cwd = root.join("deep/work/here");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(root.join("outside")).unwrap();
        fs::write(root.join("outside/a.txt"), "needle\n").unwrap();
        fs::write(root.join("outside/b.txt"), "needle\n").unwrap();
        fs::write(cwd.join("rules"), "/b.txt\n../../../outside/a.txt\n").unwrap();
        let result = repo.invoke(&cwd, &["--ignore-file", "rules", "-l", "needle", "../../../outside"]).unwrap();
        assert!(result.status.success(), "{:?}", result.stderr);
        assert_eq!(result.stdout, b"../../../outside/b.txt\n");
        fs::write(cwd.join("rules"), "*.TXT\n").unwrap();
        fs::write(root.join("outside/.ignore"), "B.TXT\n").unwrap();
        let result = repo.invoke(&cwd, &["--ignore-file-case-insensitive", "--ignore-file", "rules", "-l", "needle", "../../../outside"]).unwrap();
        assert!(result.status.success(), "{:?}", result.stderr);
        assert_eq!(result.stdout, b"../../../outside/a.txt\n");
    }
}

fn assert_glob_outputs(repo: &Repo, root: &Path) {
    for case in [
        ("src/nested/hidden.txt", "./src/nested/hidden.txt\n"),
        ("!nested/*.txt", "./src/nested/visible.txt\n"),
    ] {
        assert_glob_output(repo, root, &["--files"], case);
        assert_glob_output(repo, root, &["-l", "needle"], case);
    }
}

fn assert_glob_output(repo: &Repo, root: &Path, mode: &[&str], (glob, expected): (&str, &str)) {
    let mut args = mode.to_vec();
    args.extend(["-g", glob, "./src"]);
    let result = repo.invoke(root, &args).unwrap();
    assert!(result.status.success(), "{:?}", result.stderr);
    assert_eq!(result.stdout, expected.as_bytes());
}

#[test]
fn explicit_globs_use_the_invocation_directory_for_walk_and_search() {
    let repo = Repo::new();
    for root in [&repo.root, &repo.dir.join("plain")] {
        let src = root.join("src");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join(".ignore"), "nested/hidden.txt\n").unwrap();
        for name in ["visible.txt", "hidden.txt"] {
            fs::write(src.join("nested").join(name), "needle\n").unwrap();
        }
        assert_glob_outputs(&repo, root);
    }
}

#[test]
fn compressed_search_chooses_decoder_from_named_symlink() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let repo = Repo::new();
    repo.write("payload.bz2", "raw bytes");
    symlink("payload.bz2", repo.root.join("visible.gz")).unwrap();
    let programs = repo.dir.join("no-programs");
    fs::create_dir(&programs).unwrap();
    let gzip = programs.join("gzip");
    fs::write(&gzip, "#!/bin/sh\nprintf 'decoded gzip\\n'\n").unwrap();
    fs::set_permissions(&gzip, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(repo.query(&["-z", "decoded", "visible.gz"]), "decoded gzip\n");
}

#[test]
fn attached_values_require_an_option_with_an_operand() {
    let repo = Repo::new();
    repo.write("sample", "--hidden=yes\n");
    for (flag, name, value) in [
        ("--hidden=", "--hidden", ""),
        ("--fixed-strings=yes", "--fixed-strings", "yes"),
        ("--column=2", "--column", "2"),
    ] {
        let out = repo.invoke(&repo.root, &[flag, "hidden", "sample"]).unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert_eq!(String::from_utf8(out.stderr).unwrap(), format!(
            "cx: invalid CLI arguments: unexpected argument for option '{name}': {value:?}\n"
        ));
    }
    assert_eq!(repo.query(&["--regexp=--hidden=yes", "sample"]), "--hidden=yes\n");
}

#[test]
fn a_scoped_trigram_lookup_preserves_file_matches_and_rejects_partial_literals() {
    let repo = Repo::new();
    repo.write("match.txt", "needle λβγ\n");
    repo.write("partial.txt", "nee eed edl dle λβγ\n");
    repo.write("other.txt", "irrelevant\n");
    for (file, pattern, expected) in [
        ("match.txt", "needle", Some("needle λβγ\n")),
        ("partial.txt", "needle", None),
        ("partial.txt", "λβγ", Some("nee eed edl dle λβγ\n")),
        ("other.txt", "λβγ", None),
    ] {
        let result = repo.invoke(&repo.root, &["-F", pattern, file]).unwrap();
        assert_eq!(result.status.code(), Some(i32::from(expected.is_none())));
        assert_eq!(result.stdout, expected.unwrap_or_default().as_bytes());
        assert!(result.stderr.is_empty());
    }
}

#[test]
fn config_and_cli_switches_respect_occurrence_order_across_the_watcher() {
    let repo = Repo::new();
    repo.write("sample", "needle\nNEEDLE\n");
    repo.write("patterns", "^NEEDLE$\n");
    let config = repo.dir.join("rg.conf");
    fs::write(&config, "-i\n").unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_cx")).args(args).current_dir(&repo.root)
            .env("HOME", &repo.dir).env("RIPGREP_CONFIG_PATH", &config).output().unwrap()
    };
    for (args, expected) in [
        (vec!["-nsfpatterns", "sample"], "2:NEEDLE\n"),
        (vec!["--no-config", "-n", "needle", "sample"], "1:needle\n"),
        (vec!["-snifpatterns", "sample"], "1:needle\n2:NEEDLE\n"),
    ] {
        let output = run(&args);
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, expected.as_bytes());
    }
}

#[test]
fn multiline_limit_keeps_complete_records_and_trailing_context() {
    let repo = Repo::new();
    repo.write("sample", "a\nb\nother\na\nb\nend\n");
    let search = |flags: &[&str]| {
        let mut args = vec!["-Un"];
        args.extend_from_slice(flags);
        args.extend(["(a)\nb", "sample"]);
        repo.query(&args)
    };
    assert_eq!(search(&["-m1", "-A1"]), "1:a\n2:b\n3-other\n");
    assert_eq!(search(&["-m2", "-r", "[$1]", "-A1"]), "1:[a]\n3-other\n4:[a]\n6-end\n");
    assert_eq!(search(&["-m2", "--count-matches"]), "2\n");
    let events = search(&["-m1", "-A1", "--json"]);
    assert_eq!(events.matches("\"type\":\"match\"").count(), 1);
    assert!(events.contains("\"lines\":{\"text\":\"a\\nb\\n\"}"));
    assert!(events.contains("\"lines\":{\"text\":\"other\\n\"}"));
}

#[test]
fn multiline_limit_excludes_a_capture_crossing_the_last_admitted_record() {
    let repo = Repo::new();
    repo.write("sample", "a\nb a\nb\nend\n");
    assert_eq!(repo.query(&["-Unm1", "-r", "[$1]", "-A1", "(a)\nb", "sample"]),
               "1:[a] a\n3-b\n");
    assert_eq!(repo.query(&["-Unom1", "(a)\nb", "sample"]), "1:a\n2:b\n");
}

#[test]
fn word_and_line_constraints_participate_in_alternative_selection() {
    let repo = Repo::new();
    repo.write("input", "ab ax\nab\n");
    assert_eq!(repo.query(&["-wno", "a|ab", "input"]), "1:ab\n2:ab\n");
    assert_eq!(repo.query(&["-xno", "a|ab", "input"]), "2:ab\n");
    assert_eq!(repo.query(&["-xwno", "a|ab", "input"]), "1:ab\n2:ab\n");
    assert_eq!(repo.query(&["-wxno", "a|ab", "input"]), "2:ab\n");

    fs::write(repo.root.join("input"), b"a\xffa a\xef\xbf\xbda").unwrap();
    assert_eq!(repo.query(&["-awbo", "a", "input"]), "4:a\n8:a\n");
}

#[test]
fn empty_match_offsets_follow_source_bytes_and_record_terminators() {
    let repo = Repo::new();
    repo.write("input", "😀y\n");
    assert_eq!(repo.query(&["-abo", "a?", "input"]), "0:\n1:\n2:\n3:\n4:\n5:\n");
    repo.write("input", "x");
    assert_eq!(repo.query(&["-Uabo", "$", "input"]), "0:x\n");
    repo.write("input", "x\n");
    assert_eq!(repo.query(&["--null-data", "-abo", "$", "input"]), "1:\0");
}

#[test]
fn summary_uses_record_terminators_and_binary_search_stops_at_its_limit() {
    let repo = Repo::new();
    repo.write("records", "hit hit\r\nnext\r\n");
    assert_eq!(repo.query(&["--crlf", "--count-matches", "hit", "records"]), "2\r\n");
    assert_eq!(repo.query(&["--null-data", "-c", "hit", "records"]), "1\0");

    assert_eq!(repo.query(&["-cov", "hit", "records"]), "0\n");
    assert_eq!(repo.query(&["--count-matches", "-v", "hit", "records"]), "1\n");
    repo.write("mixed", "before\r\nhit\nafter");
    assert_eq!(repo.query(&["--crlf", "-o", "-C1", "hit", "mixed"]), "before\r\nhit\r\nafter\r\n");
    repo.write("inline.bin", "hit\0end");
    assert_eq!(repo.query(&["-H", "--null", "hit", "inline.bin"]),
               "inline.bin: binary file matches (found \"\\0\" byte around offset 3)\n");

    let bytes = format!("hit\nnext\n{}\n\0", "x".repeat(65_536));
    repo.write("blob", &bytes);
    assert_eq!(repo.query(&["--binary", "-m1", "-A1", "hit", "blob"]), "hit\nnext\n");
}

#[test]
fn stopped_search_emits_one_closing_context_line() {
    let repo = Repo::new();
    repo.write("input", "before\nhit\nhit\ngap\nhit\nafter\n");
    assert_eq!(
        repo.query(&["--stop-on-nonmatch", "-n", "-A5", "hit", "input"]),
        "2:hit\n3:hit\n4-gap\n"
    );
    assert_eq!(
        repo.query(&["-m1", "-n", "-A2", "hit", "input"]),
        "2:hit\n3:hit\n4-gap\n"
    );
}

#[test]
fn scalar_escapes_and_byte_escapes_select_distinct_input_bytes() {
    let repo = Repo::new();
    fs::write(repo.root.join("data"), b"\xe9\n\xc3\xa9\n").unwrap();
    for pattern in [r"(?-u:\x{e9})", r"(?-u:\u00E9)", r"(?-u:\U000000E9)"] {
        let answer = repo.invoke(&repo.root, &["-ao", pattern, "data"]).unwrap();
        assert!(answer.status.success());
        assert_eq!(answer.stdout, "é\n".as_bytes());
    }
    let byte = repo.invoke(&repo.root, &["-ao", r"(?-u:\xE9)", "data"]).unwrap();
    assert_eq!(byte.stdout, b"\xe9\n");
}

#[test]
fn a_broken_followed_link_preserves_readable_siblings() {
    let repo = Repo::new();
    let plain = repo.dir.join("plain");
    fs::create_dir(&plain).unwrap();
    fs::write(plain.join("valid"), "hit\n").unwrap();
    std::os::unix::fs::symlink("absent", plain.join("broken")).unwrap();
    let search = repo.invoke(&plain, &["-j1", "-L", "hit", "."]).unwrap();
    assert_eq!(search.status.code(), Some(2));
    assert_eq!(search.stdout, b"./valid:hit\n");
    assert!(String::from_utf8(search.stderr).unwrap().contains("./broken"));
    assert!(repo.invoke(&plain, &["-qL", "hit", "."]).unwrap().status.success());
}

#[test]
fn line_search_reads_current_source_when_snapshot_refresh_fails() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    repo.write("sample", "before\n");
    repo.write(".gitignore", "unused\n");
    assert!(repo.query(&["--cx", "content", "before"]).contains("sample"));

    let control = repo.root.join(".gitignore");
    fs::set_permissions(&control, fs::Permissions::from_mode(0)).unwrap();
    if fs::File::open(&control).is_ok() {
        // A privileged runner cannot induce this permission failure.
        return;
    }
    repo.write("sample", "after\n");
    let indexed = repo.invoke(&repo.root, &["--cx", "content", "after"]).unwrap();
    assert_eq!(indexed.status.code(), Some(2));
    assert!(indexed.stdout.is_empty());
    assert_eq!(repo.query(&["-n", "after", "sample"]), "1:after\n");

    fs::set_permissions(&control, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(repo.query(&["--cx", "content", "after"]).contains("sample"));
}

#[test]
fn byte_view_literals_keep_indexed_utf8_candidates() {
    let repo = Repo::new();
    repo.write("source", "café\n");
    let pattern = r"(?-u:caf\xC3\xA9)";
    assert_eq!(repo.query(&["-o", pattern, "source"]), "café\n");
    assert_eq!(repo.query(&["-o", pattern, "."]), "./source:café\n");
}

#[test]
fn raw_bytes_and_scalar_literals_can_share_a_capture() {
    let repo = Repo::new();
    fs::write(repo.root.join("sample"), b"\xff\xc3\xa9\n\xff\xe9\n").unwrap();
    let pattern = r"(?-u:\xFF(\u00E9))";
    let result = repo.invoke(&repo.root, &["-ao", "-r", "$1", pattern, "sample"]).unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, "é\n".as_bytes());
}

#[test]
fn quiet_reports_selected_records_in_both_filename_mode_orders() {
    let repo = Repo::new();
    repo.write("sample", "needle\n");
    for switches in [["-q", "--files-without-match"], ["--files-without-match", "-q"]] {
        for (pattern, code) in [("needle", 0), ("missing", 1)] {
            let result = repo.invoke(&repo.root, &[switches[0], switches[1], pattern, "sample"]).unwrap();
            assert_eq!(result.status.code(), Some(code));
            assert!(result.stdout.is_empty());
        }
    }
}

#[test]
fn multiline_binary_matching_preserves_the_source_nul() {
    let repo = Repo::new();
    fs::write(repo.root.join("sample"), b"aa\0bb\n").unwrap();
    assert_eq!(repo.invoke(&repo.root, &["-Uq", "aa\nbb", "sample"]).unwrap().status.code(), Some(1));
    assert_eq!(repo.query(&["-U", r"aa[\s\S]*bb", "sample"]),
               "binary file matches (found \"\\0\" byte around offset 2)\n");
}

#[test]
fn empty_records_accept_zero_width_word_matches() {
    let repo = Repo::new();
    repo.write("sample", "\n");
    assert_eq!(repo.query(&["-abow", "^", "sample"]), "0:\n");
}

#[test]
fn crlf_record_can_match_without_a_printable_submatch() {
    let repo = Repo::new();
    repo.write("sample", "a\r\n");
    assert_eq!(repo.query(&["--crlf", "--count-matches", r"\B", "sample"]), "0\r\n");
    assert_eq!(repo.query(&["--crlf", "-ao", r"\B", "sample"]), "a\r\n");
}

#[test]
fn replacement_positions_account_for_preceding_replacements() {
    let repo = Repo::new();
    repo.write("sample", "x-x\n");
    assert_eq!(repo.query(&["-onb", "--column", "-r", "{xxx}", "x", "sample"]),
               "1:1:0:{xxx}\n1:7:6:{xxx}\n");
}

#[test]
fn vimgrep_projects_each_match_onto_the_same_replaced_row() {
    let repo = Repo::new();
    repo.write("sample", "x-x\n");
    assert_eq!(
        repo.query(&["--vimgrep", "-r", "[xxx]", "x", "sample"]),
        "sample:1:1:[xxx]-[xxx]\nsample:1:7:[xxx]-[xxx]\n"
    );
}

#[test]
fn vimgrep_reports_empty_boundary_columns_and_unterminated_eof() {
    let repo = Repo::new();
    repo.write("sample", "aa\nb");
    assert_eq!(
        repo.query(&["--vimgrep", r"\b", "sample"]),
        "sample:1:1:aa\nsample:1:3:aa\nsample:2:1:b\n"
    );
    assert_eq!(repo.query(&["--vimgrep", "$", "sample"]), "sample:1:3:aa\nsample:2:b\n");
}

#[test]
fn vimgrep_reports_each_rewritten_column_in_a_joined_source_block() {
    let repo = Repo::new();
    repo.write("sample", "ab\ncab\nc end\n");
    assert_eq!(
        repo.query(&["--vimgrep", "-b", "-U", "-r", "X", "ab\\nc", "sample"]),
        "sample:1:1:0:XX end\nsample:1:2:0:XX end\n"
    );
}

#[test]
fn multiline_replacements_report_positions_in_the_rendered_block() {
    let repo = Repo::new();
    repo.write("sample", "!a aa\na?\n");
    assert_eq!(repo.query(&["-onb", "--column", "-U", "-r", "x\ny", "a|a\nb", "sample"]),
               "1:2:1:x\n2:2:1:y\n2:6:5:x\n3:6:5:y\n3:9:8:x\n4:9:8:y\n5:13:12:x\n6:13:12:y\n");
}

#[test]
fn nul_delimited_multiline_replacement_preserves_embedded_lf() {
    let repo = Repo::new();
    repo.write("sample", "x\0y\0");
    for mode in [vec![], vec!["-o"]] {
        let mut args = vec!["--null-data", "-U", "-r", "one\ntwo"];
        args.extend(mode);
        args.extend([r"x\x00y", "sample"]);
        assert_eq!(repo.query(&args), "one\ntwo\0");
    }
}

#[test]
fn unterminated_eof_match_keeps_record_offset_and_has_no_column() {
    let repo = Repo::new();
    repo.write("sample", "a!\nab");
    assert_eq!(repo.query(&["-onb", "--column", "$", "sample"]), "1:3:2:\n2:3:ab\n");
}

#[test]
fn pinned_search_counts_unmatched_files_in_plain_statistics() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join("data")).unwrap();
    repo.write("data/one", "needle\n");
    repo.write("data/two", "other\n");
    repo.query(&["--cx", "content", "needle"]);
    let result = repo.query(&["--stats", "needle", "data"]);
    assert!(result.contains("2 files searched\n"));
    assert!(result.contains("13 bytes searched\n"));
}

#[test]
fn json_binary_search_keeps_the_nul_offset_in_the_event_stream() {
    let repo = Repo::new();
    let mut bytes = b"hit\n".to_vec();
    bytes.extend(b"y\n".repeat(35_000));
    bytes.extend(b"\0tail\n");
    fs::write(repo.root.join("blob"), bytes).unwrap();
    for (operand, searched) in [("blob", 70_010), (".", 65_536)] {
        let output = repo.query(&["--json", "hit", operand]);
        assert_eq!(output.lines().count(), 4);
        let end = output.lines().nth(2).unwrap();
        assert!(end.contains("\"type\":\"end\""));
        assert!(end.contains("\"binary_offset\":70004"));
        assert!(end.contains(&format!("\"bytes_searched\":{searched}")));
    }
}

#[test]
fn json_reports_context_only_files_when_passthru_is_enabled() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join("data")).unwrap();
    repo.write("data/one", "before\nneedle\nafter\n");
    repo.write("data/two", "unrelated\n");
    let result = repo.query(&["--json", "--passthru", "--sort", "path", "needle", "data"]);
    let kinds: Vec<_> = result.lines().map(|line| {
        line.split_once("\"type\":\"").unwrap().1.split('"').next().unwrap()
    }).collect();
    assert_eq!(kinds, ["begin", "context", "match", "context", "end", "begin", "context", "end", "summary"]);
}

#[test]
fn vimgrep_byte_offset_identifies_each_occurrence_on_a_source_row() {
    let repo = Repo::new();
    repo.write("sample", "xx\n");
    assert_eq!(repo.query(&["--vimgrep", "-b", "x", "sample"]),
               "sample:1:1:0:xx\nsample:1:2:1:xx\n");
}

#[test]
fn multiline_only_matching_rows_share_the_occurrence_start() {
    let repo = Repo::new();
    repo.write("sample", "xx\nyx\ny res\n");
    assert_eq!(repo.query(&["-onb", "--column", "-U", r"x\ny", "sample"]),
               "1:2:1:x\n2:2:1:y\n2:5:4:x\n3:5:4:y\n");
}

#[test]
fn replacement_omits_the_terminal_empty_match_at_unterminated_eof() {
    let repo = Repo::new();
    repo.write("sample", "x");
    assert_eq!(repo.query(&["-r", "AB", "a?|$", "sample"]), "ABx\n");
}

#[test]
fn multiline_vimgrep_starts_after_context_beyond_the_covered_rows() {
    let repo = Repo::new();
    repo.write("sample", "first\nx\ny\nafter\n");
    assert_eq!(repo.query(&["--vimgrep", "-U", "-A1", r"x\ny", "sample"]),
               "sample:2:1:x\nsample-4-after\n");
}

#[test]
fn projected_blank_row_inside_a_match_keeps_the_context_group() {
    let repo = Repo::new();
    repo.write("sample", "a\n\nb\nend\n");
    assert_eq!(repo.query(&["-Un", "-o", "-A1", r"a\n\nb", "sample"]),
               "1:a\n3:b\n4-end\n");
}

#[test]
fn multiline_columns_identify_the_first_occurrence_in_each_block() {
    let repo = Repo::new();
    repo.write("sample", "ax\nx\nzzx\na\n   x\n");
    assert_eq!(repo.query(&["-Un", "--column", r"x|q\nz", "sample"]),
               "1:2:ax\n2:2:x\n3:2:zzx\n5:4:   x\n");
}

#[test]
fn replaced_rows_terminate_after_the_displayed_content_and_column_limit() {
    let repo = Repo::new();
    repo.write("sample", "x\n");
    assert_eq!(repo.query(&["--crlf", "--vimgrep", "-r", "X", "x", "sample"]),
               "sample:1:1:X\r\n");
    assert_eq!(repo.query(&["--crlf", "-o", "-r", "A\n", "x", "sample"]), "A\n");
    assert_eq!(repo.query(&["--crlf", "-o", "-M1", "--max-columns-preview", "-r", "A\n", "x", "sample"]),
               "A [... 0 more matches]\r\n");
}

#[test]
fn inverted_multiline_occurrences_select_whole_records_outside_the_match() {
    let repo = Repo::new();
    repo.write("sample", "head\nx\ny\ntail\n");
    assert_eq!(repo.query(&["-Unbv", "--vimgrep", "-o", r"x\ny", "sample"]),
               "sample:1:0:head\nsample:4:9:tail\n");
    assert_eq!(repo.query(&["-Unbv", "--vimgrep", "-C1", r"x\ny", "sample"]),
               "sample:1:0:head\nsample-2-5-x\nsample-3-7-y\nsample:4:9:tail\n");
}

#[test]
fn inverted_multiline_context_keeps_selected_rows_in_text_and_events() {
    let repo = Repo::new();
    repo.write("sample", "a\nb\nc\nx\ny\n");
    let args = ["-Uvn", "-m1", "-A2", r"x\ny", "sample"];
    assert_eq!(repo.query(&args), "1:a\n2:b\n3:c\n");
    let mut json = vec!["--json"];
    json.extend(args);
    let events = repo.query(&json);
    assert_eq!(events.lines().filter(|line| line.contains("\"type\":\"match\"")).count(), 3);
    assert!(events.contains("\"matched_lines\":3"));
}

#[test]
fn multiline_summary_counts_occurrences_within_selected_records() {
    let repo = Repo::new();
    repo.write("sample", "a\nx\ny\nb\n");
    let result = repo.invoke(&repo.root, &["-Uvc", "--include-zero", r"x\ny", "sample"]).unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(result.stdout, b"0\n");
    assert_eq!(repo.query(&["-Uv", "--files-without-match", r"x\ny", "sample"]), "sample\n");
    assert_eq!(repo.query(&["-Uvc", "x", "sample"]), "3\n");
}

#[test]
fn inverted_json_context_can_end_inside_a_multiline_match() {
    let repo = Repo::new();
    repo.write("sample", "a\nb\nx\ny\na\nb\nend\n");
    let events = repo.query(&["-Uv", "-m1", "-C2", "--json", r"a\nb", "sample"]);
    let lines: Vec<_> = events.lines().filter(|line| line.contains("\"line_number\"")).collect();
    assert_eq!(lines.len(), 5);
    for (line, (number, kind)) in lines.iter().zip([(1, "context"), (2, "context"), (3, "match"), (4, "match"), (5, "context")]) {
        assert!(line.contains(&format!("\"line_number\":{number},")));
        assert!(line.contains(&format!("\"type\":\"{kind}\"")));
    }
}
