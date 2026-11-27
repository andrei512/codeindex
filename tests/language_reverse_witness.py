"""Check Clang-labelled call sites against cx reverse results before any source intervention."""
from collections import defaultdict
from dataclasses import dataclass
import json
import os
import subprocess


def compiler_targets(filename, all_calls):
    by_site = defaultdict(set)
    for _, pos, target in all_calls:
        by_site[f"{filename}:{pos}"].add(f"{filename}:{target}")
    return by_site


def forward_labels(rows):
    by_target = defaultdict(list)
    for row in rows:
        if row["op"] == "definition":
            by_target[row["expected"]].append(row)
    return by_target


def adjudicate(target, checks, payload, by_site):
    hits = {f"{h['path']}:{h['line']}:{h['col']}" for h in payload["result"]}
    # Unlabelled sites may be valid non-call references and remain ungraded.
    wrong = sorted(site for site in hits if len(by_site.get(site, ())) == 1 and target not in by_site[site])
    missing = sorted(row["subject"] for row in checks if row["subject"] not in hits)
    truncated = payload["truncated"]
    missed_after_correct_forward = [] if truncated else [
        row["subject"] for row in checks if not row["diff"] and row["subject"] in missing
    ]
    return dict(
        target=target, checked=len(checks), wrong=wrong,
        missing=[] if truncated else missing, censored=missing if truncated else [],
        missing_after_correct_forward=missed_after_correct_forward,
    )


@dataclass(frozen=True)
class ReverseProbe:
    binary: object
    repo: object
    home: object

    def query(self, target):
        proc = subprocess.run(
            [self.binary, "--cx", "references", *target.rsplit(":", 2)],
            cwd=self.repo, capture_output=True, text=True, timeout=120,
            env={**os.environ, "HOME": str(self.home), "GIT_CONFIG_GLOBAL": "./.missing-config",
                 "GIT_CONFIG_NOSYSTEM": "1"},
        )
        if proc.returncode:
            raise RuntimeError(proc.stderr)
        return json.loads(proc.stdout)


def reverse_witness(probe, filename, all_calls, sampled_rows):
    by_site = compiler_targets(filename, all_calls)
    return [
        adjudicate(target, checks, probe.query(target), by_site)
        for target, checks in forward_labels(sampled_rows).items()
    ]
