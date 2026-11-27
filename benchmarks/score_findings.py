"""Balanced, source-pinned development eval for function findings."""

# /// script
# requires-python = ">=3.11"
# dependencies = ["lizard==1.17.31", "pathspec==1.1.1", "pygments==2.21.0", "tree-sitter-language-pack==0.9.0", "tree-sitter==0.27.0"]
# ///
import argparse
from bisect import insort
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile

import lizard
from finding_grammars import measure
from score_sources import REPOS, TEST
from score_response import scored_paths, physical_lines, observed_findings

HERE = Path(__file__).resolve().parent
LIMITS = {"branch complexity": 9, "parameters": 5, "unit lines": 70}
EXCLUDED = {
    "vendor",
    "third_party",
    "third-party",
    "node_modules",
    "generated",
    "fixtures",
    "dist",
    "deps",
}
PER_REPO = 12


def sha(data):
    return hashlib.sha256(data).hexdigest()


def eligible_archive_path(member, extensions):
    path = Path(member.name.partition("/")[2])
    if not member.isfile() or not 100 <= member.size <= 70000:
        return None
    if path.suffix[1:] not in extensions:
        return None
    if path.is_absolute() or ".." in path.parts:
        return None
    if EXCLUDED.intersection(part.lower() for part in path.parts):
        return None
    return path


def ranked_members(tar, repo, extensions):
    for member in tar:
        if path := eligible_archive_path(member, extensions):
            yield sha(f"{repo}/{path}".encode()), path, member


def sample_archive(archive, repo, extensions):
    candidates = []
    with tarfile.open(archive, "r|gz") as tar:
        for key, path, member in ranked_members(tar, repo, extensions):
            if len(candidates) == 160 and key >= candidates[-1][0]:
                continue
            insort(candidates, (key, str(path), tar.extractfile(member).read()))
            del candidates[160:]
    return candidates


def measure_source(grammar, path, text, data):
    if grammar:
        count, findings, rules = measure(grammar, data)
        if not count:
            return None
        oracle = "tree-sitter:" + grammar
    else:
        return lizard_findings(path, text)
    return count, findings, rules, oracle


def lizard_findings(path, text):
    funcs = [
        f
        for f in lizard.analyze_file.analyze_source_code(
            path, text
        ).function_list
        if f.start_line > 0 and f.name != "*global*"
    ]
    if not funcs:
        return None
    findings = []
    for f in funcs:
        for rule, value in zip(
            LIMITS, (f.cyclomatic_complexity, f.parameter_count, f.nloc)
        ):
            if value >= LIMITS[rule]:
                findings.append(
                    dict(rule=rule, line=f.start_line, value=value, unit=f.name)
                )
    count = len(funcs)
    rules = list(LIMITS)
    oracle = "Lizard 1.17.31"
    return count, findings, rules, oracle


def fetch(project, folder):
    repo, rev = project["repository"], project["revision"]
    if repo.casefold() in {REPOS[n]["repository"].casefold() for n in TEST}:
        raise ValueError(f"Blind repository: {repo}")
    archive = folder / (rev + ".tgz")
    if not archive.exists():
        tmp = archive.with_suffix(".part")
        subprocess.run(
            [
                "curl",
                "-fsSL",
                "--retry",
                "3",
                "-o",
                str(tmp),
                f"https://codeload.github.com/{repo}/tar.gz/{rev}",
            ],
            check=True,
        )
        tmp.replace(archive)
    candidates = sample_archive(archive, repo, project["extensions"])
    return balance_cases(project, candidates)


def distinct_utf8_sources(candidates):
    seen = set()
    for _, path, data in candidates:
        try:
            text = data.decode()
        except UnicodeDecodeError:
            continue
        digest = sha(data)
        if digest not in seen:
            seen.add(digest)
            yield path, text, digest, data


def measured_sources(candidates, grammar):
    for path, text, digest, data in distinct_utf8_sources(candidates):
        measurement = measure_source(grammar, path, text, data)
        if measurement is None:
            continue
        yield path, text, digest, measurement


