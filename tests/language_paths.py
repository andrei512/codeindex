"""Exercise language selection through the installed CLI, one real file per repo."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

from language_conformance import ROOT, registry_profiles

# The filenames and source examples form the test contract. Do not generate them
# from Language::for_path: a removed branch would remove its own test.
# Each positive source places its declaration at the single spelling of cxprobe.
CASES = {
    "rust": [("entry.rs", "fn cxprobe() {}")],
    "js": [
        ("entry.js entry.mjs entry.cjs", "function cxprobe() {}"),
        ("entry.ts entry.mts entry.cts entry.d.ts", "interface cxprobe { n: number }"),
        ("entry.jsx", "function cxprobe() { return <span />; }"),
        ("entry.tsx", "function cxprobe(): JSX.Element { return <span />; }"),
    ],
    "python": [("entry.py", "def cxprobe(): pass"), ("entry.pyi", "def cxprobe() -> int: ...")],
    "starlark": [
        ("entry.bzl", "def cxprobe():\n    pass"),
        # BUILD and WORKSPACE are constrained dialects. Use legal input, then
        # inspect parsed_files in a one-file repository to verify routing.
        ("BUILD BUILD.bazel", 'filegroup(name = "target", srcs = [])'),
        ("WORKSPACE WORKSPACE.bazel", 'workspace(name = "sample")'),
    ],
    "shell": [("entry.sh entry.bash", "cxprobe() { :; }")],
    "powershell": [("entry.ps1 entry.psm1", "function cxprobe {}")],
    "csharp": [("Entry.cs", "class cxprobe {}")],
    "swift": [("entry.swift", "func cxprobe() {}")],
    "c": [("entry.c entry.h", "struct cxprobe { int n; };")],
    "cpp": [("entry.cc entry.cpp entry.hpp entry.cxx", "class cxprobe {};")],
    "proto": [("entry.proto", 'syntax = "proto3";\nmessage cxprobe {}')],
    "css": [("entry.css", ".cxprobe { color: blue; }")],
    "html": [("entry.html entry.htm", '<div id="cxprobe"></div>')],
    "sql": [("entry.sql", "CREATE TABLE cxprobe (n INT);")],
    "go": [("entry.go", "package sample\nfunc cxprobe() {}")],
    "java": [("Entry.java", "class cxprobe {}")],
    "kotlin": [("entry.kt entry.kts", "fun cxprobe() = 1")],
    "objectivec": [("Entry.m", "@interface cxprobe\n@end")],
    "dart": [("entry.dart", "void cxprobe() {}")],
    "php": [("entry.php entry.phtml", "<?php function cxprobe() {}")],
    "hack": [
        ("entry.hack entry.hhi entry.php", "<?hh\nnewtype cxprobe = int;"),
        ("bom.php", "\ufeff<?hh\nnewtype cxprobe = int;"),
    ],
    "dockerfile": [
        ("Dockerfile Containerfile Dockerfile.dev Containerfile.dev release.dockerfile", "FROM scratch AS cxprobe"),
    ],
    "makefile": [
        ("Makefile makefile GNUmakefile Makefile.dev makefile.dev GNUmakefile.dev rules.mk rules.mak", "cxprobe:\n\t@:"),
    ],
    "batchfile": [("entry.cmd entry.bat", ":cxprobe\nexit /b")],
    "cmake": [("CMakeLists.txt entry.cmake", "function(cxprobe)\nendfunction()")],
    "procfile": [("Procfile Procfile.dev", "cxprobe: echo hello")],
    "hcl": [("entry.hcl entry.tf", 'variable "cxprobe" {}'), ("entry.tfvars", 'cxprobe = "test"')],
    "nix": [("entry.nix", "let cxprobe = 1; in 1")],
    "ruby": [("entry.rb entry.rake entry.gemspec Gemfile Rakefile", "def cxprobe; end")],
    "lua": [("entry.lua", "local function cxprobe() end")],
    "perl": [("entry.pl entry.pm", "sub cxprobe { 1 }")],
    "assembly": [("entry.s entry.S entry.asm", "cxprobe:")],
    "mako": [("entry.mako entry.mao", '<%def name="cxprobe()">ok</%def>')],
    "plpgsql": [("entry.plpgsql entry.pgsql", "CREATE FUNCTION cxprobe() RETURNS void LANGUAGE plpgsql AS $$ BEGIN RETURN; END; $$;")],
}

# Negative controls catch dispatch on a parent directory or a filename substring.
NEGATIVES = ("entry.rs.bak", "Dockerfile-tests", "nested.Dockerfile/notes.txt")


def check(binary, profile, name, template):
    with tempfile.TemporaryDirectory(prefix="cx-path-") as tmp:
        root = Path(tmp)
        repo, home = root / "repo", root / "home"
        repo.mkdir()
        home.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
        path = repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        if template.count("cxprobe") > 1:
            raise ValueError(f"{name} has multiple declaration anchors")
        prefix, anchor, _ = template.partition("cxprobe")
        wants_hit = bool(anchor) and profile != "negative"
        line = prefix.count("\n") + 1
        col = 1 + len(prefix.split("\n")[-1].encode("utf-16-le")) // 2
        path.write_text(template + "\n", encoding="utf-8")
        proc = subprocess.run(
            [binary, "--symbol", "cxprobe", "--json"], cwd=repo, capture_output=True,
            text=True, timeout=60,
            env={**os.environ, "HOME": str(home), "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "./.missing-config"},
        )
        doc = json.loads(proc.stdout)
        want = [(name, line, col, "cxprobe")] if wants_hit else []
        got = [(r["path"], r["line"], r["col"], r["name"]) for r in doc["hits"]]
        parsed = doc["coverage"]["parsed_files"]
        ok = (got == want and parsed == int(profile != "negative")
              and not doc["truncated"] and proc.returncode == int(not want))
        return dict(profile=profile, path=name, passed=ok, expected=want, actual=got,
                    parsed_files=parsed, exit_code=proc.returncode)


def path_cases():
    cases = [(profile, name, source) for profile, group in CASES.items()
             for names, source in group for name in names.split()]
    cases.extend(("negative", name, "fn cxprobe() {}") for name in NEGATIVES)
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-paths.json")
    args = parser.parse_args()
    live = registry_profiles()
    gaps = sorted(live.symmetric_difference(CASES))
    results = []
    for profile, name, source in path_cases():
        try:
            row = check(args.binary.resolve(), profile, name, source)
        except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
            row = dict(profile=profile, path=name, passed=False, error=str(exc))
        results.append(row)
        if not row["passed"]:
            print(json.dumps(row))
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(dict(inventory_gaps=gaps, cases=results), indent=2) + "\n")
    failures = sum(not r["passed"] for r in results)
    print(f"{len(CASES)} profiles, {len(results)} path cases, {failures} failures, inventory difference {gaps}")
    return int(bool(gaps or failures))


if __name__ == "__main__":
    raise SystemExit(main())
