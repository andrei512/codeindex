# /// script
# requires-python = ">=3.9"
# dependencies = ["libclang==18.1.1"]
# ///
"""Causal triage on a deterministic sample of unresolved public Clang discrepancies.

Run after language_compiled_oracle.py, with the same Clang/SDK. Cases are ranked
before examining the new controls. A control counts only if Clang retains the
same written declarations and call targets and cx then produces the expected
answer. A non-recovery leaves that individual mismatch open.
"""
import argparse
from bisect import insort
from collections import Counter, defaultdict
from contextlib import ExitStack
from dataclasses import dataclass
from functools import cached_property, wraps
import hashlib
import json
from pathlib import Path
import re
from itertools import chain
from typing import Sequence
import tempfile

from clang import cindex
from language_compiled_oracle import CertifiedUnit, compile_command, configure_clang, query_rows, write_compile_commands
from language_clang_rewrites import active_guard_source, add_combined_control, written_controls
# Historical replay scripts import these control constructors from this module.
from language_clang_rewrites import Control, compose_lines
from language_conformance import ROOT, checkout, run_case


def exact_recoveries(rows, confirmed):
    identities = set(map(tuple, rows))
    if len(identities) != len(rows) or not identities <= confirmed:
        raise ValueError("duplicate or unsupported recovered compiler query")
    return identities


def recovered_queries(unit, failed):
    # Older reports identify recoveries by subject alone. Only unambiguous
    # subjects in that report can establish an exact recovery.
    controls = unit.get("compile_guards", {}), unit.get("digit_separators", {})
    if controls == ({}, {}):
        return set()
    confirmed = {key for key, row in failed.items() if not row.get("excluded")}
    recovered, legacy = set(), set()
    for control in controls:
        if "recovered_queries" in control:
            recovered.update(exact_recoveries(control["recovered_queries"], confirmed))
        else:
            legacy.update(control.get("recovered", ()))
    if legacy:
        recovered.update(legacy_recoveries(legacy, confirmed))
    return recovered


def legacy_recoveries(names, confirmed):
    """A subject-only certificate is valid only for a unique confirmed query."""
    by_subject = {}
    for key in confirmed:
        subject = key[1]
        by_subject[subject] = None if subject in by_subject else key
    return {key for name in names if (key := by_subject.get(name)) is not None}


def rank_unit_failures(unit, data, waves, groups):
    failed = {key: row for key, row in query_rows(unit, data["rows"]) if row["diff"]}
    recovered = recovered_queries(data, failed)
    project = unit.split("/", 1)[0]
    for key, row in failed.items():
        if key in recovered:
            continue
        ident = "|".join((unit, *key))
        rank = hashlib.sha256(ident.encode()).hexdigest()
        group = groups[(project, row["op"])]
        insert_ranked(group, (rank, unit, row), waves)
    return len(recovered)


def insert_ranked(group, candidate, limit):
    insort(group, candidate)
    if limit >= 0:
        del group[limit:]


def sample_rows(units, waves):
    groups = defaultdict(list)
    excluded = 0
    for unit, data in units.items():
        excluded += rank_unit_failures(unit, data, waves, groups)
    selected = defaultdict(list)
    for wave, candidate in chain.from_iterable(enumerate(candidates[:waves], 1) for candidates in groups.values()):
        _, unit, row = candidate
        selected[unit].append({"wave": wave, **row, "outcome": "unattributed"})
    return selected, excluded


def logical_lines(lines):
    start = 0
    for end, line in enumerate(lines, 1):
        if not line.rstrip("\r\n").endswith("\\") or end == len(lines):
            yield start, end
            start = end


class WrittenSource:
    """Physical lines of the original file, grouped across preprocessor continuations."""
    def __init__(self, text):
        self.text = text

    @cached_property
    def lines(self):
        return self.text.splitlines(keepends=True)

    @cached_property
    def directives(self):
        return tuple((start, end) for start, end in logical_lines(self.lines)
                     if self.lines[start].lstrip().startswith("#"))

    def replace_code(self, pattern, replacement):
        lines = self.lines.copy()
        tokens = re.compile(pattern)
        cursor = 0
        for start, end in chain(self.directives, [(len(lines), len(lines))]):
            for i in range(cursor, start):
                lines[i] = tokens.sub(replacement, lines[i])
            cursor = end
        return "".join(lines)


@dataclass(frozen=True)
class RewriteInputs(WrittenSource):
    """One untouched translation unit and its compiler-certified declaration sites."""
    project: str
    path: Path
    text: str
    flags: Sequence[str]
    definitions: Sequence[tuple[str, str]]

    def controls(self):
        return written_controls(self.project, self.path, self, (self.flags, self.definitions))


