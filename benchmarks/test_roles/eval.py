"""Evaluate cx test roles at frozen, independently labelled source locations.

Run: python3 benchmarks/test_roles/eval.py --binary target/release/cx
Only an explicit --split test evaluates held-out projects. Sources are cached
under target/test-role-eval. The cache and gold labels stay outside cx's input.
"""
import argparse
import bisect
import collections
import concurrent.futures
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath
from typing import NamedTuple

import testroutes

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE.parent))
from score_response import result_paths
from role_source import (
    InputFile, ROLES, byte_span, checked_path, labelled_file, pin_key, pinned_files,
    pinned_source, reason_text,
)


def baseline(path):
    # Same file naming conventions as benchmarks/score_eval.py::path_role.
    p = PurePosixPath(path.lower())
    if (set(p.parts[:-1]) & {"test", "tests", "__tests__", "spec", "specs"}
            or p.stem.startswith("test_")
            or p.stem.endswith(("_test", ".test", ".spec", "tests", "testcase"))):
        return "test"
    return "production"


class Manifest(NamedTuple):
    rows: list
    digest: str


def manifest_hashes(manifests, paths):
    selected = list(dict.fromkeys(paths))
    names = collections.Counter(path.name for path in selected)
    return {(path.name if names[path.name] == 1 else str(path)): manifests[path].digest
            for path in selected}


def record_manifest_pin(row, splits, by_digest):
    repo, split = row["repository"].lower(), row["split"]
    if split not in {"dev", "test"} or splits.setdefault(repo, split) != split:
        raise ValueError(f"invalid or overlapping repository split: {repo}")
    if by_digest.setdefault(row["sha256"], split) != split:
        raise ValueError(f"identical source crosses repository splits: {repo}/{row['path']}")


def load_manifests(paths):
    """Validate every source pin across splits before selecting a report population."""
    manifests = {}
    splits = {repo.lower(): split for split, _, repo, _ in testroutes.PROJECTS}
    by_digest = {}
    for path in dict.fromkeys(paths):
        source = InputFile.read(path)
        rows = source.json_lines()
        for row in rows:
            record_manifest_pin(row, splits, by_digest)
        manifests[path] = Manifest(rows, source.digest())
    return manifests


def rate(n, d):
    return round(n / d, 5) if d else None


def precision_recall(expected, predicted):
    return {
        role: {"precision": rate(row[role], predicted[role]),
               "recall": rate(row[role], sum(row.values()))}
        for role, row in expected.items()
    }


def sparse_confusion(matrix):
    return {gold: {prediction: row[prediction] for prediction in ROLES if row[prediction]}
            for gold, row in matrix.items() if row}


def metrics(matrix):
    known = {role: matrix[role] for role in ROLES if role != "unknown"}
    predicted = sum(known.values(), collections.Counter())
    known_count = sum(predicted.values())
    correct = sum(row[role] for role, row in known.items())
    test_roles = ("test", "test-support")

    return {
        "test_or_support_recall": rate(sum(known[gold][pred] for gold in test_roles for pred in test_roles),
                                       sum(sum(known[gold].values()) for gold in test_roles)),
        "production_mislabelled_as_test": sum(known["production"][p] for p in test_roles),
        "labels": known_count + sum(matrix["unknown"].values()),
        "known_labels": known_count,
        "correct_known": correct,
        "abstained_on_known": predicted["unknown"],
        "accuracy_known": rate(correct, known_count),
        "roles": precision_recall(known, predicted),
        "confusion_gold_to_prediction": sparse_confusion(matrix),
    }


def load_panel(rows, cache):
    # Fetch each immutable source once; every label row still contributes its evidence.
    projects = {}
    groups = pinned_files(rows)
    revisions = {}
    for repo, revision, _ in groups:
        if revisions.setdefault(repo, revision) != revision:
            raise ValueError(f"repository has two revisions: {repo}")
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as workers:
        files = workers.map(lambda entry: labelled_file(entry, cache), groups.items())
        for (repo, _, path), file in zip(groups, files):
            projects.setdefault(repo, {})[path] = file
    return projects


def role_evidence(row, key, location):
    role = row[key]
    if role not in ROLES or not reason_text(row.get("reason")):
        if key == "default_role":
            raise ValueError(f"invalid default role in {location}")
        raise ValueError(f"invalid region in {location}: {row}")
    return role


def add_region(spans, covered, region):
    for line in covered:
        spans[line].append(region)


