# /// script
# requires-python = ">=3.9"
# dependencies = ["libclang==18.1.1"]
# ///
"""Run paired, disposable C source controls: uv run tests/language_c_residual_controls.py.

Each trial edits only the return-type prefix on ONE actual definition. Header and
forward declarations are untouched. The libclang baseline must agree on every
eligible definition and call anchor before a changed cx result can be counted.
The ordinary sampled oracle's sites are tagged separately from the full census.
"""
import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import re
import sys
import tempfile

from clang import cindex
from language_compiled_oracle import PROJECTS, collect, compiler, run, sample, symbol_probe, write_compile_commands
from language_conformance import ROOT, check, checkout

CONTROLS = {
    "nanopb": (b"checkreturn", b" " * len("checkreturn")),
    # The pinned cJSON.h declares typedef int cJSON_bool.
    "cjson": (b"cJSON_bool", b"int".ljust(len("cJSON_bool"))),
}


def replacement(source, name, site, project):
    """Return bytes with only the immediate preceding written token replaced."""
    line, col = map(int, site.split(":"))
    lines = source.splitlines(keepends=True)
    text = lines[line - 1]
    prefix = text.decode().encode("utf-16-le")[:2 * (col - 1)].decode("utf-16-le").encode()
    if text[len(prefix):len(prefix) + len(name)] != name.encode():
        raise ValueError(f"Clang anchor does not identify {name} at {site}")
    old, new = CONTROLS[project]
    match = re.search(rb"(?<![a-zA-Z0-9_])" + old + rb"(?=[ \t]+$)", prefix)
    if not match:
        return None
    offset = sum(map(len, lines[:line - 1])) + match.start()
    return source[:offset] + new + source[offset + len(old):]


def observe(binary, repo, home, filename, row, cache):
    if row["op"] == "symbol":
        return symbol_probe(binary, repo, home, row["name"], filename + ":" + row["target"], cache)
    return check(binary, repo, home, "definition", filename + ":" + row["site"],
                 filename + ":" + row["target"], "resolved", False, "control"), None


def probe_unit(repo, filename, binary, home, flags, project):
    path = repo / filename
    source = path.read_bytes()
    defs, calls = collect(path, flags)
    old_defs, old_calls = set(sample(defs)), set(sample(calls))
    rows = [dict(op="symbol", name=n, site=d, target=d, sampled=(n, d) in old_defs) for n, d in defs]
    rows += [dict(op="definition", name=n, site=s, target=d, sampled=(n, s, d) in old_calls)
             for n, s, d in calls]
    groups = defaultdict(list)
    cache = {}
    for row in rows:
        diff, exclusion = observe(binary, repo, home, filename, row, cache)
        row["baseline_diff"] = diff
        row["verdict"] = "unverified" if exclusion else "untested_failure" if diff else "baseline_pass"
        if exclusion:
            row["reason"] = exclusion
        groups[(row["name"], row["target"])].append(row)

    for (name, anchor), related in groups.items():
        if not any(row["verdict"] == "untested_failure" for row in related):
            continue
        changed = replacement(source, name, anchor, project)
        if changed is None:
            for row in related:
                if row["verdict"] == "untested_failure":
                    row["verdict"] = "outside_control"
            continue
        try:
            path.write_bytes(changed)
            after_defs, after_calls = collect(path, flags)
            if (defs, calls) != (after_defs, after_calls):
                raise ValueError("Clang's eligible definition or direct-binding locations changed")
            corrected = {}
            for row in related:
                if row["verdict"] == "unverified":
                    continue
                diff, exclusion = observe(binary, repo, home, filename, row, corrected)
                row["control_diff"] = diff
                if exclusion:
                    row["verdict"] = "unverified"
                    row["reason"] = exclusion
                elif row["baseline_diff"]:
                    row["verdict"] = "still_fails" if diff else "recovered"
                else:
                    row["verdict"] = "new_failure" if diff else "retained_pass"
        except (ValueError, RuntimeError, cindex.TranslationUnitLoadError) as exc:
            for row in related:
                row["verdict"] = "unverified"
                row["reason"] = str(exc)
        finally:
            path.write_bytes(source)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-c-residual-controls.json")
    args = parser.parse_args()
    if sys.platform == "darwin":
        driver = Path(run("xcrun", "--find", "clang").stdout.strip())
        cindex.Config.set_library_file(str(driver.parent.parent / "lib/libclang.dylib"))
    results = {}
    for project in CONTROLS:
        url, commit, files, includes = PROJECTS[project]
        with tempfile.TemporaryDirectory() as tmp:
            repo, home = Path(tmp) / "repo", Path(tmp) / "home"
            repo.mkdir()
            home.mkdir()
            checkout({"url": url, "commit": commit}, repo, ROOT / "target/conformance-cache")
            flags = compiler("c") + ["-I" + str(repo / directory) for directory in includes]
            write_compile_commands(repo, [
                dict(directory=str(repo), file=path, arguments=["clang", *flags, "-c", path]) for path in files])
            for filename in files:
                unit = results[project + "/" + filename] = dict(commit=commit)
                try:
                    rows = probe_unit(repo, filename, args.binary.resolve(), home, flags, project)
                except (ValueError, RuntimeError, cindex.TranslationUnitLoadError) as exc:
                    unit["unverified"] = str(exc)
                    continue
                unit["rows"] = rows
                unit["totals"] = dict(Counter(r["verdict"] for r in rows))
                unit["original_sample"] = dict(Counter(r["verdict"] for r in rows if r["sampled"]))
                print(filename, unit["totals"], "original sample", unit["original_sample"], flush=True)
    args.report.parent.mkdir(exist_ok=True, parents=True)
    args.report.write_text(json.dumps(results, indent=2) + "\n")
    return int(any(u.get("unverified") or any(r["verdict"] in ("unverified", "new_failure")
                   for r in u.get("rows", [])) for u in results.values()))


if __name__ == "__main__":
    raise SystemExit(main())
