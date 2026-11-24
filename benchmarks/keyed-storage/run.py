"""Alternate backend order, verify every sample, and retain raw metrics outside each temporary store."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import statistics

def main():
    cli = argparse.ArgumentParser()
    cli.add_argument("repo", type=Path)
    cli.add_argument("output", type=Path)
    cli.add_argument("--rounds", type=int, default=6)
    cli.add_argument("--arms", nargs="+", choices=["cow", "runs", "sqlite", "sqlite-blocks", "sqlite-batch"],
                     default=["cow", "sqlite", "runs"])
    args = cli.parse_args()
    if args.rounds < 1:
        cli.error("rounds must be positive")
    repo = args.repo.resolve()
    git = lambda *a: subprocess.check_output(["git", "-C", repo, *a], text=True).strip()
    if git("status", "--porcelain"):
        cli.error("use a clean checkout")
    binary = Path(__file__).resolve().parent / "target/release/cx-storage-replay"
    report = {"revision": git("rev-parse", "HEAD"), "runs": []}
    arms = args.arms
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for i in range(args.rounds):
        offset = i % len(arms) if len(arms) > 2 else 0
        order = arms[offset:] + arms[:offset]
        for mode in order[::(-1 if i % 2 else 1)]:
            with tempfile.TemporaryDirectory() as folder:
                result = subprocess.check_output([binary, repo, Path(folder)/"db",mode],text=True)
            row = {"backend":mode,"round":i}
            for line in result.splitlines():
                key, value = line.split()
                row.setdefault(key,[]).append(int(value))
            report["runs"].append(row)
            args.output.write_text(json.dumps(report)+"\n")
    assert git("rev-parse","HEAD")==report["revision"] and not git("status","--porcelain")
    for arm in arms:
        rows=[r for r in report["runs"] if r["backend"]==arm]
        def percentile(metric,p):
            values=sorted(v for row in rows for v in row[metric])
            return statistics.median(values) if p == .5 else values[round((len(values)-1)*p)]
        print(arm, {k: (percentile(k,.5),percentile(k,.95)) for k in
              ("fresh_us","edit_us","query_us","reopen_us","maintenance_us","churn_bytes","final_bytes")},flush=True)

if __name__=="__main__":
    main()
