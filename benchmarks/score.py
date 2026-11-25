"""Evaluate cx scores from source on the public, pinned 5,000-file FORGE corpus."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import urllib.request

URL = ("https://raw.githubusercontent.com/codescene-research/ai-friendliness-forge-2026/"
       "c7e7afd437cfaecb282b9e7c366ed1dbae06ca2d/dataset/python_corp_bleud.jsonl")
SHA256 = "06b694877c0279fbd60ddcd138392bf63fa28202d0a307cab37f5ee5d86a18ec"
RULES = {"Complex Method": "branch complexity", "Large Method": "unit lines",
         "Complex Conditional": "compound conditions", "Bumpy Road Ahead": "nested regions",
         "Deep, Nested Complexity": "nesting depth", "Excess Number of Function Arguments": "parameters",
         "High Degree of Code Duplication": "similar body (code lines)"}


def corpus(path):
    if path:
        raw = path.read_bytes()
    else:
        with urllib.request.urlopen(URL, timeout=90) as response:
            raw = response.read()
    if hashlib.sha256(raw).hexdigest() != SHA256:
        raise ValueError("Unexpected corpus SHA-256")
    rows = [json.loads(line) for line in raw.splitlines()]
    if len(rows) != 5000:
        raise ValueError("Incomplete corpus")
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/cx"))
    parser.add_argument("--dataset", type=Path, help="use a local copy of the pinned JSONL")
    parser.add_argument("--output", type=Path, help="save summary plus individual errors")
    args = parser.parse_args()
    binary = args.binary.resolve()
    rows = corpus(args.dataset)
    # The checkout contains only the input sources. Keep labels outside cx's index.
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp) / "repo"
        subprocess.run(["git", "init", "-q", root], check=True)
        for i, row in enumerate(rows):
            (root / f"p{i:04}.py").write_bytes(row["task"].encode())
        home = Path(tmp) / "home"
        home.mkdir()
        result = json.loads(subprocess.check_output([binary, "--score", "--json"], cwd=root,
                            env=dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"))))
    if result["scored_files"] != len(rows) or len(result["files"]) != len(rows):
        raise ValueError("cx did not score the entire corpus")
    measured = {r["path"]: r for r in result["files"]}
    details = []
    hits, extras, misses = Counter(), Counter(), Counter()
    for i, row in enumerate(rows):
        observed = measured[f"p{i:04}.py"]
        expected = Counter((RULES.get(f["code-smell"], f["code-smell"]), f["location"]["start-line"])
                           for f in row["smells"])
        actual = Counter((f["rule"], f["line"]) for f in observed["findings"])
        for counts, target in [(expected & actual, hits), (expected - actual, misses), (actual - expected, extras)]:
            for (rule, _line), n in counts.items():
                target[rule] += n
        details.append(dict(row=i, name=row["name"], expected=row["score"], actual=observed["score"],
                            error=abs(row["score"] - observed["score"])))
    errors = sorted(r["error"] for r in details)
    report = dict(dataset_sha256=SHA256, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  files=len(rows), exact_within_1e_9=sum(e < 1e-9 for e in errors),
                  within_half_point=sum(e <= 0.5 for e in errors), mean_absolute_error=statistics.mean(errors),
                  median_absolute_error=statistics.median(errors), worst_error=max(errors),
                  findings_by_rule_and_start_line=dict(matched=hits, missed=misses, extra=extras))
    print(json.dumps(report, indent=2))
    if args.output:
        args.output.write_text(json.dumps(dict(report, cases=details), indent=2) + "\n")


if __name__ == "__main__":
    main()