def accumulate_regions(regions, source, location, spans):
    lines = list(spans)
    for region in regions:
        role_evidence(region, "role", location)
        if not source.valid_region_bounds(region):
            raise ValueError(f"invalid region in {location}: {region}")
        first = bisect.bisect_left(lines, region["start_line"])
        after = bisect.bisect_right(lines, region["end_line"], lo=first)
        add_region(spans, lines[first:after], region)


def line_predictions(file, output, location):
    # Validate and project each reported region once, even on context-only files.
    source = file["source"]
    lines = sorted(file["labels"])
    spans = {line: [] for line in lines}
    default = "unknown"
    if output is not None:
        default = role_evidence(output, "default_role", location)
        accumulate_regions(output["regions"], source, location, spans)
    for line, covering in spans.items():
        role = covering[-1]["role"] if covering else default
        mixed = bool(covering and source.mixed_roles(line, default, covering))
        yield line, (role, mixed, output is not None)


def labelled_predictions(repo, files, actual):
    for path, file in sorted(files.items()):
        for line, prediction in line_predictions(file, actual.get(path), f"{repo}/{path}"):
            yield (repo, file["revision"], path, line), prediction


def tracked_checkout(root, home, files):
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
        dest.write_bytes(r["source"].data)
    # Every labelled input is tracked in its public source revision.
    # Preserve that property, including intentionally hidden fixtures.
    for git_args in (["init", "--quiet", "--template="], ["add", "-f", "--", "."],
                     ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                      "commit", "--quiet", "-m", "Pinned evaluation inputs"]):
        subprocess.run(["git", *git_args], cwd=root, env=env, check=True, capture_output=True)
    return env


def evaluate(projects, binary):
    samples = []
    for repo, files in sorted(projects.items()):
        with tempfile.TemporaryDirectory(prefix="cx-test-roles-") as directory:
            root = Path(directory, "repo")
            home = Path(directory, "home")
            root.mkdir()
            home.mkdir()
            env = tracked_checkout(root, home, files)
            proc = subprocess.run([binary, "--score", "--roles", "--json"], cwd=root, env=env,
                                  capture_output=True, check=False)
            if proc.returncode:
                raise RuntimeError(f"cx failed for {repo} (exit {proc.returncode}): {proc.stderr.decode(errors='replace')}")
            actual = result_paths(json.loads(proc.stdout), files, allow_missing=True)
            samples.extend(labelled_predictions(repo, files, actual))
    return samples


def scores(votes):
    cx_rows = {role: collections.Counter() for role in ROLES}
    baseline_rows = {role: collections.Counter() for role in ROLES}
    for (gold, cx, baseline), count in votes.items():
        cx_rows[gold][cx] += count
        baseline_rows[gold][baseline] += count
    return {"cx": metrics(cx_rows), "baseline": metrics(baseline_rows)}


def macro_accuracy(by_language, arm):
    accuracies = [r[arm]["accuracy_known"] for r in by_language.values()
                  if r[arm]["accuracy_known"] is not None]
    return round(sum(accuracies) / len(accuracies), 5) if accuracies else None


def anchor(row):
    if type(row["line"]) is not int or row["line"] < 1:
        raise ValueError(f"invalid role anchor line: {row['line']!r}")
    return row["repository"], row["revision"], row["path"], row["line"]


def index_anchors(rows):
    indexed = {anchor(row): row for row in rows}
    if len(indexed) != len(rows):
        raise ValueError("duplicate role anchor")
    return indexed


class FrozenPanel:
    """Gold, evidence and the path baseline belong to the source manifest."""
    def __init__(self, projects):
        self.sites = {}
        self.languages = collections.Counter()
        for repo, files in projects.items():
            for path, file in files.items():
                predicted = baseline(path)
                self.languages[file["language"]] += len(file["labels"])
                for line, (gold, evidence) in file["labels"].items():
                    self.sites[repo, file["revision"], path, line] = (file["language"], gold, predicted, evidence)
        self.languages = +self.languages  # context-only files contribute no language denominator

    def join(self, observations):
        pending = self.sites.copy()
        for site, (cx, mixed, returned) in observations:
            if type(site[3]) is not int or site not in pending:
                raise ValueError(f"duplicate or unexpected role observation: {site}")
            language, gold, path_baseline, evidence = pending.pop(site)
            repo, revision, path, line = site
            yield dict(repository=repo, revision=revision, path=path, line=line, language=language,
                       expected=gold, cx=cx, baseline=path_baseline, mixed_line=mixed,
                       file_returned=returned, evidence=evidence)
        if pending:
            raise ValueError(f"missing role observation: {next(iter(pending))}")


