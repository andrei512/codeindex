"""Exercise public symbol queries across repository changes with known source locations."""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import subprocess
import tempfile

from language_conformance import ROOT, git
from language_paths import CASES

NAME = "cxprobe"


def run_profile(binary, profile):
    filename, source = next((names.split()[0], source) for names, source in CASES[profile] if NAME in source)
    prefix = source.split(NAME)[0]
    col = len(prefix.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2 + 1
    line = prefix.count("\n") + 1
    records = []
    with tempfile.TemporaryDirectory(prefix="cx-transitions-") as base:
        root = Path(base)
        repo, home = root / "repo", root / "home"
        repo.mkdir()
        home.mkdir()
        git("init", "-q", cwd=repo)

        def write(directory, contents=source):
            path = repo / directory / filename
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((contents + "\n").encode())
            return path

        def verify(wave, step, paths, name=NAME):
            proc = subprocess.run([binary, "--symbol", name, "--json"], cwd=repo, text=True,
                                  capture_output=True, timeout=90, env={
                                      **os.environ, "HOME": str(home), "GIT_CONFIG_GLOBAL": "./.missing-config",
                                      "GIT_CONFIG_NOSYSTEM": "1"})
            want = [(str(p.relative_to(repo)), line, col, name) for p in sorted(paths)]
            try:
                answer = json.loads(proc.stdout)
                got = [(r["path"], r["line"], r["col"], r["name"]) for r in answer["hits"]]
                ok = got == want[:50] and answer["truncated"] == (len(want) > 50) and proc.returncode == int(not want)
            except (ValueError, KeyError, TypeError):
                ok = False
            record = {"wave": wave, "step": step, "passed": ok}
            if not ok:
                record.update(expected=want[:50], expected_truncated=len(want) > 50,
                              actual=proc.stdout, stderr=proc.stderr, exit=proc.returncode)
            records.append(record)

        paths = [write(f"project-{n:02d}") for n in reversed(range(49))]
        verify("result limit", "49 hits", paths)
        paths.append(write("project-49"))
        verify("result limit", "50 hits", paths)
        paths.append(write("project-50"))
        verify("result limit", "51 hits", paths)
        first = min(paths)
        first.unlink()
        paths.remove(first)
        verify("result limit", "delete first and promote last", paths)
        for path in paths:
            path.unlink()

        tracked = write("ignored/tracked")
        untracked = write("ignored/untracked")
        git("add", "--", str(tracked.relative_to(repo)), cwd=repo)
        git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
            "-c", "commit.gpgSign=false", "commit", "-qm", "fixture", cwd=repo)
        verify("ignore and staging", "tracked and untracked", [tracked, untracked])
        ignore = repo / ".gitignore"
        ignore.write_text("ignored/\n")
        verify("ignore and staging", "ignored tracked file stays visible", [tracked])
        git("rm", "--cached", "-q", str(tracked.relative_to(repo)), cwd=repo)
        verify("ignore and staging", "untrack ignored file", [])
        ignore.unlink()
        verify("ignore and staging", "remove ignore rule", [tracked, untracked])
        tracked.unlink()
        untracked.unlink()

        path = write("work tree/🙂 area")
        verify("file replacement", "spaced Unicode path", [path])
        before = path.stat()
        new_name = "cxother"
        replacement = path.with_name(path.name + ".tmp")
        replacement.write_bytes(path.read_bytes().replace(NAME.encode(), new_name.encode()))
        os.utime(replacement, ns=(before.st_atime_ns, before.st_mtime_ns))
        os.replace(replacement, path)
        verify("file replacement", "equal size and mtime old name", [])
        verify("file replacement", "equal size and mtime new name", [path], new_name)
        before = path.stat()
        path.write_bytes((source + "\n").encode())
        os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))
        verify("file replacement", "same inode size and mtime restored name", [path])
        verify("file replacement", "same inode size and mtime removed name", [], new_name)

        path.write_bytes((source + "\n").replace("\n", "\r\n").encode())
        verify("source layout", "CRLF", [path])
        path.write_bytes(source.rstrip("\n").encode())
        verify("source layout", "no final newline", [path])
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-transitions.json")
    args = parser.parse_args()
    profiles = {}
    for profile in sorted(CASES):
        profiles[profile] = run_profile(args.binary.resolve(), profile)
    all_rows = [r for rows in profiles.values() for r in rows]
    failures = Counter(r["wave"] for r in all_rows if not r["passed"])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(profiles, indent=2) + "\n")
    print(f"{len(profiles)} profiles, {len(all_rows)} checks, failures by wave: {dict(failures)}")
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
