"""Run independently labelled language queries against the built cx binary."""
import argparse
from collections import Counter
from functools import cache
from language_response import (
    ExpectedLocations, binding_targets, check_evidence, check_response, compare_rows,
    context_targets, expectation, relationship_envelope, site_locations, symbol_error,
    verify_comparison,
)
from language_fixture_positions import SourceLines, utf16_prefix
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests/fixtures/languages"
OPERATIONS = {"symbol", "definition", "references", "callers"}


def git(*args, cwd):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, timeout=180)


def cached_revision(url, commit, cache):
    key = hashlib.sha256(url.encode()).hexdigest()
    mirror = cache / key
    # Reuse an existing repository cache read-only. New entries are immutable
    # per revision and published only after git has fetched the complete input.
    if not (mirror.is_dir() and subprocess.run(
        ["git", "cat-file", "-e", commit + "^{commit}"], cwd=mirror, capture_output=True
    ).returncode == 0):
        cache.mkdir(parents=True, exist_ok=True)
        mirror = cache / (key + "-" + commit)
        if not mirror.is_dir():
            with tempfile.TemporaryDirectory(dir=cache) as temp:
                ready = Path(temp) / "mirror"
                ready.mkdir()
                git("init", "-q", "--bare", cwd=ready)
                git("fetch", "--depth=1", url, commit, cwd=ready)
                try:
                    ready.rename(mirror)
                except OSError:
                    if not mirror.is_dir():
                        raise
    return mirror


def checkout(spec, destination, cache):
    url, commit = spec["url"], spec["commit"]
    if not re.fullmatch(r"https://github\.com/[\w.-]+/[\w.-]+", url) or not re.fullmatch("[a-f0-9]{40}", commit):
        raise ValueError("public cases require a GitHub HTTPS URL and full commit")
    mirror = cached_revision(url, commit, cache)
    # Only run cx on upstream source. No upstream build or setup scripts.
    git("init", "-q", cwd=destination)
    git("fetch", "-q", "--depth=1", str(mirror), commit, cwd=destination)
    git("checkout", "-q", "--detach", "FETCH_HEAD", cwd=destination)


def read_lines(path):
    return SourceLines(path.read_bytes())


class QuerySession:
    """Queries against one version of a checkout and its index home."""
    def __init__(self, binary, repo, home):
        self.binary = binary
        self.repo = repo
        self.home = home
        self.read_source = cache(read_lines)

    def command(self, op, subject):
        if op == "symbol":
            return [self.binary, "--symbol", subject, "--json"]
        position = subject.rsplit(":", 2)
        if op == "references":
            path, line, column = position
            source = self.read_source(self.repo / path).at(int(line))
            prefix = utf16_prefix(source, int(column))
            return [self.binary, "--reference", path, line, str(len(prefix.encode()) + 1), "--json"]
        return [self.binary, "--cx", op, *position]


    def advanced(self, op, position):
        return run_command([self.binary, "--cx", op, *position.rsplit(":", 2)], self.repo, self.home)

    def request(self, op, subject, timeout=120):
        command = self.command(op, subject)
        return run_command(command, self.repo, self.home, timeout=timeout)

    def __call__(self, query, phase):
        proc = self.request(query["op"], query["subject"])
        if not proc.stdout.strip() or proc.returncode not in (0, 1, 2):
            return {"error": proc.stderr.strip() or f"cx exited {proc.returncode}"}
        try:
            return check_response(proc, query, phase)
        except (ValueError, TypeError, KeyError, IndexError) as exc:
            return {"response_error": f"{type(exc).__name__}: {exc}"}


# Existing fixture tools use this factory to obtain a callable for one source version.
scoped_query = QuerySession


def run_command(command, repo, home, timeout=120):
    return subprocess.run(
        command, cwd=repo, text=True, capture_output=True, timeout=timeout,
        # A relative Git config selects the standalone process path on macOS.
        env={**os.environ, "HOME": str(home), "GIT_CONFIG_GLOBAL": "./.missing-config", "GIT_CONFIG_NOSYSTEM": "1"},
    )


def expectations(file):
    for line in file.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        yield parse_expectation(line)


