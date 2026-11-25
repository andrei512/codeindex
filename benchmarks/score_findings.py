"""Balanced, source-pinned development eval for function findings."""

# /// script
# requires-python = ">=3.11"
# dependencies = ["lizard==1.17.31", "pathspec==1.1.1", "pygments==2.21.0", "tree-sitter-language-pack==0.9.0", "tree-sitter==0.27.0"]
# ///
import argparse
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
    candidates = []
    with tarfile.open(archive, "r|gz") as tar:
        for member in tar:
            path = Path(member.name.partition("/")[2])
            if (
                not member.isfile()
                or not 100 <= member.size <= 70000
                or path.suffix[1:] not in project["extensions"]
                or EXCLUDED.intersection(p.lower() for p in path.parts)
                or path.is_absolute()
                or ".." in path.parts
            ):
                continue
            key = sha(f"{repo}/{path}".encode())
            if len(candidates) == 160 and key >= candidates[-1][0]:
                continue
            candidates.append((key, str(path), tar.extractfile(member).read()))
            candidates.sort()
            del candidates[160:]
    buckets = [[], []]
    seen = set()
    for _, path, data in candidates:
        try:
            text = data.decode()
        except UnicodeDecodeError:
            continue
        if sha(data) in seen:
            continue
        seen.add(sha(data))
        if grammar := project.get("grammar"):
            count, findings, rules = measure(grammar, data)
            if not count:
                continue
            oracle = "tree-sitter:" + grammar
        else:
            funcs = [
                f
                for f in lizard.analyze_file.analyze_source_code(
                    path, text
                ).function_list
                if f.start_line > 0 and f.name != "*global*"
            ]
            if not funcs:
                continue
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
        bucket = buckets[bool(findings)]
        if len(bucket) < PER_REPO:
            bucket.append(
                dict(
                    project=repo,
                    revision=rev,
                    language=project["language"],
                    path=path,
                    source_hash=sha(data),
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


def prepare(folder):
    projects = json.loads((HERE / "finding_projects.json").read_text())
    repos = Counter(p["language"] for p in projects)
    assert len(set(repos.values())) == 1
    with ThreadPoolExecutor(5) as pool:
        cases = [
            c for block in pool.map(lambda p: fetch(p, folder), projects) for c in block
        ]
    if len({c["source_hash"] for c in cases}) != len(cases):
        raise ValueError("Duplicate files across projects")
    body = "".join(json.dumps(c, sort_keys=True) + "\n" for c in cases)
    fingerprint = sha(body.encode())
    lock = HERE / "finding_panel.sha256"
    if lock.exists() and fingerprint != lock.read_text().strip():
        raise ValueError("Pinned panel or labels changed")
    (folder / "panel.jsonl").write_text(body)
    return dict(
        files=len(cases),
        functions=sum(c["functions"] for c in cases),
        findings=sum(len(c["findings"]) for c in cases),
        sha256=fingerprint,
    )


def evaluate(folder, binary, controls=False):
    if controls:
        from finding_controls import cases as control_cases

        cases = list(control_cases())
        raw = json.dumps(cases, sort_keys=True).encode()
    else:
        raw = (folder / "panel.jsonl").read_bytes()
        if sha(raw) != (HERE / "finding_panel.sha256").read_text().strip():
            raise ValueError("Unexpected panel hash")
        cases = list(map(json.loads, raw.splitlines()))
    summary = defaultdict(lambda: defaultdict(Counter))
    details = []
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "src"
        subprocess.run(["git", "init", "-q", root], check=True)
        for c in cases:
            dest = root / c["project"] / c["path"]
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(c["source"])
        out = json.loads(
            subprocess.check_output(
                [binary.resolve(), "--score", "--json"],
                cwd=root,
                env=dict(os.environ, HOME=td, XDG_CONFIG_HOME=td + "/.config"),
            )
        )
        by_path = {f["path"]: f for f in out["files"]}
        for c in cases:
            path = c["project"] + "/" + c["path"]
            row = by_path[path]
            expected = Counter((f["rule"], f["line"]) for f in c["findings"])
            actual = Counter(
                (f["rule"], f["line"])
                for f in row["findings"]
                if f["rule"] in c["rules"]
                and (c.get("include_file") or f["unit"] != "<file>")
            )
            for rule in c["rules"]:
                stats = summary[c["language"]][rule]
                for key, counts in [
                    ("matched", expected & actual),
                    ("missed", expected - actual),
                    ("extra", actual - expected),
                ]:
                    stats[key] += sum(v for (r, line), v in counts.items() if r == rule)
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
