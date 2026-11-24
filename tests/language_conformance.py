"""Run independently labelled language queries against the built cx binary."""
import argparse
from collections import Counter
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


def checkout(spec, destination, cache):
    url, commit = spec["url"], spec["commit"]
    if not re.fullmatch(r"https://github\.com/[\w.-]+/[\w.-]+", url) or not re.fullmatch("[a-f0-9]{40}", commit):
        raise ValueError("public cases require a GitHub HTTPS URL and full commit")
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
    # Only run cx on upstream source. No upstream build or setup scripts.
    git("init", "-q", cwd=destination)
    git("fetch", "-q", "--depth=1", str(mirror), commit, cwd=destination)
    git("checkout", "-q", "--detach", "FETCH_HEAD", cwd=destination)


def check(binary, repo, home, op, subject, expected, status, subset, phase):
    args = [subject] if op == "symbol" else subject.rsplit(":", 2)
    if op == "symbol":
        command = [binary, "--symbol", subject, "--json"]
    elif op == "references":
        path, line, utf16_col = args
        source = (repo / path).read_bytes().split(b"\n")[int(line) - 1].decode()
        prefix = source.encode("utf-16-le")[:2 * (int(utf16_col) - 1)].decode("utf-16-le")
        command = [binary, "--reference", path, line, str(len(prefix.encode()) + 1), "--json"]
    else:
        command = [binary, "--cx", op, *args]
    proc = subprocess.run(
        command, cwd=repo, text=True, capture_output=True, timeout=120,
        # A relative Git config selects the standalone process path on macOS.
        env={**os.environ, "HOME": str(home), "GIT_CONFIG_GLOBAL": "./.missing-config", "GIT_CONFIG_NOSYSTEM": "1"},
    )
    if not proc.stdout.strip() or proc.returncode not in (0, 1, 2):
        return {"error": proc.stderr.strip() or f"cx exited {proc.returncode}"}
    try:
        return check_response(proc, op, subject, expected, status, subset, phase)
    except (ValueError, TypeError, KeyError, IndexError) as exc:
        return {"response_error": f"{type(exc).__name__}: {exc}"}


def check_response(proc, op, subject, expected, status, subset, phase):
    answer = json.loads(proc.stdout)
    rows = answer["hits" if op == "symbol" else "result"]
    diff = compare(answer, op, expected, status, subset)
    if op == "symbol" and any(row.get("name") != subject for row in rows):
        diff["names"] = [row.get("name") for row in rows]
    if phase == "reopen" and answer.get("reused") is not True:
        diff["reopen"] = "expected to reuse the published generation"
    if op in ("symbol", "references"):
        want_exit = 2 if answer.get("operation_status") in ("unsupported", "unindexed") else int(not rows)
        if proc.returncode != want_exit:
            diff["exit"] = [want_exit, proc.returncode]
    elif proc.returncode:
        diff["exit"] = [0, proc.returncode]
    if op != "symbol":
        error = check_evidence(answer, op)
        if error:
            diff["evidence"] = error
    return diff


def check_evidence(answer, op):
    """Every reported relationship must point back to a binding in its context."""
    targets = {
        (context["context"], target)
        for context in answer["contexts"]
        for binding in context["bindings"]
        for target in binding["targets"]
    }
    rows = answer["result"]
    supported = set()
    role = {"definition": "definition", "references": "reference", "callers": "call_target"}[op]
    for item in answer["evidence"]:
        hit = item["hit"]
        if type(hit) is not int or not 0 <= hit < len(rows):
            return "evidence points outside the result list"
        if item["role"] != role or (item["context"], item["target"]) not in targets:
            return "evidence disagrees with the operation or selected binding"
        if op != "callers" and rows[hit]["symbol"] != item["target"]:
            return "hit and evidence name different targets"
        supported.add(hit)
    if len(supported) != len(rows):
        return "a reported location has no supporting evidence"


