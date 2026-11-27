"""Compare published bindings after cross-language edits with a fresh index."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
from tempfile import TemporaryDirectory

from language_conformance import ROOT


def check(binary, root):
    repo = root / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    initial = {
        "Cargo.toml": '[package]\nname="sample"\nversion="0.0.1"\n',
        "src/lib.rs": "pub fn rust_target() {}\npub fn rust_user() { rust_target(); }\n",
        "aa.ts": "export const earlier = 1;\n",
        "jslib.ts": "export function js_target() {}\n",
        "client.ts": 'import {js_target} from "./jslib";\nexport function js_user() { js_target(); }\n',
        "package.json": '{"name":"sample","version":"1.0.0"}',
    }
    for path, body in initial.items():
        (repo / path).parent.mkdir(exist_ok=True)
        (repo / path).write_text(body)
    queries = [("callers", "js_target"), ("callers", "rust_target"),
               ("definition", "src/lib.rs", "2", "24"), ("definition", "client.ts", "2", "30")]

    def query(args, home, *, expect_hits=True):
        cmd = subprocess.run([binary, "--cx", *args], cwd=repo,
                             env=dict(os.environ, HOME=str(home), GIT_CONFIG_NOSYSTEM="1",
                                      GIT_CONFIG_GLOBAL="./.missing-config"),
                             capture_output=True, text=True, check=True)
        answer = json.loads(cmd.stdout)
        assert bool(answer["result"]) == expect_hits, (args, answer)
        return {key: answer[key] for key in ("result", "contexts", "evidence") if key in answer}

    edits = [None, ("aa.ts", initial["aa.ts"] + "export function another() {}\n"),
             ("src/lib.rs", initial["src/lib.rs"] + "pub fn more() {}\n"),
             ("package.json", '{"name":"replacement","version":"2.0.0"}')]
    compare_unrelated_edits(repo, query, queries, edits)

    compare_module_reselection(repo, root, query)
    compare_provider_edits(repo, root, query)


def compare_unrelated_edits(repo, query, queries, edits):
    for step, edit in enumerate(edits):
        if edit:
            (repo / edit[0]).write_text(edit[1])
        for args in queries:
            assert query(args, repo.parent / "incremental") == query(args, repo.parent / f"clean-{step}"), (edit, args)


def compare_module_reselection(repo, root, query):
    # A former module still exists on disk after the parent drops its edge.
    # The caller's written site stays fixed. Fresh and retained graphs must
    # agree both when the edge disappears and when the parent restores it.
    (repo / "src/leaf.rs").write_text("pub fn module_target() {}\n")
    rust = repo / "src/lib.rs"
    with_module = "mod leaf;\npub fn user() { leaf::module_target(); }\n"
    without_module = with_module.replace("mod leaf;", "// hidden")
    args = ("definition", "src/lib.rs", "2", "24")
    for step, (body, expected) in enumerate([(with_module, True), (without_module, False),
                                             (with_module, True)]):
        rust.write_text(body)
        current = query(args, root / "incremental", expect_hits=expected)
        fresh = query(args, root / f"module-{step}", expect_hits=expected)
        assert current == fresh, (step, current, fresh)


def compare_provider_edits(repo, root, query):
    fixtures = ROOT / "tests/fixtures/languages"
    for fixture in ["proto/discovery_imported_package", "powershell/module_exports",
                    "csharp/project_span_alias"]:
        for path in (fixtures / fixture / "repo").iterdir():
            shutil.copyfile(path, repo / path.name)
    (repo / "library.sh").write_text("target() { :; }\n")
    (repo / "caller.sh").write_text("source ./library.sh\ntarget\n")
    (repo / "extension.cs").write_text("class Extra {}\n")
    changes = [
        ("alpha.proto", "message Parcel", "message Carton", ("definition", "envelope.proto", "6", "3")),
        ("Helpers.psm1", "-Function Public", "-Function Hidden", ("definition", "main.ps1", "2", "9")),
        ("library.sh", "target()", "changed()", ("definition", "caller.sh", "2", "2")),
        ("extension.cs", "class Extra {}", "static class Extra { static int AsSpan() {return 0;} }",
         ("definition", "Demo.cs", "5", "43")),
    ]
    for path, old, new, args in changes:
        compare_provider(repo / path, root, query, (old, new, args))


def compare_provider(target, root, query, change):
    old, new, args = change
    original = target.read_text()
    for step, (body, expected) in enumerate([(original, True), (original.replace(old, new), False),
                                             (original, True)]):
        target.write_text(body)
        current = query(args, root / "incremental", expect_hits=expected)
        fresh = query(args, root / f"clean-{target.name}-{step}", expect_hits=expected)
        assert current == fresh, (target.name, step, current, fresh)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    args = parser.parse_args()
    with TemporaryDirectory() as folder:
        check(args.binary.resolve(), Path(folder))
    print("Incremental bindings match fresh builds across six languages")
