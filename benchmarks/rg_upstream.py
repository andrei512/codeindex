"""Run ripgrep 14's unchanged integration tests against both rg and cx on Unix."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cx", type=Path)
    parser.add_argument("ripgrep", type=Path, help="checkout of ripgrep tag 14.1.1")
    parser.add_argument("results", type=Path, help="directory for the two test logs")
    parser.add_argument("--pcre2", action="store_true", help="also run upstream PCRE2 tests")
    args = parser.parse_args()
    repo = args.ripgrep.resolve()
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
    if revision != "4649aa9700619f94cf9c66876e9549d83420e16c":
        parser.error("expected ripgrep tag 14.1.1")
    build = subprocess.check_output(
        ["cargo", "test", "--locked", *(["--features", "pcre2"] if args.pcre2 else []), "--test", "integration", "--no-run", "--message-format=json"],
        cwd=repo, text=True)
    artifacts = {msg["target"]["name"]: Path(msg["executable"]) for msg in
                 (json.loads(line) for line in build.splitlines())
                 if msg.get("executable")}
    args.results.mkdir(parents=True, exist_ok=True)
    success = True
    for arm, binary in [("rg", artifacts["rg"]), ("cx", args.cx.resolve())]:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            # The upstream runner finds the program at ../rg relative to itself.
            (root / "deps").mkdir()
            runner = root / "deps" / "integration"
            shutil.copy2(artifacts["integration"], runner)
            shutil.copy2(binary, root / "rg")
            env = dict(os.environ, TMPDIR=temp, HOME=temp)
            log = args.results / f"{arm}.log"
            with log.open("wb") as stream:
                result = subprocess.run([runner, "--test-threads=4"], cwd=repo, env=env,
                                        stdout=stream, stderr=subprocess.STDOUT)
            print(arm, next((line for line in reversed(log.read_text(errors="replace").splitlines())
                              if line.startswith("test result:")), f"exit {result.returncode}"))
            success &= result.returncode == 0
    return not success


if __name__ == "__main__":
    raise SystemExit(main())