def compare(answer, op, expected, status, subset):
    rows = answer["hits" if op == "symbol" else "result"]
    want = Counter([] if expected == "-" else expected.split(","))
    with_col = any(len(site.rsplit(":", 2)) == 3 for site in want)
    check_label = any("|" in site for site in want)
    got = Counter()
    for row in rows:
        site = f"{row['path']}:{row['line']}"
        if with_col:
            site += f":{row['col']}"
        if check_label:
            site += "|" + str(row["kind" if op == "symbol" else "caller"])
        got[site] += 1
    diff = {}
    if want - got:
        diff["missing"] = list((want - got).elements())
    if not subset and got - want:
        diff["unexpected"] = list((got - want).elements())
    if answer.get("truncated") and not subset:
        diff["truncated"] = True
    status, _, coverage = status.partition("/")
    if coverage and {ctx["coverage"] for ctx in answer["contexts"]} != {coverage}:
        diff["coverage"] = [coverage, [ctx["coverage"] for ctx in answer["contexts"]]]
    if status and status != answer.get("operation_status"):
        diff["status"] = [status, answer.get("operation_status")]
    return diff


def verify_comparison():
    answer = {"result": [{"path": "a", "line": 3, "col": 4}], "operation_status": "resolved",
              "contexts": [{"coverage": "partial"}]}
    def difference(answer):
        return compare(answer, "references", "a:3:4", "resolved/partial", False)
    if difference(answer):
        raise RuntimeError("comparator rejected the reference answer")
    for change in ({"result": []}, {"result": [{"path": "a", "line": 3, "col": 5}]},
                   {"contexts": [{"coverage": "complete"}]}, {"truncated": True}):
        if not difference({**answer, **change}):
            raise RuntimeError(f"comparator missed deliberate mutation {change}")

    graph = {
        "result": [{"symbol": "target"}],
        "contexts": [{"context": "c", "bindings": [{"targets": ["target"]}]}],
        "evidence": [{"hit": 0, "context": "c", "target": "target", "role": "reference"}],
    }
    if check_evidence(graph, "references"):
        raise RuntimeError("rejected consistent relationship evidence")
    for change in ({"evidence": []}, {"contexts": []}, {"result": [{"symbol": "decoy"}]}):
        if not check_evidence({**graph, **change}, "references"):
            raise RuntimeError(f"missed inconsistent relationship evidence {change}")


def run_checks(file, phase, binary, repo, home, records):
    for line in file.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        op, subject, expected, *opts = line.split("\t")
        probe = op.startswith("?")
        op = op.removeprefix("?")
        status = opts[0] if opts else ""
        subset = opts[1:] == ["contains"]
        if op not in OPERATIONS or len(opts) > 2 or (len(opts) == 2 and not subset):
            raise ValueError(f"invalid test row: {line}")
        if subset and expected == "-":
            raise ValueError("an inclusion check needs at least one verified location")
        try:
            diff = check(binary, repo, home, op, subject, expected, status, subset, phase)
        except subprocess.TimeoutExpired as exc:
            diff = {"response_error": str(exc)}
        records.append(dict(phase=phase, op=op, probe=probe, subject=subject, expected=expected, status=status, subset=subset, diff=diff))


def check_deleted_sources(case, binary, repo, home, records):
    # Deletion and restoration have independent answers for any written declaration.
    symbols = [row for row in records if row["phase"] == "fresh" and row["op"] == "symbol"
               and row["expected"] != "-" and not row["diff"] and not row["probe"]]
    if not symbols:
        return
    for file in (case / "repo").rglob("*"):
        if file.is_file():
            (repo / file.relative_to(case / "repo")).unlink()
    for phase in ("deleted", "restored"):
        if phase == "restored":
            shutil.copytree(case / "repo", repo, dirs_exist_ok=True)
        for row in symbols:
            expected = "-" if phase == "deleted" else row["expected"]
            subset = phase == "restored" and row["subset"]
            try:
                diff = check(binary, repo, home, "symbol", row["subject"], expected, "", subset, phase)
            except subprocess.TimeoutExpired as exc:
                diff = {"response_error": str(exc)}
            records.append({**row, "phase": phase, "expected": expected, "subset": subset, "diff": diff})


