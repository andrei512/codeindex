"""Compare native cx output and exit status with ripgrep on a disposable worktree."""
import argparse
from dataclasses import dataclass
import errno
import json
import os
from pathlib import Path
import pty
import shutil
import subprocess
import tempfile


@dataclass(frozen=True)
class Invocation:
    binary: str
    root: Path
    env: dict

    def capture(self, args, stdin, tty):
        # Do not offer cx a ripgrep executable at runtime.
        options = dict(cwd=self.root, env=dict(self.env, PATH=""))
        command = [self.binary, *args]
        if tty:
            return capture_terminal(command, options)
        run = subprocess.run(
            command, **options, input=stdin,
            stdin=subprocess.DEVNULL if stdin is None else None, capture_output=True,
        )
        return run.returncode, run.stdout, run.stderr


def terminal_chunk(master):
    try:
        return os.read(master, 65536)
    except OSError as exc:
        if exc.errno == errno.EIO:
            return b""
        raise


def read_terminal(master):
    output = bytearray()
    for chunk in iter(lambda: terminal_chunk(master), b""):
        output.extend(chunk)
    return bytes(output)


def capture_terminal(command, options):
    master, slave = pty.openpty()
    try:
        with subprocess.Popen(command, **options, stdin=subprocess.DEVNULL,
                              stdout=slave, stderr=subprocess.PIPE) as proc:
            os.close(slave)
            output = read_terminal(master)
            return proc.wait(), output, proc.stderr.read()
    finally:
        os.close(master)


def json_events(result):
    status, stdout, stderr = result
    events = [json.loads(line) for line in stdout.splitlines()]
    for event in events:
        data = event["data"]
        data.pop("elapsed_total", None)
        data.get("stats", {}).pop("elapsed", None)
    return status, events, stderr


def observed_pair(cx, rg, env, comparison):
    directory, argv, data, tty = comparison
    expected = Invocation(rg, directory, env).capture(argv, data, tty)
    actual = Invocation(cx, directory, env).capture(argv, data, tty)
    # Each executable identifies itself in diagnostics.
    stderr = b"".join(b"cx: " + line[4:] if line.startswith(b"rg: ") else line
                      for line in expected[2].splitlines(keepends=True))
    expected = (*expected[:2], stderr)
    if "--json" in argv:
        return json_events(expected), json_events(actual)
    return expected, actual


