"""Refresh runner-derived development labels. Requires uv and a native C toolchain.

    python3 benchmarks/test_roles/runner_labels.py --output /tmp/runner.jsonl

The frozen libuv panel was collected on macOS arm64. Its runner registers a
different set of tests on other platforms. Click collection uses its own
pytest configuration. Only actual registered tests and associated fixtures
become labels; unlisted functions have no label.
"""
import argparse
import ast
from collections import Counter, defaultdict
from functools import cache
import hashlib
import inspect
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
PYTHON = "3.13.15"
PYTEST = "9.1.1"
CMAKE = "3.31.6"
PROJECTS = {
    "python": ("pallets/click", "08b803cf32ceb2ae6a772c62d5da42757a897d44",
               "5053b5df8a007357e1984e3670f412ec29c50baed442a799dba30288470d511b"),
    "c": ("libuv/libuv", "de4c8602d54255c4e7f3fa91dd6f44b682679724",
          "f5046bd218eae717376ca6ecfc37be7ad186bdc81bfae5d7d7e6eb662e484c1f"),
}


def fetch_archive(archive, repo, revision, digest):
    url = f"https://codeload.github.com/{repo}/tar.gz/{revision}"
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError(f"archive checksum mismatch: {repo}")
    archive.write_bytes(data)


def source(language, directory):
    repo, revision, digest = PROJECTS[language]
    archive = ROOT / "target/finding-eval" / (revision + ".tgz")
    if not archive.exists():
        archive = directory.parent / (revision + ".tgz")
        if not archive.exists():
            fetch_archive(archive, repo, revision, digest)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
        raise ValueError(f"archive checksum mismatch: {repo}")
    # Re-extract complete regular files; previously edited sources must never
    # become gold just because a cache directory exists.
    fresh = Path(tempfile.mkdtemp(dir=directory))
    with tarfile.open(archive) as tar:
        tar.extractall(fresh, members=[m for m in tar if m.isfile() or m.isdir()], filter="data")
    return fresh / f"{repo.split('/')[-1]}-{revision}"


def pytest_locations(root, dest):
    import pytest

    @cache
    def definitions(path):
        nodes = ast.walk(ast.parse(path.read_bytes(), filename=str(path)))
        return {(min([node.lineno, *(d.lineno for d in node.decorator_list)]), node.name): node.lineno
                for node in nodes if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))}

    result = defaultdict(dict)
    skipped = set()
    def record(obj, role):
        # Inspect the collected callable and join to its def line, skipping
        # third-party fixture implementations and generated source.
        target = inspect.unwrap(obj)
        code = getattr(target, "__code__", None)
        if code is None:
            return
        path = Path(code.co_filename).resolve()
        if not path.is_relative_to(root / "tests"):
            return
        line = definitions(path).get((code.co_firstlineno, code.co_name))
        if line is None:
            skipped.add((str(path.relative_to(root)), code.co_name))
            return
        labels = result[str(path.relative_to(root))]
        value = (role, f"pytest {PYTEST} collected {code.co_name}" if role == "test"
                 else f"pytest {PYTEST} resolved the project fixture {code.co_name} for a collected test")
        if line in labels and labels[line] != value:
            raise ValueError(f"Conflicting pytest callables at {path}:{line}")
        labels[line] = value

    class Witness:
        def pytest_collection_finish(self, session):
            for item in session.items:
                record(item.obj, "test")
                # The final definition is the active one in pytest's override chain.
                for defs in item._fixtureinfo.name2fixturedefs.values():
                    if defs:
                        record(defs[-1].func, "test-support")
            print(f"Collected {len(session.items)} pytest items; {len(skipped)} project "
                  "callables without a unique source definition", file=sys.stderr)

    if pytest.__version__ != PYTEST:
        raise RuntimeError(f"expected pytest {PYTEST}")
    if pytest.main(["--collect-only", "-q", "tests"], plugins=[Witness()]) != pytest.ExitCode.OK:
        raise RuntimeError("pytest collection failed")
    dest.write_text(json.dumps(result))


