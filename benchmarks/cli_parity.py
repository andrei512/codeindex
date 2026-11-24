"""Compare native cx output and exit status with ripgrep on a disposable worktree."""
import argparse
import errno
import json
import os
from pathlib import Path
import pty
import shutil
import subprocess
import tempfile


def result(binary, args, root, env, stdin=None, tty=False):
    # Do not offer cx a ripgrep executable at runtime.
    env = dict(env, PATH="")
    if not tty:
        run = subprocess.run([binary, *args], cwd=root, env=env, input=stdin,
                             stdin=subprocess.DEVNULL if stdin is None else None, capture_output=True)
        return run.returncode, run.stdout, run.stderr
    master, slave = pty.openpty()
    try:
        with subprocess.Popen([binary, *args], cwd=root, env=env, stdin=subprocess.DEVNULL,
                              stdout=slave, stderr=subprocess.PIPE) as proc:
            os.close(slave)
            output = bytearray()
            while True:
                try:
                    part = os.read(master, 65536)
                except OSError as exc:
                    if exc.errno == errno.EIO:
                        break
                    raise
                if not part:
                    break
                output.extend(part)
            return proc.wait(), bytes(output), proc.stderr.read()
    finally:
        os.close(master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cx", type=Path)
    args = parser.parse_args()
    cx = str(args.cx.resolve())
    rg = shutil.which("rg")
    if not rg:
        parser.error("install ripgrep for the test oracle")
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp, "repo")
        root.mkdir()
        subprocess.run(["git", "init", "-q", root], check=True)
        (root / "src").mkdir()
        (root / "src/a.txt").write_bytes(b"zero\r\nneedle\r\n\nthree\nfour\nfive\nsix\nneedle again\n")
        (root / "names").write_text("symbol\nhelp\n--reference\n")
        (root / ".gitignore").write_text("ignored.log\n")
        (root / "ignored.log").write_text("needle\n")
        env = {k: v for k, v in os.environ.items() if k != "RIPGREP_CONFIG_PATH"}
        env.update(HOME=temp, TERM="dumb", NO_COLOR="1")
        examples = [
            ["needle", "src/a.txt"], ["needle", "./src"], ["needle"], ["-nH", "needle", "src/a.txt"],
            ["-A24", "needle", "src/a.txt"], ["-nB1", "needle", "src"], ["-C1", "needle", "src/a.txt"],
            ["--column", "needle", "src"], ["-F", "needle again"], ["-iF", "NEEDLE"],
            ["-g", "*.txt", "needle"], ["-c", "needle", "src"], ["-l", "needle", "src"],
            ["-q", "needle"], ["-e", "--reference"], ["--", "--reference"], ["help"],
            ["symbol", "names"], ["^$", "src/a.txt"], ["missing", "src"], ["needle", "ignored.log"],
            ["-w", "needle", "src"], ["-x", "six", "src/a.txt"], ["-o", "n[a-z]+", "src"],
            ["--color=always","--column","-H","needle","src/a.txt"],
            ["--files", "src"], ["--json", "-C1", "needle", "src/a.txt"],
            ["-n", "-e", "three", "-e", "four", "src/a.txt"],
        ]
        cases = [(argv, None, False) for argv in examples]
        cases.extend((argv, None, True) for argv in [["needle", "src"], ["needle", "src/a.txt"]])
        cases.extend((argv, b"no\nneedle\n", False) for argv in [["needle"], ["-n", "needle", "-"]])
        failures = 0
        for argv, data, tty in cases:
            expected = result(rg, argv, root, env, data, tty)
            actual = result(cx, argv, root, env, data, tty)
            if "--json" in argv:
                def json_events(result):
                    status, stdout, stderr = result
                    events = [json.loads(line) for line in stdout.splitlines()]
                    for event in events:
                        data = event["data"]
                        data.pop("elapsed_total", None)
                        data.get("stats", {}).pop("elapsed", None)
                    return status, events, stderr
                expected, actual = json_events(expected), json_events(actual)
            if actual != expected:
                failures += 1
                print(f"{argv} tty={tty} stdin={data!r}\n  rg {expected!r}\n  cx {actual!r}")
        print(f"{len(cases) - failures}/{len(cases)} cases matched {subprocess.check_output([rg, '--version'], text=True).splitlines()[0]}")
        return bool(failures)


if __name__ == "__main__":
    raise SystemExit(main())
