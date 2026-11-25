"""Evaluate cx test roles at frozen, independently labelled source locations.

Run: python3 benchmarks/test_roles/eval.py --binary target/release/cx
Only an explicit --split test evaluates held-out projects. Sources are cached
under target/test-role-eval. The cache and gold labels stay outside cx's input.
"""
import argparse
import collections
import concurrent.futures
import hashlib
import json
import os
import re
import subprocess
import tempfile
import urllib.parse
import urllib.request
from pathlib import Path, PurePosixPath

import testroutes

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
ROLES = ("production", "test", "test-support", "unknown")


def baseline(path):
    # Same file naming conventions as benchmarks/score_eval.py::path_role.
    p = PurePosixPath(path.lower())
    if (set(p.parts[:-1]) & {"test", "tests", "__tests__", "spec", "specs"}
            or p.stem.startswith("test_")
            or p.stem.endswith(("_test", ".test", ".spec", "tests", "testcase"))):
        return "test"
    return "production"


def checked_path(name):
    p = PurePosixPath(name)
    if p.is_absolute() or not p.parts or any(s in {".", "..", ".git"} for s in p.parts):
        raise ValueError(f"unsafe fixture path: {name!r}")
    return p


def read_manifests(paths, split):
    splits = {repo.lower(): project_split for project_split, _, repo, _ in testroutes.PROJECTS}
    rows = []
    by_digest = {}
    for path in paths:
        for line in path.read_text().splitlines():
            r = json.loads(line)
            repo = r["repository"].lower()
            if r["split"] not in {"dev", "test"} or splits.setdefault(repo, r["split"]) != r["split"]:
                raise ValueError(f"invalid or overlapping repository split: {repo}")
            if by_digest.setdefault(r["sha256"], r["split"]) != r["split"]:
                raise ValueError(f"identical source crosses repository splits: {repo}/{r['path']}")
            if split is None or r["split"] == split:
                rows.append(r)
    return rows


def get_source(cache, r):
    repo, revision, path, digest = (r[k] for k in ("repository", "revision", "path", "sha256"))
    checked_path(path)
    if (not re.fullmatch(r"[\w.-]+/[\w.-]+", repo) or not re.fullmatch(r"[a-f0-9]{40}", revision)
            or not re.fullmatch(r"[a-f0-9]{64}", digest)):
        raise ValueError(f"invalid pin for {repo}/{path}")
    blob = cache / "blobs" / digest
    if blob.exists():
        data = blob.read_bytes()
    else:
        url = f"https://raw.githubusercontent.com/{repo}/{revision}/{urllib.parse.quote(path)}"
        with urllib.request.urlopen(url, timeout=60) as stream:
            data = stream.read()
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError(f"source hash mismatch: {repo}/{path}")
    if not blob.exists():
        with tempfile.NamedTemporaryFile(dir=blob.parent, delete=False) as tmp:
            tmp.write(data)
        Path(tmp.name).replace(blob)
    return data


def classify(default, ranges, line):
    for region in reversed(ranges):
        if region["start_line"] <= line <= region["end_line"]:
            return region["role"]
    return default


def mixed_roles(data, start, end, default, spans):
    """Distinct effective roles on non-whitespace bytes; catches default-role gaps too."""
    bounds = sorted({start, end} | {n for r in spans for n in (r["start_byte"], r["end_byte"])
                                    if start < n < end})
    seen = set()
    for left, right in zip(bounds, bounds[1:]):
        if not data[left:right].strip():
            continue
        role = next((r["role"] for r in reversed(spans) if r["start_byte"] <= left < r["end_byte"]), default)
        seen.add(role)
        if len(seen) > 1:
            return True
    return False


def metrics(pairs):
    matrix = collections.Counter(pairs)
    def rate(n, d):
        return round(n / d, 5) if d else None
    measured = [(gold, pred) for gold, pred in pairs if gold != "unknown"]
    per_role = {}
    for role in ROLES[:-1]:
        right = matrix[role, role]
        per_role[role] = {
            "precision": rate(right, sum(pred == role for _, pred in measured)),
            "recall": rate(right, sum(gold == role for gold, _ in measured)),
        }
    test_roles = {"test", "test-support"}
    return {
        "test_or_support_recall": rate(sum(a in test_roles and b in test_roles for a, b in measured),
                                       sum(a in test_roles for a, _ in measured)),
        "production_mislabelled_as_test": sum(a == "production" and b in test_roles for a, b in measured),
        "labels": len(pairs),
        "known_labels": len(measured),
        "correct_known": sum(a == b for a, b in measured),
        "abstained_on_known": sum(b == "unknown" for _, b in measured),
        "accuracy_known": rate(sum(a == b for a, b in measured), len(measured)),
        "roles": per_role,
        "confusion_gold_to_prediction": {
            g: {p: matrix[g, p] for p in ROLES if matrix[g, p]} for g in ROLES if any(a == g for a, _ in pairs)
        },
    }


