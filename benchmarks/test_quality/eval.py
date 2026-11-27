"""Run the test-maintainability development baseline. No blind execution path."""
import argparse
import collections
from dataclasses import dataclass
import gzip
from itertools import chain
import hashlib
import json
import shutil
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE.parent))
from score_response import scored_paths, observed_findings, physical_lines

sys.path.insert(0, str(HERE.parent / "test_roles"))
from eval import InputFile, byte_span, checked_path, pinned_source, pin_key as key, pinned_files  # pinned public source retrieval
from sample_dev import EXT

RULES = {"conditional_logic": {"conditional test expectation", "branch complexity", "compound conditions", "nesting depth", "nested regions"},
         "verbosity": {"unit lines"}}


def capture_inputs():
    role_dir = HERE.parent / "test_roles"
    paths = [*HERE.glob("*.jsonl"), HERE / "blind.lock.json", HERE / "pairs/manifest.json",
             *(role_dir / f"{name}.jsonl" for name in ("public", "assets", "runner"))]
    return {path: InputFile.read(path) for path in sorted(paths)}


def fingerprint(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def measure(binary, root, compatibility):
    args = [str(binary), "--score", "--json"]
    if compatibility:
        args.append("--ignore-test-rules")
    return subprocess.check_output(args, cwd=root)


def review_outcome(pair, signals, row):
    if any(v not in {"yes", "no", "na", "uncertain"} for v in pair):
        raise ValueError(f"unexpected review label: {pair}")
    label, other = pair
    if label != other:
        return "disagreement"
    if label in {"na", "uncertain"}:
        return label
    if row["score"] is None:
        return "unscored"
    hit = any(f["rule"] in signals for f in row["findings"])
    return ("positive_" if label == "yes" else "negative_") + ("flagged" if hit else "silent")


@dataclass(frozen=True)
class ReviewedFile:
    identity: tuple[str, str, str]
    language: str
    labels: dict[str, str]

    @classmethod
    def decode(cls, row):
        return cls(key(row), row["language"], {rule: row[rule]["label"] for rule in RULES})


def review_index(group):
    index = {key(row): row for row in group}
    if len(index) != len(group):
        raise ValueError("review panels differ or contain duplicate files")
    return index


def matched_reviews(reviews):
    a, b = map(review_index, reviews)
    if a.keys() != b.keys():
        raise ValueError("review panels differ or contain duplicate files")
    for k, left in a.items():
        right = b[k]
        if left["language"] != right["language"]:
            raise ValueError(f"review language differs for {k}")
        yield ReviewedFile.decode(left), ReviewedFile.decode(right)


def review_counts(reviews, files):
    def counters():
        return {rule: collections.Counter() for rule in RULES}

    total = counters()
    profiles = collections.defaultdict(counters)
    for left, right in matched_reviews(reviews):
        profile = profiles[left.language]
        for rule, signals in RULES.items():
            pair = left.labels[rule], right.labels[rule]
            outcome = review_outcome(pair, signals, files[left.identity])
            total[rule][outcome] += 1
            profile[rule][outcome] += 1

    def ordered(counts):
        return {rule: dict(sorted(values.items())) for rule, values in counts.items()}
    return ordered(total), {lang: ordered(profiles[lang]) for lang in sorted(profiles)}


def evaluate_pairs(binary, compatibility, pairs):
    folder = HERE / "pairs"
    out = []
    for pair in pairs:
        scores = []
        with tempfile.TemporaryDirectory(prefix="cx-quality-pair-") as td:
            root = Path(td)
            subject = folder / checked_path(pair["subject"])
            dest = root / "tests" / Path(pair["left"]).name
            dest.parent.mkdir()
            shutil.copyfile(subject, dest.parent / subject.name)
            for side in ("left", "right"):
                shutil.copyfile(folder / checked_path(pair[side]), dest)
                answer = json.loads(measure(binary, root, compatibility))
                row = scored_paths(answer, [dest.relative_to(root).as_posix(), "tests/" + subject.name])[dest.relative_to(root).as_posix()]
                scores.append(row["score"])
        if None in scores:
            observed = "unscored"
        elif format(scores[0], ".2f") == format(scores[1], ".2f"):
            observed = "tie"
        else:
            observed = "prefer_right" if scores[1] > scores[0] else "prefer_left"
        out.append(dict(id=pair["id"], family=pair["family"], kind=pair["kind"],
                        expected=pair["expected"], observed=observed, scores=scores))
    return out


def validate_reviews(reviews):
    for group in reviews:
        coverage = collections.Counter(r["language"] for r in group)
        if set(coverage) != set(EXT) or set(coverage.values()) != {3}:
            raise ValueError("review panel requires three files per profile")


def development_roles(inputs):
    for name in ("public", "assets", "runner"):
        for r in inputs[HERE.parent / "test_roles" / f"{name}.jsonl"].json_lines(skip_empty=True):
            if r["split"] == "dev":
                yield r


def development_inputs(inputs):
    reviews = [inputs[HERE / f"review_{letter}.jsonl"].json_lines(skip_empty=True) for letter in "ab"]
    blind = {r["repository"].lower() for r in json.loads(inputs[HERE / "blind.lock.json"].data)}
    role_rows = list(development_roles(inputs))
    allowed = {key(row)[:2] for row in role_rows}
    labelled = (row for row in role_rows if any(label[1] in {"test", "test-support"} for label in row["labels"]))
    challenge = inputs[HERE / "conditional.jsonl"].json_lines(skip_empty=True)
    validate_reviews(reviews)
    raw = chain(labelled, inputs[HERE / "dev_sample.jsonl"].json_lines(skip_empty=True), challenge, *reviews)
    sources = checked_development_sources(raw, allowed, blind)
    groups = collections.defaultdict(dict)
    for k, row in sources.items():
        groups[k[:2]][k[2]] = row
    return sources, groups, reviews, challenge


def checked_development_sources(rows, allowed, blind):
    sources = {}
    for k, duplicates in sorted(pinned_files(rows).items()):
        if k[:2] not in allowed or k[0] in blind:
            raise ValueError(f"repository outside development split: {k[0]}")
        if any(r.get("split", "dev") != "dev" for r in duplicates):
            raise ValueError(f"repository outside development split: {k[0]}")
        sources[k] = duplicates[-1]
    return sources


def materialize(group, root, cache, localized_keys):
    retained = {}
    for r in group:
        relative, data = pinned_source(cache, r)
        p = root / relative
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        identity = key(r)
        if identity in localized_keys:
            retained[identity] = data
    return retained


@dataclass(frozen=True)
class ScoreReplay:
    binary: Path
    compatibility: bool
    reference: Path | None

    def __call__(self, root, repository):
        observed = measure(self.binary, root, self.compatibility)
        if self.reference and observed != measure(self.reference, root, False):
            raise ValueError(f"compatibility drift in {repository}")
        return json.loads(observed)


def project_rows(project, group, data):
    indexed = scored_paths(data, group, allow_missing=True)
    for path in group.keys() - indexed.keys():
        indexed[path] = dict(path=path, score=None, findings=[], lines=0, absent=True)
    return {(*project, path): row for path, row in indexed.items()}


def score_projects(groups, labels, read_scores):
    labels = [(key(r), r) for r in labels]
    localized_keys = {k for k, _ in labels}
    localized_sources = {}
    files = {}
    raw = []
    cache = ROOT / "target/test-role-eval"
    (cache / "blobs").mkdir(parents=True, exist_ok=True)
    for project, group in groups.items():
        with tempfile.TemporaryDirectory(prefix="cx-quality-") as td:
            root = Path(td)
            localized_sources.update(materialize(group.values(), root, cache, localized_keys))
            first = next(iter(group.values()))
            data = read_scores(root, first["repository"])
            files.update(project_rows(project, group, data))
            raw.append(dict(repository=first["repository"], revision=first["revision"], result=data))
    return files, raw, [(k, r, localized_sources[k]) for k, r in labels]


def language_summary(sources, files):
    profiles = {}
    by_language = collections.defaultdict(list)
    for k, source in sources.items():
        by_language[source["language"]].append(files[k])
    for lang, subset in sorted(by_language.items()):
        scores = [r["score"] for r in subset if r["score"] is not None]
        profiles[lang] = dict(files=len(subset), scored=len(scores),
                              omitted=sum(r.get("absent", False) for r in subset),
                              mean=statistics.mean(scores) if scores else None,
                              findings=sum(len(r["findings"]) for r in subset))
    if set(profiles) != set(EXT):
        raise ValueError("development set lost language coverage")
    return profiles


@dataclass(frozen=True)
class LocalizedUnit:
    name: str
    start_line: int
    end_line: int

    @classmethod
    def checked(cls, identity, label, data, source):
        if source["sha256"] != label["sha256"]:
            raise ValueError(f"stale verbosity label: {identity}")
        lo, hi = label["start_byte"], label["end_byte"]
        if not byte_span(data, lo, hi):
            raise ValueError(f"invalid verbosity unit: {identity}")
        unit = cls(label["unit"], label["start_line"], label["end_line"])
        if not unit.matches_source(data, lo, hi):
            raise ValueError(f"invalid verbosity unit: {identity}")
        return unit

    def matches_source(self, data, lo, hi):
        if type(self.start_line) is not int or type(self.end_line) is not int:
            return False
        first = data.count(b"\n", 0, lo) + 1
        last = first + data.count(b"\n", lo, hi)
        return (first, last) == (self.start_line, self.end_line) and self.name.encode() in data[lo:hi].splitlines()[0]

    def matches_finding(self, finding):
        return finding["unit"] == self.name and self.start_line <= finding["line"] <= self.end_line


def localized_verbosity(sources, files, samples):
    results = []
    pinned = {k: data for k, _, data in samples}
    by_file = {k: list(observed_findings(files[k]["findings"], physical_lines(data), {"unit lines"}))
               for k, data in pinned.items()}
    for k, label, data in samples:
        unit = LocalizedUnit.checked(k, label, data, sources[k])
        hits = [finding for finding in by_file[k] if unit.matches_finding(finding)]
        results.append(dict(unit=unit.name, path=label["path"], expected=label["verbosity"],
                            reviewers=len(label["evidence"]), length_findings=hits))
    return results


def conditional_counts(challenge, files):
    challenge_counts = collections.Counter()
    for r in challenge:
        row = files[key(r)]
        label = r["conditional_logic"]["label"]
        hits = [f for f in row["findings"] if f["rule"] in RULES["conditional_logic"]
                and r["start_line"] <= f["line"] <= r["end_line"]]
        challenge_counts[label + ("_unscored" if row["score"] is None else "_flagged" if hits else "_silent")] += 1
    return challenge_counts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--reference", type=Path, help="pre-flag executable for bytewise baseline verification")
    parser.add_argument("--candidate", action="store_true", help="measure default scoring instead of the frozen policy")
    parser.add_argument("--out", type=Path, default=ROOT / "target/test-quality")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if args.candidate and args.reference:
        parser.error("reference checks apply only in compatibility mode")
    inputs = capture_inputs()
    sources, groups, reviews, challenge = development_inputs(inputs)
    files, raw, localized = score_projects(
        groups, inputs[HERE / "verbosity_units.jsonl"].json_lines(skip_empty=True),
        ScoreReplay(binary, not args.candidate, args.reference.resolve() if args.reference else None),
    )
    profiles = language_summary(sources, files)
    review, review_by_profile = review_counts(reviews, files)
    args.out.mkdir(parents=True, exist_ok=True)
    mode = "candidate" if args.candidate else "baseline"
    snapshot = args.out / f"{mode}.json.gz"
    payload = json.dumps(raw, ensure_ascii=False, separators=(",", ":")).encode()
    snapshot.write_bytes(gzip.compress(payload, mtime=0))
    report = dict(split="dev", mode=mode, repositories=len(groups), files=len(sources),
                  binary_sha256=fingerprint(binary), output_sha256=hashlib.sha256(payload).hexdigest(),
                  reference_sha256=fingerprint(args.reference) if args.reference else None,
                  inputs={str(p.relative_to(HERE.parent)): item.digest() for p, item in inputs.items()},
                  profiles=profiles, file_level_review=review)
    report["reviewed_files"] = len(reviews[0])
    report["review_by_profile"] = review_by_profile
    report["conditional_challenge"] = conditional_counts(challenge, files)
    report["localized_verbosity"] = localized_verbosity(sources, files, localized)
    report["paired_controls"] = evaluate_pairs(binary, not args.candidate,
                                                json.loads(inputs[HERE / "pairs/manifest.json"].data)["pairs"])
    (args.out / f"{mode}.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k:report[k] for k in ("files","repositories","file_level_review","conditional_challenge")}, indent=2))


if __name__ == "__main__":
    main()
