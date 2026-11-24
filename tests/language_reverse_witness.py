"""Check Clang-labelled call sites against cx reverse results before any source intervention."""
from collections import defaultdict
import json
import os
import subprocess


def reverse_witness(binary, repo, home, filename, all_calls, sampled_rows):
    by_site = defaultdict(set)
    for _, pos, target in all_calls:
        by_site[f"{filename}:{pos}"].add(f"{filename}:{target}")
    by_target = defaultdict(list)
    for row in sampled_rows:
        if row["op"] == "definition":
            by_target[row["expected"]].append(row)
    report = []
    for target, checks in by_target.items():
        proc = subprocess.run([binary, "--cx", "references", *target.rsplit(":", 2)],
                              cwd=repo, capture_output=True, text=True, timeout=120,
                              env={**os.environ, "HOME": str(home), "GIT_CONFIG_GLOBAL": "./.missing-config",
                                   "GIT_CONFIG_NOSYSTEM": "1"})
        if proc.returncode:
            raise RuntimeError(proc.stderr)
        payload = json.loads(proc.stdout)
        hits = {f"{h['path']}:{h['line']}:{h['col']}" for h in payload["result"]}
        # The AST census only covers eligible direct calls in this unit. Other
        # locations may be valid non-call references and remain ungraded here.
        wrong = sorted(site for site in hits if len(by_site.get(site, ())) == 1 and target not in by_site[site])
        missing = sorted(r["subject"] for r in checks if r["subject"] not in hits)
        report.append(dict(target=target, checked=len(checks), wrong=wrong,
                           missing=[] if payload["truncated"] else missing,
                           censored=missing if payload["truncated"] else [],
                           missing_after_correct_forward=[r["subject"] for r in checks
                              if not r["diff"] and r["subject"] in missing and not payload["truncated"]]))
    return report
