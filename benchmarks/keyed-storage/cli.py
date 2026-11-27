"""Time a source or Markdown edit through a standalone cx process on a disposable clone."""
import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


@dataclass(frozen=True)
class Replay:
    binary: Path
    repo: Path
    home: Path
    path: str

    def query(self, *args):
        start = time.perf_counter_ns()
        output = subprocess.check_output(
            [self.binary, "--cx", *args], cwd=self.repo,
            env={**os.environ, "HOME": str(self.home), "XDG_CONFIG_HOME": str(self.home / ".config"),
                 "GIT_CONFIG_GLOBAL": "./.cx-replay-no-global", "GIT_CONFIG_NOSYSTEM": "1"},
        )
        elapsed = (time.perf_counter_ns() - start) / 1e6
        return json.loads(output), elapsed


WITNESS = "cx_update_witness"


def source_step(replay, generation, step):
    path = replay.path
    result, ms = replay.query("path-exact", path)
    assert result["generation"] == generation and not result["reused"], (step, result)
    assert result["hits"] == [{"path": path}], (step, result)
    symbols, _ = replay.query("symbol", WITNESS)
    expected = [(path, WITNESS)] if step == "declaration" else []
    assert [(hit["path"], hit["name"]) for hit in symbols["hits"]] == expected, (step, symbols)
    return ms


def markdown_step(replay, _generation, step):
    path = replay.path
    anchor = "other" if step == "retarget" else "guide"
    result, ms = replay.query("backlinks", f"{path}#{anchor}")
    line = 3 if step == "fresh" else 4
    assert [(hit["path"], hit["line"], hit["target"]) for hit in result["hits"]] == [(path, line, f"{path}#{anchor}")], result
    if step == "retarget":
        old, _ = replay.query("backlinks", f"{path}#guide")
        assert not old["hits"], old
    return ms


def source_scenario(repo, path):
    tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=repo).split(b"\0")
    if os.fsencode(path) not in tracked:
        raise ValueError("source must be a tracked repository-relative path")
    doc = repo / path
    if doc.resolve() != doc or doc.suffix not in [".rs", ".js", ".ts", ".tsx"]:
        raise ValueError("expected a regular Rust or JavaScript-family file")
    initial = doc.read_bytes()
    witness = WITNESS.encode()
    declaration = b"pub fn " if doc.suffix == ".rs" else b"export function "
    steps = [("fresh", initial), ("comment", initial + b"\n// " + witness + b"\n"),
             ("declaration", initial + b"\n" + declaration + witness + b"() {}\n")]
    return path, steps, source_step


def markdown_scenario(repo):
    path = "cx-storage-probe.md"
    doc = repo / path
    initial = b"# Guide\n\n[link](#guide)\n"
    with doc.open("xb") as f:
        f.write(initial)
    moved = initial.replace(b"[link]", b"\n[link]")
    steps = [("fresh", initial), ("move", moved), ("retarget", moved.replace(b"(#guide)", b"(#other)"))]
    return path, steps, markdown_step


def paired_replay(workspace, binaries, scenario, rounds):
    repo, folder = workspace
    path, steps, measure = scenario
    doc = repo / path
    for i in range(rounds):
        for arm, binary in binaries[::(-1 if i % 2 else 1)]:
            home = Path(folder) / f"{arm}-{i}"
            home.mkdir()
            replay = Replay(binary, repo, home, path)
            for generation, (step, body) in enumerate(steps, start=1):
                doc.write_bytes(body)
                ms = measure(replay, generation, step)
                yield dict(arm=arm, pair=i, step=step, ms=ms)


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
    with tempfile.TemporaryDirectory() as folder:
        repo = Path(folder).resolve() / "repo"
        subprocess.run(["git", "clone", "--quiet", "--no-hardlinks", args.repo.resolve(), repo], check=True)
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
        try:
            scenario = source_scenario(repo, args.source) if args.source else markdown_scenario(repo)
        except ValueError as err:
            cli.error(str(err))
        samples = list(paired_replay((repo, folder), binaries, scenario, args.rounds))
    path, steps, _ = scenario
    args.output.write_text(json.dumps(dict(revision=revision, path=path, samples=samples)) + "\n")
    for step, _ in steps:
        print(step, {arm: statistics.median(s["ms"] for s in samples if s["step"] == step and s["arm"] == arm)
                     for arm, _ in binaries})


if __name__ == "__main__":
    main()