def libuv_locations(root, directory):
    cmake = ["uv", "run", "--isolated", "--no-project", "--python", PYTHON,
             "--with", f"cmake=={CMAKE}", "cmake"]
    build = directory / "cmake"
    subprocess.run([*cmake, "-S", root, "-B", build, "-DLIBUV_BUILD_TESTS=ON",
                    "-DLIBUV_BUILD_BENCH=OFF", "-DCMAKE_BUILD_TYPE=Release"], check=True)
    subprocess.run([*cmake, "--build", build, "--target", "uv_run_tests", "--parallel", "8"], check=True)
    listing = subprocess.check_output([build / "uv_run_tests", "--list"], cwd=root, text=True)
    return match_registrations(libuv_registered(listing), libuv_declarations(root))


def libuv_registered(listing):
    registered = set()
    for row in listing.splitlines():
        item = re.fullmatch(r"(\w+)\s*(?:\(helpers: ([\w ]+)\))?", row)
        if item is None:
            raise ValueError(f"Unrecognized libuv runner row: {row}")
        registered.add(("test", item[1]))
        registered.update(("test-support", name) for name in (item[2] or "").split())
    return registered


def libuv_declarations(root):
    declarations = defaultdict(list)
    for path in (root / "test").rglob("*.c"):
        for line, text in enumerate(path.read_text().splitlines(), 1):
            match = re.match(r"^\s*(TEST_IMPL|TEST_FS_IMPL|HELPER_IMPL)\s*\(\s*(\w+)\s*\)\s*\{", text)
            if match:
                role = "test-support" if match[1] == "HELPER_IMPL" else "test"
                declarations[role, match[2]].append((str(path.relative_to(root)), line))
    return declarations


def match_registrations(registered, declarations):
    result = defaultdict(dict)
    unresolved = []
    for role, name in sorted(registered):
        candidates = declarations[role, name]
        if len(candidates) == 1:
            path, line = candidates[0]
            result[path][line] = (role, f"uv_run_tests --list registered {name} as {role}")
        else:
            unresolved.append(name)
    print(f"libuv listed {len(registered)} distinct tests/helpers; "
          f"{len(unresolved)} without a unique literal source declaration: {', '.join(unresolved)}", file=sys.stderr)
    return result


def project_labels(language, origin, directory):
    repo, revision = origin
    root = source(language, directory)
    if language == "python":
        dest = directory / "pytest.json"
        env = dict(os.environ, PYTEST_DISABLE_PLUGIN_AUTOLOAD="1", PYTHONPATH=str(root / "src"))
        subprocess.run(["uv", "run", "--no-project", "--isolated", "--python", PYTHON,
                        "--with", f"pytest=={PYTEST}", "--with", root, "python", __file__, "_pytest", dest],
                       cwd=root, env=env, check=True)
        result = json.loads(dest.read_text())
    else:
        result = libuv_locations(root, directory)
    for path, labels in result.items():
        b = (root / path).read_bytes()
        yield dict(language=language, repository=repo, revision=revision, path=path,
                         sha256=hashlib.sha256(b).hexdigest(), split="dev",
                         labels=sorted([int(line), *label] for line, label in labels.items()))
    if language == "c":
        # libuv defines its registration macros in a shared header. Keep
        # that input visible to cx without assigning any role to it.
        path = "test/task.h"
        yield dict(language=language, repository=repo, revision=revision, path=path,
                         sha256=hashlib.sha256((root / path).read_bytes()).hexdigest(),
                         split="dev", labels=[], context=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    cache_root = ROOT / "target/test-role-eval"
    cache_root.mkdir(parents=True, exist_ok=True)
    rows = []
    with tempfile.TemporaryDirectory(dir=cache_root) as tmp:
        directory = Path(tmp)
        for language, (repo, revision, _) in PROJECTS.items():
            rows.extend(project_labels(language, (repo, revision), directory))
    rows.sort(key=lambda r: (r["language"], r["path"]))
    for language in PROJECTS:
        print(language, dict(Counter(role for r in rows if r["language"] == language
                                     for _, role, _ in r["labels"])))
    args.output.write_text("".join(json.dumps(r, separators=(",", ":")) + "\n" for r in rows))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "_pytest":
        pytest_locations(Path.cwd(), Path(sys.argv[2]))
    else:
        main()