def parse_expectation(line):
    op, subject, expected, *opts = line.split("\t")
    probe = op.startswith("?")
    op = op.removeprefix("?")
    status = opts[0] if opts else ""
    if op not in OPERATIONS:
        raise ValueError(f"invalid test row: {line}")
    subset = parse_inclusion(opts[1:], expected, line)
    return dict(op=op, probe=probe, subject=subject, expected=expected, status=status, subset=subset)


def parse_inclusion(inclusion, expected, line):
    subset = inclusion == ["contains"]
    if inclusion and not subset:
        raise ValueError(f"invalid test row: {line}")
    if subset and expected == "-":
        raise ValueError("an inclusion check needs at least one verified location")
    return subset


def query_difference(run_query, row, phase):
    try:
        return run_query(row, phase)
    except subprocess.TimeoutExpired as exc:
        return {"response_error": str(exc)}


def observe(run_query, row, phase):
    return {"phase": phase, **row, "diff": query_difference(run_query, row, phase)}


def published_symbol(row):
    checks = (
        row["phase"] == "fresh", row["op"] == "symbol", row["expected"] != "-",
        not row["diff"], not row["probe"],
    )
    return all(checks)


def check_deleted_sources(case, repo, run_query, records):
    # Deletion and restoration have independent answers for any written declaration.
    symbols = list(filter(published_symbol, records))
    if not symbols:
        return
    source = case / "repo"
    for file in filter(Path.is_file, source.rglob("*")):
        (repo / file.relative_to(source)).unlink()
    observe_symbols(run_query, records, symbols, "deleted")
    shutil.copytree(source, repo, dirs_exist_ok=True)
    observe_symbols(run_query, records, symbols, "restored")


def observe_symbols(run_query, records, symbols, phase):
    for row in symbols:
        changed = {**row, "expected": "-" if phase == "deleted" else row["expected"],
                   "subset": phase == "restored" and row["subset"]}
        changed["phase"] = phase
        changed["diff"] = query_difference(run_query, {**changed, "status": ""}, phase)
        records.append(changed)


def apply_edit(case, repo):
    deletions = case / "delete.txt"
    if deletions.exists():
        for path in deletions.read_text().splitlines():
            target = (repo / path).resolve()
            if repo.resolve() not in target.parents:
                raise ValueError(f"invalid deletion path {path}")
            target.unlink()
    shutil.copytree(case / "edit", repo, dirs_exist_ok=True)


def prepare_fixture(case, repo, cache):
    if (case / "public.json").exists():
        repo.mkdir()
        checkout(json.loads((case / "public.json").read_text()), repo, cache)
    else:
        shutil.copytree(case / "repo", repo)
        git("init", "-q", cwd=repo)


def run_case(case, binary, cache):
    records = []
    with tempfile.TemporaryDirectory(prefix="cx-language-") as tmp:
        tmp = Path(tmp)
        repo, home = tmp / "repo", tmp / "home"
        home.mkdir()
        prepare_fixture(case, repo, cache)
        run_query = QuerySession(binary, repo, home)
        rows = list(expectations(case / "checks.tsv"))
        for phase in ("fresh", "reopen"):
            records.extend(observe(run_query, row, phase) for row in rows)
        if (case / "repo").is_dir():
            check_deleted_sources(case, repo, run_query, records)
        if (case / "edit").is_dir():
            apply_edit(case, repo)
            run_query = QuerySession(binary, repo, home)
            edited = list(expectations(case / "edited.tsv"))
            records.extend(observe(run_query, row, "update") for row in edited)
            clean = tmp / "clean-home"
            clean.mkdir()
            clean_query = QuerySession(binary, repo, clean)
            records.extend(observe(clean_query, row, "clean-after-edit") for row in edited)
    return records


def selected_fixtures(paths, public_cases):
    selected = {}
    for path in paths:
        public = (path.parent / "public.json").exists()
        if public_cases or not public:
            selected[path.parent] = public
    return selected


