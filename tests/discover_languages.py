"""Run the language audits and retain every suite's output, including failures."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
TESTS = ROOT / "tests"
OUT = ROOT / "target/discovery"


def run(name, command):
    started = time.monotonic()
    log = OUT / (name + ".log")
    with log.open("w") as output:
        try:
            result = subprocess.run(command, cwd=ROOT, stdout=output, stderr=subprocess.STDOUT)
            code = result.returncode
        except OSError as exc:
            output.write(str(exc) + "\n")
            code = 2
    print(f"{name}: {'pass' if code == 0 else 'see ' + str(log.relative_to(ROOT))}", flush=True)
    return {"exit": code, "elapsed_seconds": round(time.monotonic() - started, 2),
            "log": str(log.relative_to(ROOT))}


def language_command(name, public):
    command = [sys.executable, TESTS / f"language_{name}.py"]
    if public and name in ("conformance", "named_references"):
        command.append("--public")
    return command


def run_public_oracles():
    summaries = {}
    summaries["python_oracle"] = run("python_oracle", [sys.executable, TESTS / "language_python_oracle.py"])
    for name in ("compiled_oracle", "format_oracle"):
        summaries[name] = run(name, ["uv", "run", TESTS / f"language_{name}.py"])
    # Install the pinned reference tool in the test cache; cx has no runtime dependency on it.
    prefix = ROOT / "target/typescript-oracle"
    summaries["typescript_setup"] = run("typescript_setup", [
        "npm", "install", "--prefix", prefix, "--no-save", "--package-lock=false", "--ignore-scripts",
        "--no-audit", "--no-fund", "typescript@5.9.3"])
    if not summaries["typescript_setup"]["exit"]:
        summaries["script_oracle"] = run("script_oracle", [
            "node", TESTS / "language_script_oracle.cjs", prefix / "node_modules/typescript"])
    return summaries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--public", action="store_true", help="include pinned public projects and independent compiler/parser checks")
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    summaries = {}
    for name in ("conformance", "paths", "named_references", "compiled_sweeps", "script_sweeps",
                 "format_sweeps", "repository_transitions"):
        summaries[name] = run(name, language_command(name, args.public))
    if args.public:
        summaries.update(run_public_oracles())
    (OUT / "suites.json").write_text(json.dumps(summaries, indent=2) + "\n")
    return int(any(result["exit"] for result in summaries.values()))


if __name__ == "__main__":
    raise SystemExit(main())
