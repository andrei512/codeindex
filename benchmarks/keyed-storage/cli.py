"""Time a source or Markdown edit through a standalone cx process on a disposable clone."""
import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


def query(binary, checkout, home, *args):
    start = time.perf_counter_ns()
    output = subprocess.check_output(
        [binary, "--cx", *args], cwd=checkout,
        env={**os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
             "GIT_CONFIG_GLOBAL": "./.cx-replay-no-global", "GIT_CONFIG_NOSYSTEM": "1"},
    )
    elapsed = (time.perf_counter_ns() - start) / 1e6
    return json.loads(output), elapsed


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("repo", type=Path)
    cli.add_argument("before", type=Path)
    cli.add_argument("after", type=Path)
    cli.add_argument("output", type=Path)
    cli.add_argument("--source", help="tracked Rust, JS, TS or TSX file to edit (defaults to a Markdown fixture)")
    cli.add_argument("--rounds", type=int, default=11)
    args = cli.parse_args()
    if args.rounds < 1:
        cli.error("rounds must be positive")
    binaries = [("before", args.before.resolve()), ("after", args.after.resolve())]
    samples = []
    with tempfile.TemporaryDirectory() as folder:
        repo = Path(folder).resolve() / "repo"
        subprocess.run(["git", "clone", "--quiet", "--no-hardlinks", args.repo.resolve(), repo], check=True)
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
        if args.source:
            tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=repo).split(b"\0")
            path = args.source
            if os.fsencode(path) not in tracked:
                cli.error("source must be a tracked repository-relative path")
            doc = repo / path
            if doc.resolve() != doc or doc.suffix not in [".rs", ".js", ".ts", ".tsx"]:
                cli.error("expected a regular Rust or JavaScript-family file")
            initial = doc.read_bytes()
            witness = b"cx_update_witness"
            declaration = b"pub fn " if doc.suffix == ".rs" else b"export function "
            steps = [("fresh", initial), ("comment", initial + b"\n// " + witness + b"\n"),
                     ("declaration", initial + b"\n" + declaration + witness + b"() {}\n")]
        else:
            path = "cx-storage-probe.md"
            doc = repo / path
            initial = b"# Guide\n\n[link](#guide)\n"
            with doc.open("xb") as f:
                f.write(initial)
            moved = initial.replace(b"[link]", b"\n[link]")
            steps = [("fresh", initial), ("move", moved), ("retarget", moved.replace(b"(#guide)", b"(#other)"))]
        for i in range(args.rounds):
            for arm, binary in binaries[::(-1 if i % 2 else 1)]:
                home = Path(folder) / f"{arm}-{i}"
                home.mkdir()
                for generation, (step, body) in enumerate(steps, start=1):
                    doc.write_bytes(body)
                    if args.source:
                        result, ms = query(binary, repo, home, "path-exact", path)
                        assert result["generation"] == generation and not result["reused"], (step, result)
                        symbols, _ = query(binary, repo, home, "symbol", witness.decode())
                        assert bool(symbols["hits"]) == (step == "declaration"), symbols
                    else:
                        anchor = "other" if step == "retarget" else "guide"
                        result, ms = query(binary, repo, home, "backlinks", f"{path}#{anchor}")
                        line = 3 if step == "fresh" else 4
                        assert [(hit["path"], hit["line"]) for hit in result["hits"]] == [(path, line)], result
                        if step == "retarget":
                            old, _ = query(binary, repo, home, "backlinks", f"{path}#guide")
                            assert not old["hits"], old
                    samples.append(dict(arm=arm, pair=i, step=step, ms=ms))
    args.output.write_text(json.dumps(dict(revision=revision, path=path, samples=samples)) + "\n")
    for step, _ in steps:
        print(step, {arm: statistics.median(s["ms"] for s in samples if s["step"] == step and s["arm"] == arm)
                     for arm, _ in binaries})


if __name__ == "__main__":
    main()
