"""Paired full-CLI score timings using independent development checkouts."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def inputs():
    blobs = {}
    for group, names in [
        (HERE.parent / "test_roles", ("assets", "public", "runner")),
        (HERE, ("conditional", "dev_sample")),
    ]:
        for name in names:
            for line in (group / (name + ".jsonl")).read_text().splitlines():
                row = json.loads(line)
                if row["split"] == "dev":
                    blobs[row["repository"].lower(), row["path"]] = row["sha256"]
    for repo in json.loads(
        gzip.decompress((HERE / "results/baseline.json.gz").read_bytes())
    ):
        for row in repo["result"]["files"]:
            key = repo["repository"].lower(), row["path"]
            digest = blobs[key]
            data = (ROOT / "target/test-role-eval/blobs" / digest).read_bytes()
            if hashlib.sha256(data).hexdigest() != digest:
                raise ValueError(key)
            yield Path(*key), data


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, type=Path)
    parser.add_argument("--after", default=ROOT / "target/release/cx", type=Path)
    parser.add_argument("--rounds", default=21, type=int)
    args = parser.parse_args()
    binaries = [p.resolve() for p in (args.before, args.after)]
    files = list(inputs())
    with tempfile.TemporaryDirectory(prefix="cx-dev-score-") as tmp:
        checkouts = [Path(tmp) / name for name in ("before", "after")]
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        for root in checkouts:
            root.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=root, env=env, check=True)
            for path, data in files:
                dest = root / path
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(data)
            subprocess.run(["git", "add", "-f", "."], cwd=root, env=env, check=True)

        def query(i, home, standalone, baseline):
            home.mkdir(exist_ok=True)
            local = dict(env, HOME=str(home), XDG_CONFIG_HOME=str(home))
            if standalone:
                # A relative Git config requires the caller's working directory.
                local["GIT_CONFIG_GLOBAL"] = ".unused-config"
            cmd = [binaries[i], "--score", "--json"]
            if baseline:
                cmd.append("--ignore-test-rules")
            start = time.perf_counter_ns()
            data = subprocess.check_output(cmd, cwd=checkouts[i], env=local)
            return (time.perf_counter_ns() - start) / 1e6, hashlib.sha256(
                data
            ).hexdigest()

        report = {"files": len(files)}
        for mode in ("fresh_baseline", "reopened", "watcher"):
            count = min(9, args.rounds) if mode == "fresh_baseline" else args.rounds
            samples = [[], []]
            for r in range(count + 1):
                digests = []
                for i in [0, 1] if r % 2 == 0 else [1, 0]:
                    suffix = str(r) if mode == "fresh_baseline" else "warm"
                    home = Path(tmp) / f"home-{i}-{suffix}"
                    elapsed, digest = query(
                        i, home, mode != "watcher", mode == "fresh_baseline"
                    )
                    digests.append(digest)
                    if r:
                        samples[i].append(elapsed)
                if digests[0] != digests[1]:
                    raise ValueError(f"{mode}: score output changed")
            report[mode] = dict(
                rounds=count,
                before_ms=statistics.median(samples[0]),
                after_ms=statistics.median(samples[1]),
                paired_delta_ms=statistics.median(b - a for a, b in zip(*samples)),
                output_sha256=digests[0],
            )
        print(json.dumps(report, indent=2))


if __name__ == "__main__":
    run()
