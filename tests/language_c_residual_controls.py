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
from dataclasses import dataclass
from collections import Counter, defaultdict
from functools import cached_property
from itertools import accumulate
import json
from pathlib import Path
import re
import sys
import tempfile

from clang import cindex
from language_compiled_oracle import PROJECTS, CertifiedUnit, Probes, configure_clang, configure_units, sample, source_change
from language_fixture_positions import utf16_prefix
from language_conformance import ROOT, checkout

CONTROLS = {
    "nanopb": (b"checkreturn", b" " * len("checkreturn")),
    # The pinned cJSON.h declares typedef int cJSON_bool.
    "cjson": (b"cJSON_bool", b"int".ljust(len("cJSON_bool"))),
}


class EditSource:
    """Physical line starts in the immutable bytes used for independent interventions."""
    def __init__(self, data):
        self.data = data

    @cached_property
    def index(self):
        lines = self.data.splitlines(keepends=True)
        return lines, [0, *accumulate(map(len, lines))]

    def replacement(self, name, site, project):
        """Return bytes with only the immediate preceding written token replaced."""
        line, col = map(int, site.split(":"))
        lines, starts = self.index
        text = lines[line - 1]
        prefix = utf16_prefix(text.rstrip(b"\r\n").decode(), col).encode()
        if text[len(prefix):len(prefix) + len(name)] != name.encode():
            raise ValueError(f"Clang anchor does not identify {name} at {site}")
        old, new = CONTROLS[project]
        match = re.search(rb"(?<![a-zA-Z0-9_])" + old + rb"(?=[ \t]+$)", prefix)
        if not match:
            return None
        offset = starts[line - 1] + match.start()
        return self.data[:offset] + new + self.data[offset + len(old):]


def replacement(source, name, site, project):
    return EditSource(source).replacement(name, site, project)


def probe_rows(defs, calls):
    old_defs, old_calls = set(sample(defs)), set(sample(calls))
    rows = [dict(op="symbol", name=n, site=d, target=d, sampled=(n, d) in old_defs) for n, d in defs]
    rows += [dict(op="definition", name=n, site=s, target=d, sampled=(n, s, d) in old_calls)
             for n, s, d in calls]
    return rows


def baseline_groups(rows, observe):
    groups = defaultdict(list)
    for row in rows:
        diff, exclusion = observe(row)
        row["baseline_diff"] = diff
        row["verdict"] = "unverified" if exclusion else "untested_failure" if diff else "baseline_pass"
        if exclusion:
            row["reason"] = exclusion
        groups[(row["name"], row["target"])].append(row)
    return groups


def control_verdict(row, diff, exclusion):
    row["control_diff"] = diff
    if exclusion:
        row["reason"] = exclusion
        return "unverified"
    if row["baseline_diff"]:
        return "still_fails" if diff else "recovered"
    return "new_failure" if diff else "retained_pass"


@dataclass
class UnitReplay:
    probes: Probes
    filename: str
    unit: CertifiedUnit

    def reader(self, probes):
        def observe(row):
            subject = row["name"] if row["op"] == "symbol" else self.filename + ":" + row["site"]
            return probes.request(row["op"], subject, self.filename + ":" + row["target"], "control")
        return observe

    def recertify(self, change, related):
        try:
            with source_change(self.unit.path, change):
                verified = self.probes.recertify(self.unit)
                if verified is None:
                    raise ValueError("Clang's eligible definition or direct-binding locations changed")
                apply_verdicts(related, self.reader(verified))
        except (ValueError, RuntimeError, cindex.TranslationUnitLoadError) as exc:
            for row in related:
                row["verdict"] = "unverified"
                row["reason"] = str(exc)

    def replay(self, project, editable):
        rows = probe_rows(*self.unit.sites)
        for site, related in baseline_groups(rows, self.reader(self.probes)).items():
            self.replay_group(editable, project, site, related)
        return rows

    def replay_group(self, editable, project, site, related):
        failures = [row for row in related if row["verdict"] == "untested_failure"]
        if not failures:
            return
        changed = editable.replacement(*site, project)
        if changed is None or changed == editable.data:
            for row in failures:
                row["verdict"] = "outside_control"
            return
        self.recertify(changed, related)


def apply_verdicts(related, observe):
    for row in related:
        if row["verdict"] != "unverified":
            row["verdict"] = control_verdict(row, *observe(row))


def project_replays(project, binary):
    url, commit, files, includes = PROJECTS[project]
    with tempfile.TemporaryDirectory() as tmp:
        repo, home = Path(tmp) / "repo", Path(tmp) / "home"
        repo.mkdir()
        home.mkdir()
        checkout({"url": url, "commit": commit}, repo, ROOT / "target/conformance-cache")
        flags = configure_units(project, repo, files, includes)
        for filename in files:
            yield project + "/" + filename, unit_report(
                Probes(binary, repo, home), filename, flags[filename], (project, commit),
            )


def unit_report(probes, filename, flags, pin):
    project, commit = pin
    report = dict(commit=commit)
    try:
        # Read the source before asking the compiler to match the baseline error order.
        path = probes.repo / filename
        editable = EditSource(path.read_bytes())
        unit = CertifiedUnit(path, flags)
        rows = UnitReplay(probes, filename, unit).replay(project, editable)
    except (ValueError, RuntimeError, cindex.TranslationUnitLoadError) as exc:
        report["unverified"] = str(exc)
        return report
    report["rows"] = rows
    report["totals"] = dict(Counter(r["verdict"] for r in rows))
    report["original_sample"] = dict(Counter(r["verdict"] for r in rows if r["sampled"]))
    print(filename, report["totals"], "original sample", report["original_sample"], flush=True)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-c-residual-controls.json")
    args = parser.parse_args()
    configure_clang()
    results = {}
    for project in CONTROLS:
        results.update(project_replays(project, args.binary.resolve()))
    args.report.parent.mkdir(exist_ok=True, parents=True)
    args.report.write_text(json.dumps(results, indent=2) + "\n")
    return int(any(u.get("unverified") or any(r["verdict"] in ("unverified", "new_failure")
                   for r in u.get("rows", [])) for u in results.values()))


if __name__ == "__main__":
    raise SystemExit(main())