def fixture_gaps(profile, selected, public_cases):
    for public in ([False, True] if public_cases else [False]):
        if public not in selected.values():
            yield (f"{profile}: missing {'public' if public else 'synthetic'} case")
    if not any((case / "edited.tsv").is_file() and (case / "edit").is_dir() for case in selected):
        yield (f"{profile}: missing edit case")


class ProfileResults:
    """Accumulate coverage only from completed cases; probes never cover a required operation."""
    def __init__(self):
        self.operations = set()
        self.phases = Counter()
        self.failures = Counter()

    def record(self, case, rows):
        for row in rows:
            self.phases[row["phase"]] += 1
            if not row["probe"]:
                self.operations.add(row["op"])
            if row["diff"]:
                self.failures[row["probe"]] += 1
                print(case, row["phase"], row["op"], row["subject"], row["diff"], flush=True)

    def summary(self, gaps):
        required = self.failures[False]
        return {"sample_verdict": "fail" if required else "unverified" if gaps else "pass",
                "required_failures": required, "missed_probes": self.failures[True],
                "checks_by_phase": dict(self.phases), "unverified": gaps}


def evaluate_profile(profile, binary, public_cases):
    gaps, cases = [], {}
    paths = sorted((FIXTURES / profile).glob("*/checks.tsv"))
    selected = selected_fixtures(paths, public_cases)
    gaps.extend(fixture_gaps(profile, selected, public_cases))
    results = ProfileResults()
    for case in selected:
        key = str(case.relative_to(FIXTURES))
        try:
            rows = run_case(case, binary, ROOT / "target/conformance-cache")
            if not rows:
                raise ValueError("empty check file")
            cases[key] = rows
            results.record(key, rows)
        except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
            detail = getattr(exc, "stderr", None)
            gaps.append(f"{key}: {exc} {detail or ''}")
    for op in sorted(OPERATIONS - results.operations):
        gaps.append(f"{profile}: missing {op} checks")
    return cases, gaps, results.summary(gaps)


def registry_variants(directory):
    source = (directory / "mod.rs").read_text()
    prefix = "pub enum Language {"
    if prefix in source:
        body = source.split(prefix, 1)[1].split("}", 1)[0]
        return [line.strip().rstrip(",") for line in body.splitlines() if line.strip()]
    source = (directory / "analyzer.rs").read_text()
    _, start, body = source.partition("\nregistry! {")
    body, end, _ = body.partition("\n}")
    if not start or not end:
        raise ValueError("missing language registry definition")
    return re.findall(r"^\s*([A-Za-z][A-Za-z0-9_]*)\s*(?:\[[^]]*]\s*)?(?:=>|:)", body, re.M)


def registry_profiles(root=ROOT):
    variants = registry_variants(root / "src/domain/code")
    if not variants:
        raise ValueError("empty language registry")
    if len(set(variants)) != len(variants):
        raise ValueError("duplicate language registry entries")
    return {name.lower() for name in variants}


def replay_profiles(profiles, binary, public):
    cases, gaps, summaries = {}, [], {}
    for profile in profiles:
        results, missing, summary = evaluate_profile(profile, binary, public)
        cases.update(results)
        gaps.extend(missing)
        summaries[profile] = summary
    return cases, gaps, summaries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--public", action="store_true", help="include pinned public checkouts")
    parser.add_argument("--language", action="append", help="run only the named lowercase registry profile")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-conformance.json")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    args = parser.parse_args()
    profiles = registry_profiles()
    if args.language and set(args.language) - profiles:
        parser.error("unknown registry profile")
    verify_comparison()
    cases, gaps, summaries = replay_profiles(sorted(set(args.language or profiles)), args.binary.resolve(), args.public)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps({"profiles": summaries, "cases": cases, "unverified": gaps}, indent=2) + "\n")
    checks = sum(len(rows) for rows in cases.values())
    failures = sum(s["required_failures"] for s in summaries.values())
    missed_probes = sum(s["missed_probes"] for s in summaries.values())
    print(*gaps, sep="\n")
    print(f"{len(cases)} cases, {checks} checks, {failures} failures, {missed_probes} missed coverage probes, {len(gaps)} unverified; {args.report}")
    return int(bool(failures or gaps))


if __name__ == "__main__":
    raise SystemExit(main())
