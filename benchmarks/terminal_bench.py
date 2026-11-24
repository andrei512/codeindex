"""Run paired Codex trials on a named public Terminal-Bench 2.0 task."""
import argparse
import json
import subprocess
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def summarize(trial):
    result = json.loads((trial / "result.json").read_text())
    trajectory = trial / "agent/trajectory.json"
    trace = json.loads(trajectory.read_text()) if trajectory.exists() else {}
    return {
        "trial": trial.name,
        "reward": (result.get("verifier_result") or {}).get("rewards"),
        "error": result.get("exception_info"),
        "started_at": result.get("started_at"),
        "finished_at": result.get("finished_at"),
        "usage": trace.get("final_metrics"),
        "tool_calls": sum(len(s.get("tool_calls") or []) for s in trace["steps"]) if "steps" in trace else None,
    }


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("task", help="exact Terminal-Bench 2.0 task name")
    p.add_argument("--model", required=True, help="Harbor model identifier")
    p.add_argument("--cx-binary", required=True, type=Path, help="Linux cx executable for the Docker architecture")
    p.add_argument("--attempts", default=1, type=int)
    p.add_argument("--dry-run", action="store_true")
    args = p.parse_args()
    binary = args.cx_binary.resolve(strict=True)
    if args.attempts < 1 or not binary.stat().st_mode & 0o111:
        p.error("supply a positive attempt count and an executable cx binary")
    with binary.open("rb") as f:
        if f.read(4) != b"\x7fELF":
            p.error("the Docker environment requires a Linux ELF cx binary")
    jobs = ROOT / "target/terminal-bench" / uuid.uuid4().hex
    status = 0
    for arm in ("baseline", "cx"):
        cmd = ["uvx", "--from", "harbor==0.27.0", "harbor", "run",
               "-d", "terminal-bench@2.0", "-i", args.task, "-a", "codex",
               "--ak", "version=0.171.0", "--effort", "low", "-m", args.model,
               "-e", "docker", "-k", str(args.attempts), "--jobs-dir", str(jobs), "--job-name", arm]
        if arm == "cx":
            mount = {"type": "bind", "source": str(binary), "target": "/usr/local/bin/cx", "read_only": True}
            cmd += ["--mounts-json", json.dumps([mount]), "--extra-instruction-path",
                    str(ROOT / "src/adapters/cli/intro.txt")]
        if args.dry_run:
            print(json.dumps(cmd))
            continue
        status |= int(subprocess.run(cmd).returncode != 0)
        rows = [summarize(f.parent) for f in sorted((jobs / arm).glob("*/result.json"))]
        jobs.mkdir(parents=True, exist_ok=True)
        (jobs / (arm + ".json")).write_text(json.dumps(rows, indent=2) + "\n")
        status |= int(len(rows) != args.attempts or any(r["error"] or r["reward"] is None for r in rows))
    if not args.dry_run:
        print(jobs)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