def reviewed_vote(decisions, sample, vote, applied):
    decision = decisions.get(anchor(sample))
    if decision is None:
        return vote
    gold, cx, path_baseline = vote
    if (gold != decision["original_role"]
            or decision["id"] not in sample["evidence"].removeprefix("TestRoutes ").split(", ")):
        raise ValueError(f"review does not match original label: {anchor(sample)}")
    applied.append(decision["id"])
    return decision["reviewed_role"], cx, path_baseline


def print_coverage(rows, without_testroutes):
    repos = collections.defaultdict(lambda: collections.defaultdict(set))
    for row in rows:
        repos[row["language"]][row["split"]].add(row["repository"].lower())
    if not without_testroutes:
        for split, _, repo, _ in testroutes.PROJECTS:
            repos["java"][split].add(repo.lower())
    for lang, groups in sorted(repos.items()):
        print(f"{lang:15} dev {len(groups['dev']):3} repos | test {len(groups['test']):3} repos")


def prediction_report(observations, binary, panel):
    review_file = HERE / "adjudications.json"
    review_data = review_file.read_bytes()
    decisions = index_anchors(json.loads(review_data)["adjudications"])
    groups = collections.defaultdict(collections.Counter)
    mixed, reviewed = collections.Counter(), collections.Counter()
    missing_files, missing_anchors = set(), 0
    samples, applied = [], []
    for sample in panel.join(observations):
        samples.append(sample)
        vote = sample["expected"], sample["cx"], sample["baseline"]
        groups[sample["language"]][vote] += 1
        reviewed[reviewed_vote(decisions, sample, vote, applied)] += 1
        if sample["mixed_line"]:
            mixed[vote] += 1
        if not sample["file_returned"]:
            missing_files.add(sample["repository"] + "/" + sample["path"])
            missing_anchors += 1
    overall = sum(groups.values(), collections.Counter())
    report = dict(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  input_visibility=dict(missing_files=sorted(missing_files), missing_anchors=missing_anchors),
                  overall=scores(overall),
                  reviewed=dict(overall=scores(reviewed), applied=sorted(applied),
                                adjudications_sha256=hashlib.sha256(review_data).hexdigest()))
    report["by_language"] = {lang: scores(groups[lang]) for lang in sorted(groups)}
    report["language_macro_accuracy_known"] = {arm: macro_accuracy(report["by_language"], arm) for arm in ("cx", "baseline")}
    report["ambiguous_line_anchors"] = sum(mixed.values())
    report["excluding_ambiguous_lines"] = scores(overall - mixed)
    report["samples"] = samples
    return report


def selected_cases(args, selected):
    rows = [row for row in selected if row["split"] == args.split]
    omitted = []
    if not args.without_testroutes:
        routes, omitted = testroutes.cases(args.cache, args.split)
        rows.extend(routes)
    return rows, omitted


def score_selected_cases(report, projects, binary, panel):
    binary = binary.resolve()
    samples = evaluate(projects, binary)
    report.update(prediction_report(samples, binary, panel))
    for lang, row in report["by_language"].items():
        print(f"{lang:15} cx {row['cx']['correct_known']:3}/{row['cx']['known_labels']:<3}  "
              f"path {row['baseline']['correct_known']:3}/{row['baseline']['known_labels']:<3}  "
              f"abstentions {row['cx']['abstained_on_known']}")


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
    manifests = load_manifests([*canonical, fixture_panel, *paths])
    selected = (row for path in paths for row in manifests[path].rows)
    if args.coverage:
        print_coverage(selected, args.without_testroutes)
        return
    (args.cache / "blobs").mkdir(exist_ok=True, parents=True)
    rows, omitted = selected_cases(args, selected)
    projects = load_panel(rows, args.cache)
    if not projects:
        parser.error("split has no cases")
    panel = FrozenPanel(projects)
    counts = panel.languages
    print(f"{args.split}: {len(projects)} repositories, {len(counts)} languages, {sum(counts.values())} labelled lines")
    report = dict(split=args.split, repositories=sorted(projects), labels_by_language=dict(sorted(counts.items())),
                  manifest_sha256=manifest_hashes(manifests, paths),
                  testroutes_sha256=None if args.without_testroutes else testroutes.DIGEST,
                  testroutes_excluded=omitted)
    if not args.prepare_only:
        score_selected_cases(report, projects, args.binary, panel)
    filename = ("prepared" if args.prepare_only else "results") + "-" + args.split + ".json"
    output = args.output or args.cache / filename
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    print(output)


if __name__ == "__main__":
    main()
