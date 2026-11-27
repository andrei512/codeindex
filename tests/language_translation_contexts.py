"""Check context membership, origin spans and snapshot invalidation through the public CLI."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from language_conformance import ROOT


def check_distinct_evidence(contexts, rows):
    context_ids = [ctx["context"] for ctx in contexts]
    assert len(context_ids) == len(set(context_ids)), "duplicate translation context"
    row_ids = [(row["hit"], row["context"], row["target"]) for row in rows]
    assert len(row_ids) == len(set(row_ids)), "duplicate context evidence"


def shared_header_lines(hits, rows):
    evidence = {}
    for row in rows:
        hit = hits[row["hit"]]
        assert hit["path"] == "shared.hpp", hit
        evidence.setdefault(row["context"], set()).add(hit["line"])
    assert all(len(lines) == 1 for lines in evidence.values()), evidence
    return evidence


def binding_targets(context):
    return {target for binding in context["bindings"] for target in binding["targets"]}


def binding_sites(answer):
    hits, contexts, rows = (answer[key] for key in ("result", "contexts", "evidence"))
    check_distinct_evidence(contexts, rows)
    evidence = shared_header_lines(hits, rows)
    bindings = {ctx["context"]: binding_targets(ctx) for ctx in contexts}
    if not evidence:
        assert not hits and not any(bindings.values()), answer
        return {}
    targets = {}
    for row in rows:
        targets.setdefault(row["context"], set()).add(row["target"])
    assert bindings == targets, (bindings, evidence)
    return evidence


def run(binary, root):
    repo = root / "repo"
    shutil.copytree(ROOT / "tests/fixtures/languages/cpp/shared_header_contexts/repo", repo)
    env = dict(os.environ, HOME=str(root / "home"), GIT_CONFIG_GLOBAL="./.missing-config",
               GIT_CONFIG_NOSYSTEM="1")

    def query():
        result = subprocess.run([binary, "--cx", "definition", "shared.hpp", "6", "32"],
                                cwd=repo, env=env, text=True, capture_output=True, check=True)
        answer = json.loads(result.stdout)
        return binding_sites(answer)

    def expect(lines):
        result = query()
        assert sorted(next(iter(v)) for v in result.values()) == sorted(lines), result
        return result

    # Plain-folder execution uses only transient products.
    first = expect([2, 4])
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    assert expect([2, 4]) == expect([2, 4]) == first
    db = repo / "compile_commands.json"
    commands = json.loads(db.read_text())
    commands[1]["file"] = "unit0.cpp"
    commands[1]["arguments"][-1] = "unit0.cpp"
    db.write_text(json.dumps(commands))
    distinct = expect([2, 4])
    db.write_text(json.dumps([commands[1], commands[0], commands[1]]))
    assert expect([2, 4]) == distinct
    db.write_text(json.dumps([commands[1]]))
    remaining = expect([2])
    assert remaining.items() <= distinct.items()
    (repo / "flags1.hpp").write_text("#define SELECT 0\n")
    expect([4])
    db.unlink()
    # Without a translation context the written conditional is unresolved.
    assert not query()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        run(args.binary.resolve(), Path(tmp))
    print("C++ context membership, duplicate commands, reorder, edit, and deletion passed")
