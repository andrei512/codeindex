# /// script
# requires-python = ">=3.9"
# dependencies = ["libclang==18.1.1"]
# ///
"""Check native cx against independently selected Clang AST locations in real code.

Run: uv run tests/language_compiled_oracle.py
The compiler and its system headers must be installed. Only named definitions and
direct calls with an unambiguous, source-spelled, same-file definition qualify.
No checkout is built or executed. An invalid translation unit is unverified.
"""
import argparse
from collections import Counter
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

from language_clang_bindings import collect
from language_reverse_witness import ReverseProbe, reverse_witness
from clang import cindex
from language_conformance import ROOT, checkout, QuerySession, expectation, ExpectedLocations, git, symbol_error


from language_compiled_projects import PROJECTS


def run(*args, **kwargs):
    return subprocess.run(args, text=True, capture_output=True, check=True, **kwargs)


def compiler(lang):
    driver = shutil.which("clang++" if lang == "c++" else "clang")
    if not driver:
        raise RuntimeError("install Clang and system headers for " + lang)
    search = run(driver, "-E", "-x", lang, "-v", "-", input="").stderr
    paths = search.split("#include <...> search starts here:", 1)[1].split("End of search list.", 1)[0]
    args = ["-x", lang, "-std=c++17" if lang == "c++" else "-std=c11"]
    for directory in paths.strip().splitlines():
        directory = directory.strip()
        if directory.endswith(" (framework directory)"):
            args.extend(["-F", directory.removesuffix(" (framework directory)")])
        else:
            args.extend(["-isystem", directory])
    if sys.platform == "darwin":
        args.extend(["-isysroot", run("xcrun", "--show-sdk-path").stdout.strip()])
    return args