def balance_cases(project, candidates):
    repo, rev = project["repository"], project["revision"]
    buckets = [[], []]
    for path, text, digest, measurement in measured_sources(candidates, project.get("grammar")):
        count, findings, rules, oracle = measurement
        bucket = buckets[bool(findings)]
        if len(bucket) < PER_REPO:
            bucket.append(
                dict(
                    project=repo,
                    revision=rev,
                    language=project["language"],
                    path=path,
                    source_hash=digest,
                    functions=count,
                    oracle=oracle,
                    rules=rules,
                    findings=findings,
                    source=text,
                )
            )
        if min(map(len, buckets)) >= PER_REPO // 2:
            break
    selected = buckets[0][: PER_REPO // 2] + buckets[1][: PER_REPO // 2]
    selected += (buckets[0][PER_REPO // 2 :] + buckets[1][PER_REPO // 2 :])[
        : PER_REPO - len(selected)
    ]
    if len(selected) != PER_REPO:
        raise ValueError(f"{repo}: only {len(selected)} eligible cases")
    return selected


def development_cases(folder, projects):
    repos = Counter(p["language"] for p in projects)
    if len(set(repos.values())) != 1:
        raise ValueError("unbalanced language coverage")
    with ThreadPoolExecutor(5) as pool:
        return [case for block in pool.map(lambda project: fetch(project, folder), projects) for case in block]


def prepare(folder):
    projects = json.loads((HERE / "finding_projects.json").read_text())
    cases = development_cases(folder, projects)
    if len({c["source_hash"] for c in cases}) != len(cases):
        raise ValueError("Duplicate files across projects")
    body = "".join(json.dumps(c, sort_keys=True) + "\n" for c in cases)
    fingerprint = sha(body.encode())
    check_panel_lock(fingerprint)
    (folder / "panel.jsonl").write_text(body)
    return dict(
        files=len(cases),
        functions=sum(c["functions"] for c in cases),
        findings=sum(len(c["findings"]) for c in cases),
        sha256=fingerprint,
    )


def check_panel_lock(fingerprint):
    lock = HERE / "finding_panel.sha256"
    if lock.exists() and fingerprint != lock.read_text().strip():
        raise ValueError("Pinned panel or labels changed")


def panel_inputs(folder, controls):
    if controls:
        from finding_controls import cases as control_cases

        cases = list(control_cases())
        raw = json.dumps(cases, sort_keys=True).encode()
    else:
        raw = (folder / "panel.jsonl").read_bytes()
        if sha(raw) != (HERE / "finding_panel.sha256").read_text().strip():
            raise ValueError("Unexpected panel hash")
        cases = list(map(json.loads, raw.splitlines()))
    return cases, raw


def count_sites(summary, rules, expected, actual):
    for label, counts in (("matched", expected & actual), ("missed", expected - actual),
                          ("extra", actual - expected)):
        totals = Counter()
        for (rule, _line), count in counts.items():
            totals[rule] += count
        for rule in rules:
            summary[rule][label] += totals[rule]


def finding_sites(findings, rules, include_file, line_count):
    selected = observed_findings(findings, line_count, rules, include_file=include_file)
    return Counter((f["rule"], f["line"]) for f in selected)


def evaluate(folder, binary, controls=False):
    cases, raw = panel_inputs(folder, controls)
    indexed = {c["project"] + "/" + c["path"]: c for c in cases}
    if len(indexed) != len(cases):
        raise ValueError("duplicate score finding input path")
    summary = defaultdict(lambda: defaultdict(Counter))
    details = []
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "src"
        subprocess.run(["git", "init", "-q", root], check=True)
        for path, c in indexed.items():
            dest = root / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(c["source"])
        out = json.loads(
            subprocess.check_output(
                [binary.resolve(), "--score", "--ignore-test-rules", "--json"],
                cwd=root,
                env=dict(os.environ, HOME=td, XDG_CONFIG_HOME=td + "/.config"),
            )
        )
        by_path = scored_paths(out, indexed)
        for path, c in indexed.items():
            row = by_path[path]
            expected = Counter((f["rule"], f["line"]) for f in c["findings"])
            line_count = physical_lines(c["source"])
            actual = finding_sites(row["findings"], c["rules"], c.get("include_file", False), line_count)
            count_sites(summary[c["language"]], c["rules"], expected, actual)
            details.append(
                dict(path=path, expected=c["findings"], actual=row["findings"])
            )
    return dict(
        panel_sha256=sha(raw),
        binary_sha256=sha(binary.read_bytes()),
        files=len(cases),
        languages=summary,
        details=details,
    )


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("action", choices=["prepare", "run", "controls"])
    p.add_argument("--dir", type=Path, default=HERE.parent / "target/finding-eval")
    p.add_argument("--binary", type=Path, default=HERE.parent / "target/release/cx")
    p.add_argument("--output", type=Path)
    args = p.parse_args()
    args.dir.mkdir(parents=True, exist_ok=True)
    result = (
        prepare(args.dir)
        if args.action == "prepare"
        else evaluate(args.dir, args.binary, args.action == "controls")
    )
    body = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.write_text(body)
    else:
        print(body, end="")