def run_case(case, binary, cache):
    records = []
    with tempfile.TemporaryDirectory(prefix="cx-language-") as tmp:
        tmp = Path(tmp)
        repo, home = tmp / "repo", tmp / "home"
        home.mkdir()
        if (case / "public.json").exists():
            repo.mkdir()
            checkout(json.loads((case / "public.json").read_text()), repo, cache)
        else:
            shutil.copytree(case / "repo", repo)
            git("init", "-q", cwd=repo)
        run_checks(case / "checks.tsv", "fresh", binary, repo, home, records)
        run_checks(case / "checks.tsv", "reopen", binary, repo, home, records)
        if (case / "repo").is_dir():
            check_deleted_sources(case, binary, repo, home, records)
        if (case / "edit").is_dir():
            deletions = case / "delete.txt"
            if deletions.exists():
                for path in deletions.read_text().splitlines():
                    target = (repo / path).resolve()
                    if repo.resolve() not in target.parents:
                        raise ValueError(f"invalid deletion path {path}")
                    target.unlink()
            shutil.copytree(case / "edit", repo, dirs_exist_ok=True)
            run_checks(case / "edited.tsv", "update", binary, repo, home, records)
            clean = tmp / "clean-home"
            clean.mkdir()
            run_checks(case / "edited.tsv", "clean-after-edit", binary, repo, clean, records)
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--public", action="store_true", help="include pinned public checkouts")
    parser.add_argument("--language", action="append", help="run only the named lowercase registry profile")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-conformance.json")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    args = parser.parse_args()
    registry = (ROOT / "src/domain/code/mod.rs").read_text().split("pub enum Language {", 1)[1].split("}", 1)[0]
    profiles = {v.strip().rstrip(",").lower() for v in registry.splitlines() if v.strip()}
    if args.language and set(args.language) - profiles:
        parser.error("unknown registry profile")
    verify_comparison()
    cases, gaps, summaries = {}, [], {}
    for profile in sorted(set(args.language or profiles)):
        prior_gaps = len(gaps)
        paths = sorted((FIXTURES / profile).glob("*/checks.tsv"))
        for public in ([False, True] if args.public else [False]):
            if not any((p.parent / "public.json").exists() == public for p in paths):
                gaps.append(f"{profile}: missing {'public' if public else 'synthetic'} case")
        if not any((p.parent / "edited.tsv").is_file() and (p.parent / "edit").is_dir() for p in paths):
            gaps.append(f"{profile}: missing edit case")
        tested_ops = set()
        for path in paths:
            case = path.parent
            if (case / "public.json").exists() and not args.public:
                continue
            key = str(case.relative_to(FIXTURES))
            try:
                rows = run_case(case, args.binary.resolve(), ROOT / "target/conformance-cache")
                if not rows:
                    raise ValueError("empty check file")
                cases[key] = rows
                tested_ops.update(r["op"] for r in rows if not r["probe"])
                for row in rows:
                    if row["diff"]:
                        print(key, row["phase"], row["op"], row["subject"], row["diff"], flush=True)
            except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
                detail = getattr(exc, "stderr", None)
                gaps.append(f"{key}: {exc} {detail or ''}")
        for op in sorted(OPERATIONS - tested_ops):
            gaps.append(f"{profile}: missing {op} checks")
        samples = [r for key, records in cases.items() if key.split("/")[0] == profile for r in records]
        failures = sum(bool(r["diff"]) and not r["probe"] for r in samples)
        summaries[profile] = {
            "sample_verdict": "fail" if failures else "unverified" if len(gaps) > prior_gaps else "pass",
            "required_failures": failures,
            "missed_probes": sum(bool(r["diff"]) and r["probe"] for r in samples),
            "checks_by_phase": dict(Counter(r["phase"] for r in samples)),
            "unverified": gaps[prior_gaps:],
        }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps({"profiles": summaries, "cases": cases, "unverified": gaps}, indent=2) + "\n")
    rows = [row for case in cases.values() for row in case]
    failures = sum(bool(row["diff"]) and not row["probe"] for row in rows)
    missed_probes = sum(bool(row["diff"]) and row["probe"] for row in rows)
    print(*gaps, sep="\n")
    print(f"{len(cases)} cases, {len(rows)} checks, {failures} failures, {missed_probes} missed coverage probes, {len(gaps)} unverified; {args.report}")
    return int(bool(failures or gaps))


if __name__ == "__main__":
    raise SystemExit(main())