def sample(rows, cap=24):
    if len(rows) <= cap:
        return rows
    return [rows[i * (len(rows) - 1) // (cap - 1)] for i in range(cap)]


class CertifiedUnit:
    """Original compiler locations and direct bindings for a specific file and flags."""
    def __init__(self, path, flags):
        self.path = path
        self.flags = tuple(flags)
        self.sites = collect(path, self.flags)

    def unchanged(self, extra_flags):
        return collect(self.path, [*self.flags, *extra_flags]) == self.sites


    def probes(self, binary, repo, home, extra_flags):
        """Create a new observation scope only after rechecking every Clang anchor."""
        if self.unchanged(extra_flags):
            return Probes(binary, repo, home)


class Probes:
    def __init__(self, binary, repo, home):
        self.binary = binary
        self.repo = repo
        self.home = home
        self.symbols = {}
        self.run_query = QuerySession(binary, repo, home)

    def recertify(self, unit, extra_flags=()):
        """A control can reuse gold anchors only after Clang confirms their identities."""
        return unit.probes(self.binary, self.repo, self.home, extra_flags)

    def check(self, row):
        subject = row["name"] if row["op"] == "symbol" else row["subject"]
        return self.request(row["op"], subject, row["expected"], "fresh")

    def request(self, op, subject, expected, phase):
        if op != "symbol":
            return self.run_query(expectation(op, subject, expected), phase), None
        if subject not in self.symbols:
            result = self.run_query.request("symbol", subject)
            if result.returncode not in (0, 1):
                raise RuntimeError(result.stderr)
            self.symbols[subject] = SymbolAnswer(subject, json.loads(result.stdout), result.returncode)
        return self.symbols[subject].verdict(expected)


class SymbolAnswer:
    """One certified response in one source version, projected as its expectations require."""
    def __init__(self, name, answer, status):
        self.answer = answer
        self.error = symbol_error(answer, name, status)
        self.locations = {}

    def verdict(self, expected):
        if self.error:
            return self.error, None
        query = ExpectedLocations.parse(expected)
        projection = query.with_column, query.with_label
        if projection not in self.locations:
            self.locations[projection] = query.observe(self.answer["hits"], "symbol")
        diff = query.difference_from(self.locations[projection], subset=True)
        if diff and self.answer["truncated"]:
            return {}, "missing from truncated output"
        return diff, None


def symbol_verdict(name, expected, answer, status):
    return SymbolAnswer(name, answer, status).verdict(expected)


@contextmanager
def source_change(path, content):
    """Apply an intervention for this probe and restore its exact previous bytes."""
    before = path.read_bytes() if path.exists() else None
    try:
        if isinstance(content, bytes):
            path.write_bytes(content)
        else:
            path.write_text(content)
        yield
    finally:
        if before is None:
            path.unlink(missing_ok=True)
        else:
            path.write_bytes(before)


def replay_control(probes, unit, rows, control):
    """Compare an isolated source change after Clang confirms all measured sites agree."""
    path = unit.path
    before = path.read_text()
    if control == "digit_separators":
        after, count = re.subn(r"(?<=\d)'(?=\d)", "", before)
    else:
        defines = run("clang++", *unit.flags, "-dM", "-E", str(path)).stdout
        known = set(re.findall(r"^#define (LODEPNG_COMPILE_\w+)\b", defines, re.M))
        def replacement(m):
            enabled = (m[2] in known) == (m[1] == "ifdef")
            return "#if " + str(int(enabled))
        after, count = re.subn(r"^[ \t]*#[ \t]*(ifdef|ifndef)[ \t]+(LODEPNG_COMPILE_\w+)\b",
                               replacement, before, flags=re.M)
    if after == before:
        return {"unverified": "control changed no source"}
    with source_change(path, after):
        corrected = probes.recertify(unit)
        if corrected is None:
            return {"unverified": "Clang declaration or binding locations changed"}
        return control_results(corrected, rows, count)


def query_key(row):
    return row["op"], row["subject"], row["expected"]


def query_rows(unit, rows):
    seen = set()
    for row in rows:
        key = query_key(row)
        if key in seen:
            raise ValueError(f"duplicate compiler oracle query in {unit}: {key}")
        seen.add(key)
        yield key, row


def control_results(probes, rows, count):
    restored, unresolved, restored_queries = [], [], []
    residuals = ((key, row) for key, row in query_rows("intervention", rows) if row["diff"] and not row["excluded"])
    for key, row in residuals:
        diff, excluded = probes.check(row)
        if diff or excluded:
            unresolved.append(row["subject"])
        else:
            restored.append(row["subject"])
            restored_queries.append(key)
    return {"changed_tokens_or_guards": count, "recovered": restored, "remaining": unresolved,
            "recovered_queries": restored_queries}


def write_compile_commands(repo, commands):
    path = repo / "compile_commands.json"
    path.write_text(json.dumps(commands))
    # Some upstreams ignore generated build files. Both participants must see
    # the exact same flags. Only the disposable checkout's index is changed.
    git("add", "--force", "--intent-to-add", "--", path.name, cwd=repo)


def check_unit(probes, filename, flags, pin):
    url, commit = pin
    binary, repo, home = probes.binary, probes.repo, probes.home
    path = repo / filename
    entry = {"url": url, "commit": commit, "rows": []}
    try:
        entry["arguments"] = [v.replace(str(repo), "<checkout>") for v in flags]
        unit = CertifiedUnit(path, flags)
        defs, calls = unit.sites
    except (RuntimeError, cindex.TranslationUnitLoadError) as error:
        entry["unverified"] = str(error)
        print(filename, "unverified", error, flush=True)
        return entry
    entry["eligible"] = {"symbol": len(defs), "definition": len(calls)}
    queries = [("symbol", n, n, f"{filename}:{loc}") for n, loc in sample(defs)]
    queries += [("definition", n, f"{filename}:{site}", f"{filename}:{decl}")
                for n, site, decl in sample(calls)]
    for op, name, subject, expected in queries:
        row = dict(op=op, name=name, subject=subject, expected=expected)
        row["diff"], row["excluded"] = probes.check(row)
        entry["rows"].append(row)
    entry["reverse"] = reverse_witness(ReverseProbe(binary, repo, home), filename, calls, entry["rows"])
    control = {"lodepng.cpp": "compile_guards", "primes/primes.cpp": "digit_separators"}.get(filename)
    if control:
        # The intervention needs its own symbol answers for the changed source.
        entry[control] = replay_control(probes, unit, entry["rows"], control)
    print(filename, entry["eligible"], Counter(r["op"] for r in entry["rows"] if r["diff"]), flush=True)
    return entry


def configure_clang():
    if sys.platform == "darwin":
        # Match the host SDK headers with the Apple frontend, retaining pinned bindings.
        tool = Path(run("xcrun", "--find", "clang").stdout.strip())
        cindex.Config.set_library_file(str(tool.parent.parent / "lib/libclang.dylib"))


def compile_command(repo, filename, flags):
    driver = "clang" if filename.endswith(".c") else "clang++"
    return {"directory": str(repo), "file": filename, "arguments": [driver, *flags, "-c", filename]}


def configure_units(project, repo, files, includes):
    flags_by_file = {}
    commands = []
    for filename in files:
        lang = "c" if filename.endswith(".c") else "c++"
        flags = compiler(lang) + (["-std=c++20"] if project == "kostya" else []) + ["-I" + str(repo / d) for d in includes]
        flags_by_file[filename] = flags
        commands.append(compile_command(repo, filename, flags))
    # Supply exactly the compiler context used for the oracle to cx as well.
    write_compile_commands(repo, commands)
    return flags_by_file


def check_project(project, binary):
    url, commit, files, includes = PROJECTS[project]
    with tempfile.TemporaryDirectory() as tmp:
        repo, home = Path(tmp) / "repo", Path(tmp) / "home"
        repo.mkdir()
        home.mkdir()
        checkout({"url": url, "commit": commit}, repo, ROOT / "target/conformance-cache")
        flags_by_file = configure_units(project, repo, files, includes)
        probes = Probes(binary, repo, home)
        for filename in files:
            yield f"{project}/{filename}", check_unit(probes, filename, flags_by_file[filename], (url, commit))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", choices=PROJECTS, action="append")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-compiled-oracle.json")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    configure_clang()
    results = {}
    for project in args.project or PROJECTS:
        results.update(check_project(project, binary))
    args.report.parent.mkdir(exist_ok=True, parents=True)
    args.report.write_text(json.dumps({"driver": run("clang", "--version").stdout.splitlines()[0],
                                      "units": results}, indent=2) + "\n")
    return int(any(unit_failed(unit) for unit in results.values()))


def unit_failed(unit):
    return (unit.get("unverified") or any(row["diff"] for row in unit["rows"])
            or any(row["wrong"] or row["missing"] for row in unit.get("reverse", [])))


if __name__ == "__main__":
    raise SystemExit(main())
