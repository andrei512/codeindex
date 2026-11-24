use std::{fs, path::{Path, PathBuf}, process::Command};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn query(repo: &Path, home: &Path, args: &[&str]) -> String {
    let answer = Command::new(env!("CARGO_BIN_EXE_cx"))
        .args(args).current_dir(repo).env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "./.missing-config").env("GIT_CONFIG_NOSYSTEM", "1")
        .output().unwrap();
    assert!(answer.status.success(), "{args:?}: {}", String::from_utf8_lossy(&answer.stderr));
    String::from_utf8(answer.stdout).unwrap()
}

#[test]
fn new_language_symbols_survive_reopen_and_follow_edits() {
    let scratch = Scratch(std::env::temp_dir().join(format!("cx-languages-{}", std::process::id())));
    fs::create_dir(&scratch.0).unwrap();
    let repo = scratch.0.join("repo");
    fs::create_dir(&repo).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
    let files = [
        ("Dockerfile", "FROM scratch AS probe\n"),
        ("A.java", "class probe {}\n"),
        ("Makefile", "probe: input\n\t@echo done\n"),
        ("run.bat", ":probe\r\nexit /b\r\n"),
        ("app.php", "<?php function probe() {}\n"),
        ("CMakeLists.txt", "function(probe)\nendfunction()\n"),
        ("main.kt", "fun probe() {}\n"),
        ("app.plpgsql", "CREATE FUNCTION probe() RETURNS void AS $$ BEGIN RETURN; END; $$ LANGUAGE plpgsql;\n"),
        ("app.rb", "def probe\nend\n"),
        ("main.go", "package sample\nfunc probe() {}\n"),
        ("Thing.m", "@interface probe : NSObject\n@end\n"),
        ("app.dart", "void probe() {}\n"),
        ("app.lua", "local function probe()\nend\n"),
        ("Procfile", "probe: run-service\n"),
        ("main.tf", "variable \"probe\" {}\n"),
        ("start.s", "probe:\n  ret\n"),
        ("default.nix", "{ probe = 1; }\n"),
        ("page.mako", "<%def name=\"probe()\">hello</%def>\n"),
        ("app.pl", "sub probe { return 1; }\n"),
        ("app.hack", "<?hh\nfunction probe(): void {}\n"),
    ];
    for (path, body) in files { fs::write(repo.join(path), body).unwrap(); }
    fs::write(repo.join("mixed.sql"), concat!(
        "CREATE TABLE retained (id int);\n",
        "CREATE FUNCTION embedded() RETURNS void AS $body$\n",
        "<<retry>> BEGIN\n",
        "EXIT retry;\n",
        "END retry; $body$ LANGUAGE plpgsql;\n"
    )).unwrap();
    fs::write(repo.join("pure.sql"), concat!(
        "CREATE FUNCTION outer_name() RETURNS void LANGUAGE sql AS $body$\n",
        "CREATE TABLE phantom (id int); SELECT '$BODY$'; $body$;\n",
        "CREATE VIEW retained_view AS SELECT $$CREATE TABLE phantom(id int)$$;\n",
        "CREATE TABLE dollar$part$name (id int);\n"
    )).unwrap();
    let status = query(&repo, &scratch.0, &["--cx", "path-exact", "main.go"]);
    assert!(status.contains("\"generation\":1"));
    let hits = query(&repo, &scratch.0, &["--cx", "symbol", "probe"]);
    for (path, _) in files {
        assert!(hits.contains(&format!("\"path\":\"{path}\"")), "{path} missing in {hits}");
    }
    for name in ["retained", "embedded"] {
        let hits = query(&repo, &scratch.0, &["--cx", "symbol", name]);
        assert_eq!(hits.matches("\"path\":\"mixed.sql\"").count(), 1, "{hits}");
    }
    for name in ["outer_name", "retained_view", "dollar$part$name"] {
        let hits = query(&repo, &scratch.0, &["--cx", "symbol", name]);
        assert_eq!(hits.matches("\"path\":\"pure.sql\"").count(), 1, "{hits}");
    }
    let hidden = query(&repo, &scratch.0, &["--cx", "symbol", "phantom"]);
    assert!(hidden.contains("\"hits\":[]"), "{hidden}");
    let edge = query(&repo, &scratch.0, &["--cx", "definition", "mixed.sql", "4", "6"]);
    assert!(edge.contains("\"operation_status\":\"resolved\"") && edge.contains("\"line\":3"), "{edge}");
    let processes = query(&repo, &scratch.0, &["--cx", "callers", "Procfile", "1", "2"]);
    assert!(processes.contains("unsupported_operation"), "{processes}");
    fs::write(repo.join("main.go"), "package sample\nfunc replaced() {}\n").unwrap();
    let updated = query(&repo, &scratch.0, &["--cx", "symbol", "replaced"]);
    assert!(updated.contains("\"path\":\"main.go\""));
    let remaining = query(&repo, &scratch.0, &["--cx", "symbol", "probe"]);
    assert!(!remaining.contains("\"path\":\"main.go\""));
}

#[test]
fn partial_source_and_unicode_leave_all_navigation_spans_valid() {
    use codeindex::domain::code::{Language, analyze};
    let paths = ["Dockerfile", "a.java", "Makefile", "a.bat", "a.php", "CMakeLists.txt",
        "a.kt", "a.plpgsql", "a.rb", "a.go", "a.m", "a.dart", "a.lua", "Procfile",
        "a.tf", "a.S", "a.nix", "a.mako", "a.pl", "a.hack"];
    let atoms = ["x", " ", "🙂", "名", "\n", "(", ")", "[", "]", "{", "}", "'", "\"",
        "#", "$", "\\", "function", "<", ">", "/*", "--", ":", "=>", "`", "::", "%"];
    let mut seed = 17u64;
    for _ in 0..128 {
        let mut source = String::new();
        for _ in 0..32 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            source.push_str(atoms[(seed >> 32) as usize % atoms.len()]);
        }
        for path in paths {
            let facts = analyze(Language::for_path(path).unwrap(), &source);
            for span in facts.declarations.iter().map(|v| &v.span).chain(facts.calls.iter().map(|v| &v.span)) {
                assert!(source.get(span.clone()).is_some_and(|s| !s.is_empty()), "{path} {source:?} {span:?}");
            }
            assert!(facts.calls.iter().all(|c| c.owner.is_none_or(|i| i < facts.declarations.len())));
        }
    }
}