@wraps(RewriteInputs, assigned=(), updated=())
def rewrite(*args, **kwargs):
    """Legacy generator factory: rewrite(project, path, text, flags, definitions)."""
    return RewriteInputs(*args, **kwargs).controls()


def active_guards(path, text, flags):
    return active_guard_source(path, WrittenSource(text), flags)


def probe_rows(probes, rows, name):
    restored = []
    for row in rows:
        diff, censored = probes.check(row)
        if not diff and not censored:
            row["outcome"] = "recovered"
            row.setdefault("controls", []).append(name)
            restored.append(row["subject"])
    return {"recovered": restored}


@dataclass(frozen=True)
class Intervention:
    binary: Path
    repo: Path
    scratch: Path
    filename: str
    flags: tuple

    def configure(self, extra):
        command = compile_command(self.repo, self.filename, [*self.flags, *extra])
        write_compile_commands(self.repo, [command])

    def check_change(self, control, certified, home, rows):
        with ExitStack() as changeset:
            changeset.enter_context(control.applied(certified.path))
            changeset.callback(self.configure, [])
            self.configure(control.flags)
            try:
                probes = certified.probes(self.binary, self.repo, home, control.flags)
                if probes is None:
                    raise ValueError("compiler locations or bindings changed")
            except (ValueError, RuntimeError, cindex.TranslationUnitLoadError) as error:
                return {"unverified": str(error)}
            home.mkdir()
            return probe_rows(probes, rows, control.name)

    def audit(self, project, rows):
        self.configure([])
        path = self.repo / self.filename
        text = path.read_text()
        certified = CertifiedUnit(path, self.flags)
        inputs = RewriteInputs(project, path, text, self.flags, certified.sites[0])
        # Materialize controls before editing: some ask Clang about the original.
        changes = list(inputs.controls())
        add_combined_control(changes, inputs)
        return {
            control.name: self.check_change(control, certified, self.scratch / f"index-{index}", rows)
            for index, control in enumerate(changes) if control.source != text
        }


def evidence_for_unit(binary, unit, source, rows):
    project, filename = unit.split("/", 1)
    with tempfile.TemporaryDirectory() as directory:
        scratch = Path(directory)
        repo = scratch / "repo"
        repo.mkdir()
        checkout(source, repo, ROOT / "target/conformance-cache")
        flags = tuple(arg.replace("<checkout>", str(repo)) for arg in source["arguments"])
        intervention = Intervention(binary, repo, scratch, filename, flags)
        return {"controls": intervention.audit(project, rows), "rows": rows}


def minimized_witnesses(selected, binary):
    witnesses = [
        ("robin/src/test/app/benchmark.cpp", "src/test/app/benchmark.cpp:29:9", "anonymous_namespace"),
        ("kostya/brainfuck/bf.cpp", "brainfuck/bf.cpp:144:28", "direct_initialized_receiver"),
        ("double-conversion/double-conversion/bignum.cc", "double-conversion/bignum.cc:59:5", "const_method_overload"),
        ("kostya/brainfuck/bf.cpp", "brainfuck/bf.cpp:166:45", "temporary_receiver"),
    ]
    minimized = []
    for unit, subject, fixture in witnesses:
        selected_row = next((r for r in selected.get(unit, []) if r["subject"] == subject), None)
        if not selected_row:
            continue
        path = ROOT / "tests/fixtures/languages/cpp" / ("wave_clang_" + fixture)
        results = run_case(path, binary, ROOT / "target/conformance-cache")
        minimized.append({"case": str(path.relative_to(ROOT)), "unit": unit, "subject": subject,
                          "wave": selected_row["wave"], "original_site_outcome": selected_row["outcome"],
                          "failed_original_queries": sum(r["phase"] == "fresh" and bool(r["diff"]) for r in results),
                          "failed_control_queries": sum(r["phase"] == "update" and bool(r["diff"]) for r in results)})
    return minimized


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT / "target/language-compiled-oracle.json")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-compiled-residuals.json")
    parser.add_argument("--waves", type=int, default=6)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    opts = parser.parse_args()
    binary = opts.binary.resolve(strict=True)
    configure_clang()
    units = json.loads(opts.source.read_text())["units"]
    selected, excluded = sample_rows(units, opts.waves)
    evidence = {unit: evidence_for_unit(binary, unit, units[unit], rows) for unit, rows in selected.items()}
    minimized = minimized_witnesses(selected, binary)
    opts.report.parent.mkdir(parents=True, exist_ok=True)
    opts.report.write_text(json.dumps({"excluded_previously_recovered": excluded, "units": evidence,
                                       "minimized": minimized}, indent=2) + "\n")
    for wave in range(1, opts.waves+1):
        print(wave, Counter(r["outcome"] for v in evidence.values() for r in v["rows"] if r["wave"] == wave))


if __name__ == "__main__":
    main()
