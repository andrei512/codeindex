"""Generate isolated dev controls. No repository input or network access."""

import argparse
import ast
import json
import sys
import os
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from score_response import scored_paths, observed_findings
from nesting_cases import cases
from nesting_coverage import find_failures

def python_control_owner(source):
    # In the deep test and setup variants, exactly one method contains branch logic.
    functions = (node for node in ast.walk(ast.parse(source)) if isinstance(node, ast.FunctionDef))
    owner, = (fn for fn in functions if any(isinstance(node, ast.If) for node in ast.walk(fn)))
    return owner.name, owner.lineno


@dataclass(frozen=True)
class Variant:
    path: str
    framework: str
    scenario: str
    depth: int
    source: str

    def materialize(self, root, keys):
        if self.path in keys:
            raise ValueError(f"duplicate nesting fixture: {self.path}")
        dest = root / self.path
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(self.source)
        owner = None
        if self.path.endswith(".py") and self.scenario in ("test", "setup"):
            owner = python_control_owner(self.source)
        keys[self.path] = (
            self.framework, self.scenario, self.depth, len(self.source.splitlines()), owner,
        )


def variants():
    frameworks = set()
    for n, case in enumerate(cases()):
        if case.framework in frameworks:
            raise ValueError(f"duplicate framework in nesting panel: {case.framework}")
        frameworks.add(case.framework)
        yield from case_variants(n, case)


def case_variants(ordinal, case):
    for scenario, depth, source in case.variants():
        path = f"{ordinal}/{scenario}/{depth}/{case.path}"
        yield Variant(path, case.framework, scenario, depth, source)


def write_cases(root):
    keys = {}
    for variant in variants():
        variant.materialize(root, keys)
    return keys


def checked_nesting_value(finding, path):
    value = finding["value"]
    if type(value) is not int:
        raise ValueError(f"invalid nesting value for {path}: {finding}")
    return value


def check_nesting_owners(path, owners, expected):
    # Separate declaration or unit findings cannot satisfy this fixture.
    if expected and owners:
        if owners != {expected}:
            raise ValueError(f"expected nesting owner {expected} in {path}, got {owners}")
    if len(owners) > 1:
        raise ValueError(f"nesting findings have different owners in {path}: {owners}")


def nesting_findings(row, lines, owner=None):
    if row["score"] is None:
        return None
    findings = []
    owners = set()
    rules = ("nesting depth", "nested regions")
    for finding in observed_findings(row["findings"], lines, rules):
        value = checked_nesting_value(finding, row["path"])
        owners.add((finding["unit"], finding["line"]))
        findings.append((finding["rule"], value))
    check_nesting_owners(row["path"], owners, owner)
    return sorted(findings)


def run(binary, indexed):
    result = {}
    with tempfile.TemporaryDirectory(prefix="cx-nesting-dev-") as directory:
        root = Path(directory) / "sources"
        root.mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        env.update(HOME=directory, XDG_CONFIG_HOME=directory)
        if indexed:
            subprocess.run(["git", "init", "-q"], cwd=root, env=env, check=True)
        keys = write_cases(root)
        answer = subprocess.check_output([binary, "--score", "--json"], cwd=root, env=env)
        rows = scored_paths(json.loads(answer), keys)
        for path, row in rows.items():
            f, s, d, lines, owner = keys[path]
            result.setdefault(f, {}).setdefault(s, {})[d] = nesting_findings(row, lines, owner)
    return result




if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, default=Path("target/release/cx"))
    p.add_argument("--output", type=Path, required=True)
    p.add_argument(
        "--report-only",
        action="store_true",
        help="Record known baseline failures without exiting nonzero",
    )
    p.add_argument(
        "--indexed",
        action="store_true",
        help="Use a temporary Git repository and isolated index",
    )
    args = p.parse_args()
    results = run(args.binary.resolve(), args.indexed)
    failures = find_failures(results)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(dict(results=results, failures=failures), indent=2) + "\n"
    )
    print(
        len(results), "framework/dialect combinations,", len(failures), "failed pairs"
    )
    for fail in failures:
        print(fail)
    if failures and not args.report_only:
        raise SystemExit(1)
