"""Run the test-maintainability development baseline. No blind execution path."""
import argparse
import collections
import gzip
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
sys.path.insert(0, str(HERE.parent / "test_roles"))
from eval import checked_path, get_source  # pinned public source retrieval
from sample_dev import EXT

RULES = {"conditional_logic": {"conditional test expectation", "branch complexity", "compound conditions", "nesting depth", "nested regions"},
         "verbosity": {"unit lines"}}


def rows(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line]


def key(r):
    return r["repository"].lower(), r["revision"], r["path"]


def fingerprint(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def measure(binary, root, compatibility):
    args = [str(binary), "--score", "--json"]
    if compatibility:
        args.append("--ignore-test-rules")
    return subprocess.check_output(args, cwd=root)


def review_counts(reviews, files):
    a, b = ({key(r): r for r in group} for group in reviews)
    if a.keys() != b.keys() or len(a) != len(reviews[0]) or len(b) != len(reviews[1]):
        raise ValueError("review panels differ or contain duplicate files")
    result = {}
    for rule, signals in RULES.items():
        counts = collections.Counter()
        for k in a:
            pair = [a[k][rule]["label"], b[k][rule]["label"]]
            if any(v not in {"yes", "no", "na", "uncertain"} for v in pair):
                raise ValueError(f"unexpected review label: {pair}")
            if pair[0] != pair[1]:
                counts["disagreement"] += 1
            elif pair[0] in {"na", "uncertain"}:
                counts[pair[0]] += 1
            else:
                row = files[k]
                if row["score"] is None:
                    counts["unscored"] += 1
                    continue
                hit = any(f["rule"] in signals for f in row["findings"])
                counts[("positive_" if pair[0] == "yes" else "negative_")
                       + ("flagged" if hit else "silent")] += 1
        result[rule] = dict(sorted(counts.items()))
    return result


def evaluate_pairs(binary, compatibility):
    folder = HERE / "pairs"
    out = []
    for pair in json.loads((folder / "manifest.json").read_text())["pairs"]:
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
                row = next(r for r in answer["files"] if r["path"] == dest.relative_to(root).as_posix())
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
    reviews = [rows(HERE / f"review_{letter}.jsonl") for letter in "ab"]
    blind = {r["repository"].lower() for r in json.loads((HERE / "blind.lock.json").read_text())}
    sources = {}
    # Development identity is allowlisted before any source read.
    allowed = set()
    for name in ("public", "assets", "runner"):
        for r in rows(HERE.parent / "test_roles" / f"{name}.jsonl"):
            if r["split"] == "dev":
                allowed.add(key(r)[:2])
                if any(label[1] in {"test", "test-support"} for label in r["labels"]):
                    sources[key(r)] = r
    challenge = rows(HERE / "conditional.jsonl")
    for group in [rows(HERE / "dev_sample.jsonl"), challenge, *reviews]:
        for r in group:
            k = key(r)
            if k in sources and (sources[k]["sha256"], sources[k]["language"]) != (r["sha256"], r["language"]):
                raise ValueError(f"source conflicts: {k}")
            sources[k] = r
    for group in reviews:
        coverage = collections.Counter(r["language"] for r in group)
        if set(coverage) != set(EXT) or set(coverage.values()) != {3}:
            raise ValueError("review panel requires three files per profile")
    groups = collections.defaultdict(list)
    for k, r in sorted(sources.items()):
        if k[:2] not in allowed or k[0] in blind or r.get("split", "dev") != "dev":
            raise ValueError(f"repository outside development split: {k[0]}")
        groups[k[:2]].append(r)
    files = {}
    raw = []
    cache = ROOT / "target/test-role-eval"
    (cache / "blobs").mkdir(parents=True, exist_ok=True)
    for group in groups.values():
        with tempfile.TemporaryDirectory(prefix="cx-quality-") as td:
            root = Path(td)
            for r in group:
                p = root / checked_path(r["path"])
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_bytes(get_source(cache, r))
            observed = measure(binary, root, not args.candidate)
            if args.reference and observed != measure(args.reference.resolve(), root, False):
                raise ValueError(f"compatibility drift in {group[0]['repository']}")
            data = json.loads(observed)
            indexed = {r["path"]: r for r in data["files"]}
            for r in group:
                # An omitted file is an explicit coverage gap.
                files[key(r)] = indexed.get(r["path"], dict(path=r["path"], score=None, findings=[], lines=0, absent=True))
            raw.append(dict(repository=group[0]["repository"], revision=group[0]["revision"], result=data))
    profiles = {}
    for lang in sorted({r["language"] for r in sources.values()}):
        subset = [files[k] for k, r in sources.items() if r["language"] == lang]
        scores = [r["score"] for r in subset if r["score"] is not None]
        profiles[lang] = dict(files=len(subset), scored=len(scores),
                              omitted=sum(r.get("absent", False) for r in subset),
                              mean=statistics.mean(scores) if scores else None,
                              findings=sum(len(r["findings"]) for r in subset))
    if set(profiles) != set(EXT):
        raise ValueError("development set lost language coverage")
    args.out.mkdir(parents=True, exist_ok=True)
    mode = "candidate" if args.candidate else "baseline"
    snapshot = args.out / f"{mode}.json.gz"
    payload = json.dumps(raw, ensure_ascii=False, separators=(",", ":")).encode()
    snapshot.write_bytes(gzip.compress(payload, mtime=0))
    report = dict(split="dev", mode=mode, repositories=len(groups), files=len(sources),
                  binary_sha256=fingerprint(binary), output_sha256=hashlib.sha256(payload).hexdigest(),
                  reference_sha256=fingerprint(args.reference) if args.reference else None,
                  inputs={str(p.relative_to(HERE.parent)): fingerprint(p) for p in sorted([
                      *HERE.glob("*.jsonl"), HERE / "blind.lock.json", HERE / "pairs/manifest.json",
                      *(HERE.parent / "test_roles" / f"{name}.jsonl" for name in ("public", "assets", "runner"))])},
                  profiles=profiles, file_level_review=review_counts(reviews, files))
    challenge_counts = collections.Counter()
    for r in challenge:
        row = files[key(r)]
        label = r["conditional_logic"]["label"]
        hits = [f for f in row["findings"] if f["rule"] in RULES["conditional_logic"]
                and r["start_line"] <= f["line"] <= r["end_line"]]
        challenge_counts[label + ("_unscored" if row["score"] is None else "_flagged" if hits else "_silent")] += 1
    report["reviewed_files"] = len(reviews[0])
    report["review_by_profile"] = {lang: review_counts([[r for r in group if r["language"] == lang]
                                                         for group in reviews], files) for lang in profiles}
    report["conditional_challenge"] = challenge_counts
    report["localized_verbosity"] = []
    for label in rows(HERE / "verbosity_units.jsonl"):
        k = key(label)
        if sources[k]["sha256"] != label["sha256"]:
            raise ValueError(f"stale verbosity label: {k}")
        data = get_source(cache, sources[k])
        lo, hi = label["start_byte"], label["end_byte"]
        if (not 0 <= lo < hi <= len(data)
                or label["unit"].encode() not in data[lo:hi].splitlines()[0]
                or data[:lo].count(b"\n") + 1 != label["start_line"]
                or data[:hi].count(b"\n") + 1 != label["end_line"]):
            raise ValueError(f"invalid verbosity unit: {k}")
        hits = [f for f in files[k]["findings"] if f["rule"] == "unit lines"
                and f["unit"] == label["unit"] and label["start_line"] <= f["line"] <= label["end_line"]]
        report["localized_verbosity"].append(dict(unit=label["unit"], path=label["path"],
                                                 expected=label["verbosity"], reviewers=len(label["evidence"]), length_findings=hits))
    report["paired_controls"] = evaluate_pairs(binary, not args.candidate)
    (args.out / f"{mode}.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k:report[k] for k in ("files","repositories","file_level_review","conditional_challenge")}, indent=2))


if __name__ == "__main__":
    main()