def repo_comparisons(root):
    (root / "src").mkdir()
    (root / "src/a.txt").write_bytes(b"zero\r\nneedle\r\n\nthree\nfour\nfive\nsix\nneedle again\n")
    (root / "names").write_text("symbol\nhelp\n--reference\n")
    (root / ".gitignore").write_text("ignored.log\n")
    (root / "ignored.log").write_text("needle\n")
    examples = [
        ["needle", "src/a.txt"], ["needle", "./src"], ["needle"], ["-nH", "needle", "src/a.txt"],
        ["-A24", "needle", "src/a.txt"], ["-nB1", "needle", "src"], ["-C1", "needle", "src/a.txt"],
        # An explicit side overrides -C in either order. Passthru resets both sides.
        ["-A0", "-C2", "four", "src/a.txt"], ["-C2", "-A0", "four", "src/a.txt"],
        ["-B2", "--passthru", "-A1", "four", "src/a.txt"],
        ["-A1", "-C0", "--passthru", "four", "src/a.txt"],
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
    # Use one matching file per directory query to avoid traversal-order assumptions.
    return [(root, *case) for case in cases]


def filter_comparisons(directory):
    (directory / "filters").mkdir()
    (directory / "filters/mod.py").write_text("flush\n")
    (directory / "filters/empty.rs").write_text("other\n")
    (directory / "blobs").mkdir()
    (directory / "blobs/blob.bin").write_bytes(b"bad\0\xffsecret_word\n")
    binary = "blobs/blob.bin"
    (directory / "filters/multi.txt").write_bytes(b"alpha\nbeta\n")
    (directory / "filters/.rgignore").write_text("hidden-by-rule\n")
    (directory / "filters/hidden-by-rule").write_text("ignored-token\n")
    (directory / "patterns.txt").write_text("^alpha$\n")
    commands = [
        ["-U", r"alpha\nbeta", "filters"], ["--one-file-system", "flush", "filters"],
        ["--sort", "path", "-d1", "alpha", "filters"],
        ["-r", "$2:$1", "(fl)(ush)", "filters/mod.py"],
        ["-f", "patterns.txt", "filters"], ["--pretty", "flush", "filters"],
        ["ignored-token", "filters"], ["--no-ignore-dot", "ignored-token", "filters"],
        ["--path-separator", "Z", "-n", "flush", "filters"],
        ["--path-separator", r"\xFF", "-l", "flush", "filters"],
        ["--path-separator", "abc", "flush", "filters"],
        ["--path-separator=", "flush", "filters"],
        ["--type-list"], ["--type-add", "custom:*.abc", "--type-list"],
        ["--type-clear=py", "--type-list"], ["--type-not=rust", "flush", "filters"],
        ["--type-add", "custom:include:py,rust", "-tcustom", "flush", "filters"],
        ["-tcustom", "--type-add=custom:*.py", "flush", "filters"],
        ["--type-clear=py", "-tpy", "flush", "filters"],
        ["-tpy", "-Tpy", "flush", "filters"], ["-Tpy", "-tpy", "flush", "filters"],
        ["-n", "flush", "filters", "missing"],
        ["-n", "flush", "missing", "filters"],
        ["flush", "missing"], ["flush", "missing", "also-missing"],
        ["-nt", "py", "flush", "filters"], ["--type=python", "flush", "filters"],
        ["-trs", "flush", "filters"], ["-tpy", "-trust", "other", "filters"],
        ["-trs", "-g", "*.py", "flush", "filters"], ["-trs", "flush", "filters/mod.py"], ["-t", "unknown", "flush"],
        ["--include", "*.py", "flush"], ["secret_word", "blobs"],
        ["-n", "secret_word", binary], ["-H", "secret_word", binary],
        ["-l", "secret_word", binary], ["absent", binary],
        ["-a", "secret_word", binary], ["--text", "secret_word", "blobs"],
        ["-na", "secret_word", binary], ["-ao", "--column", "secret_word", binary],
        ["-a", "--color=always", "secret_word", binary], ["-a", "secret_word", "blobs", "missing"],
    ]
    return commands


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cx", type=Path)
    parser.add_argument("--rg", default="rg", help="ripgrep executable used as the oracle")
    args = parser.parse_args()
    cx = str(args.cx.resolve())
    rg = shutil.which(args.rg)
    if not rg:
        parser.error("install ripgrep for the test oracle")
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp, "repo")
        root.mkdir()
        subprocess.run(["git", "init", "-q", root], check=True)
        comparisons = repo_comparisons(root)
        env = {k: v for k, v in os.environ.items() if k != "RIPGREP_CONFIG_PATH"}
        env.update(HOME=temp, TERM="dumb", NO_COLOR="1")
        plain = Path(temp, "plain")
        plain.mkdir()
        for directory in (root, plain):
            comparisons.extend((directory, argv, None, False) for argv in filter_comparisons(directory))
        failures = 0
        for comparison in comparisons:
            directory, argv, data, tty = comparison
            expected, actual = observed_pair(cx, rg, env, comparison)
            if actual != expected:
                failures += 1
                print(f"{directory.name} {argv} tty={tty} stdin={data!r}\n  rg {expected!r}\n  cx {actual!r}")
        print(f"{len(comparisons) - failures}/{len(comparisons)} cases matched {subprocess.check_output([rg, '--version'], text=True).splitlines()[0]}")
        return bool(failures)


if __name__ == "__main__":
    raise SystemExit(main())