def load_panel(rows, cache):
    # One vote per labelled line. Never use two versions of a project to evaluate the same split.
    projects, revisions = {}, {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as workers:
        for r, data in zip(rows, workers.map(lambda r: get_source(cache, r), rows)):
            repo, rev, path = r["repository"].lower(), r["revision"], r["path"]
            if revisions.setdefault(repo, rev) != rev:
                raise ValueError(f"repository has two revisions: {repo}")
            files = projects.setdefault(repo, {})
            row = files.setdefault(path, {"language": r["language"], "revision": rev, "data": data, "labels": {}})
            if row["data"] != data or row["language"] != r["language"]:
                raise ValueError(f"conflicting file rows: {repo}/{path}")
            if not r["labels"] and r.get("context") is not True:
                raise ValueError(f"empty label list without context=true: {repo}/{path}")
            if r.get("context") and r["labels"]:
                raise ValueError(f"context-only file also has labels: {repo}/{path}")
            line_count = data.count(b"\n") + int(bool(data) and not data.endswith(b"\n"))
            for line, role, evidence in r["labels"]:
                if type(line) is not int or not 1 <= line <= line_count or role not in ROLES or not evidence:
                    raise ValueError(f"invalid label in {repo}/{path} at {line}")
                old = row["labels"].setdefault(line, (role, evidence))
                if old[0] != role:
                    raise ValueError(f"conflicting labels: {repo}/{path}:{line}")
    return projects


def evaluate(projects, binary):
    samples = []
    for repo, files in sorted(projects.items()):
        with tempfile.TemporaryDirectory(prefix="cx-test-roles-") as directory:
            root = Path(directory, "repo")
            home = Path(directory, "home")
            root.mkdir()
            home.mkdir()
            env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
            # A relative Git config requires cx to run in this process (the
            # macOS watcher lives at /). The named file exists only in the
            # disposable HOME. No process or index survives the experiment.
            (home / "gitconfig").touch()
            env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
                       GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="../home/gitconfig")
            for path, r in files.items():
                dest = root / checked_path(path)
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(r["data"])
            # Every labelled input is tracked in its public source revision.
            # Preserve that property, including intentionally hidden fixtures.
            for git_args in (["init", "--quiet", "--template="], ["add", "-f", "--", "."],
                             ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                              "commit", "--quiet", "-m", "Pinned evaluation inputs"]):
                subprocess.run(["git", *git_args], cwd=root, env=env, check=True, capture_output=True)
            proc = subprocess.run([binary, "--score", "--roles", "--json"], cwd=root, env=env,
                                  capture_output=True, check=False)
            if proc.returncode:
                raise RuntimeError(f"cx failed for {repo} (exit {proc.returncode}): {proc.stderr.decode(errors='replace')}")
            actual = {f["path"]: f for f in json.loads(proc.stdout)["files"]}
            for path, r in sorted(files.items()):
                output = actual.get(path)
                if output:
                    if output["default_role"] not in ROLES:
                        raise ValueError(f"invalid default role in {repo}/{path}")
                    for region in output["regions"]:
                        if (region["role"] not in ROLES
                                or not 1 <= region["start_line"] <= region["end_line"]):
                            raise ValueError(f"invalid region in {repo}/{path}: {region}")
                # The gold labels identify lines. Two declarations may share
                # one line, or a method may start halfway through a class line.
                line_starts = [0, *(i + 1 for i, b in enumerate(r["data"]) if b == 10), len(r["data"])]
                for line, (gold, evidence) in sorted(r["labels"].items()):
                    spans = [] if not output else [region for region in output["regions"]
                                                    if region["start_line"] <= line <= region["end_line"]]
                    ambiguity = bool(output and mixed_roles(r["data"], line_starts[line - 1], line_starts[line],
                                                            output["default_role"], spans))
                    prediction = classify(output["default_role"], output["regions"], line) if output else "unknown"
                    samples.append(dict(repository=repo, revision=r["revision"], path=path, line=line, language=r["language"],
                                        expected=gold, cx=prediction, baseline=baseline(path),
                                        mixed_line=ambiguity, file_returned=output is not None, evidence=evidence))
    return samples


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--cache", type=Path, default=ROOT / "target/test-role-eval")
    parser.add_argument("--split", choices=("dev", "test"), default="dev")
    parser.add_argument("--manifest", type=Path, action="append", help="override curated JSONL input; repeatable")
    parser.add_argument("--without-testroutes", action="store_true")
    parser.add_argument("--coverage", action="store_true",
                        help="report frozen split membership without accessing any source or running cx")
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--output", type=Path, help="defaults to CACHE/results-SPLIT.json")
    args = parser.parse_args()
    canonical = [HERE / name for name in ("public.jsonl", "assets.jsonl", "runner.jsonl")]
    canonical.extend(sorted(HERE.glob("*_context.jsonl")))
    # The Pygments panel is entirely lexer input fixtures; count it separately
    # to avoid inflating accuracy for ordinary programs and runnable tests.
    fixture_panel = HERE / "lexer_inputs.jsonl"
    paths = args.manifest if args.manifest is not None else canonical
    read_manifests(list(dict.fromkeys([*canonical, fixture_panel, *paths])), None)
    if args.coverage:
        repos = collections.defaultdict(lambda: collections.defaultdict(set))
        for row in read_manifests(paths, None):
            repos[row["language"]][row["split"]].add(row["repository"].lower())
        if not args.without_testroutes:
            for split, _, repo, _ in testroutes.PROJECTS:
                repos["java"][split].add(repo.lower())
        for lang, groups in sorted(repos.items()):
            print(f"{lang:15} dev {len(groups['dev']):3} repos | test {len(groups['test']):3} repos")
        return
    (args.cache / "blobs").mkdir(exist_ok=True, parents=True)
    rows = read_manifests(paths, args.split)
    omitted = []
    if not args.without_testroutes:
        routes, omitted = testroutes.cases(args.cache, args.split)
        rows.extend(routes)
    projects = load_panel(rows, args.cache)
    if not projects:
        parser.error("split has no cases")
    counts = collections.Counter()
    for files in projects.values():
        for r in files.values():
            if r["labels"]:
                counts[r["language"]] += len(r["labels"])
    print(f"{args.split}: {len(projects)} repositories, {len(counts)} languages, {sum(counts.values())} labelled lines")
    report = dict(split=args.split, repositories=sorted(projects), labels_by_language=dict(sorted(counts.items())),
                  manifest_sha256={p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in paths},
                  testroutes_sha256=None if args.without_testroutes else testroutes.DIGEST,
                  testroutes_excluded=omitted)
    if not args.prepare_only:
        binary = args.binary.resolve()
        samples = evaluate(projects, binary)
        report["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
        report["input_visibility"] = {
            "missing_files": sorted({s["repository"] + "/" + s["path"] for s in samples if not s["file_returned"]}),
            "missing_anchors": sum(not s["file_returned"] for s in samples),
        }
        def scores(selected):
            return {arm: metrics([(s["expected"], s[arm]) for s in selected]) for arm in ("cx", "baseline")}
        report["overall"] = scores(samples)
        review_file = HERE / "adjudications.json"
        decisions = {(a["repository"], a["revision"], a["path"], a["line"]): a
                     for a in json.loads(review_file.read_text())["adjudications"]}
        reviewed, applied = [], []
        for sample in samples:
            key = tuple(sample[k] for k in ("repository", "revision", "path", "line"))
            decision = decisions.get(key)
            if decision:
                if (sample["expected"] != decision["original_role"]
                        or decision["id"] not in sample["evidence"].removeprefix("TestRoutes ").split(", ")):
                    raise ValueError(f"review does not match original label: {key}")
                sample = dict(sample, expected=decision["reviewed_role"])
                applied.append(decision["id"])
            reviewed.append(sample)
        report["reviewed"] = dict(overall=scores(reviewed), applied=sorted(applied),
                                  adjudications_sha256=hashlib.sha256(review_file.read_bytes()).hexdigest())
        report["by_language"] = {lang: scores([s for s in samples if s["language"] == lang]) for lang in sorted(counts)}
        def macro(arm):
            accuracies = [r[arm]["accuracy_known"] for r in report["by_language"].values()
                          if r[arm]["accuracy_known"] is not None]
            return round(sum(accuracies) / len(accuracies), 5) if accuracies else None
        report["language_macro_accuracy_known"] = {arm: macro(arm) for arm in ("cx", "baseline")}
        report["ambiguous_line_anchors"] = sum(s["mixed_line"] for s in samples)
        report["excluding_ambiguous_lines"] = scores([s for s in samples if not s["mixed_line"]])
        report["samples"] = samples
        for lang, row in report["by_language"].items():
            print(f"{lang:15} cx {row['cx']['correct_known']:3}/{row['cx']['known_labels']:<3}  "
                  f"path {row['baseline']['correct_known']:3}/{row['baseline']['known_labels']:<3}  "
                  f"abstentions {row['cx']['abstained_on_known']}")
    filename = ("prepared" if args.prepare_only else "results") + "-" + args.split + ".json"
    output = args.output or args.cache / filename
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    print(output)


if __name__ == "__main__":
    main()
