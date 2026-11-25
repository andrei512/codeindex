"""Check each fixture's six observed inputs and its response to two injected faults."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
INPUTS = [-2, -1, 0, 1, 2, 3]
JARS = {
    "junit-4.11.jar": "90a8e1603eeca48e7e879f3afbc9560715322985f39a274f6f6070b43f9d06fe",
    "hamcrest-core-1.3.jar": "66fdef91e9739348df7a096aa384a5685f4e875584cce89386a7a47251c4d8e9",
}


def observed(proc):
    return sorted(int(n) for n in re.findall(r"CASE=(-?\d+)\b", proc.stdout))


def run_pair(pair, options, cache):
    if pair["kind"] == "fixture_layout":
        left = (ROOT / pair["left"]).read_text().split()
        right = (ROOT / pair["right"]).read_text().split()
        if left != right:
            raise ValueError(f"fixture layout changed more than whitespace: {pair['id']}")
    for side in ("left", "right"):
        source = ROOT / pair[side]
        with tempfile.TemporaryDirectory(prefix="cx-test-quality-") as tmp:
            work = Path(tmp)
            shutil.copy(source, work)
            shutil.copy(ROOT / pair["subject"], work)
            env = dict(os.environ)
            env.pop("CX_PAIR_MUTANT", None)
            language = pair["language"]
            container = language in options.docker
            prefix = []
            if container:
                image = {"go": "golang@sha256:1ae0735f00daffa3aaf1363a5184c0d2dc55c78e3db4ec70241cdac97bf84b59",
                         "java": "eclipse-temurin@sha256:7a0b2bc7e1372bb05ee361fb79e8a9e822df47343b8716253b3bbaa75060653e"}[language]
                prefix = ["docker", "run", "--rm", "--network=none", "-v", f"{work}:/case",
                          "-v", f"{cache}:/buildcache", "-w", "/case", "-e", "CX_PAIR_MUTANT",
                          "-e", "GO111MODULE=off", "-e", "GOCACHE=/buildcache", image]
            if language == "python":
                cmd = ["python3", "-m", "unittest", source.name]
            elif language in ("js", "ts"):
                cmd = ["node", "--test", source.name]
            elif language == "rust":
                subprocess.run(["rustc", "--edition=2021", "--test", source.name, "-o", "suite"], cwd=work, check=True)
                cmd = ["./suite", "--nocapture", "--test-threads=1"]
            elif language == "go":
                env.update(GO111MODULE="off")
                subprocess.run(prefix + ["go", "test", "-c", "-o", "suite"], cwd=work, env=env, check=True)
                cmd = ["./suite", "-test.v"]
            elif language == "java":
                for jar, digest in JARS.items():
                    path = options.junit_dir / jar
                    if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
                        raise ValueError(f"JUnit dependency differs: {jar}")
                    shutil.copy(path, work)
                cp = os.pathsep.join([".", *JARS])
                subprocess.run(prefix + ["javac", "-cp", cp, source.name, "Subject.java"], cwd=work, check=True)
                cmd = ["java", "-cp", cp, "org.junit.runner.JUnitCore", "TransformTest"]
            else:
                raise ValueError(language)
            for mutation in (None, *pair["representative_mutants"]):
                if mutation is not None:
                    env["CX_PAIR_MUTANT"] = str(mutation)
                proc = subprocess.run(prefix + cmd, cwd=work, env=env, stdout=subprocess.PIPE,
                                      stderr=subprocess.STDOUT, text=True, timeout=60)
                calls = observed(proc)
                ok = (proc.returncode == 0 and calls == INPUTS) if mutation is None else (
                    proc.returncode == (101 if language == "rust" else 1) and mutation in calls)
                if not ok:
                    raise RuntimeError(f"{pair['id']} {side} mutant={mutation}, exit={proc.returncode}, "
                                       f"inputs={calls}\n{proc.stdout}")
    print(pair["id"], "PASS", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--language", action="append", choices=["go", "java", "js", "ts", "python", "rust"])
    parser.add_argument("--docker", action="append", choices=["go", "java"], default=[],
                        help="use the documented Docker image instead of a local toolchain")
    parser.add_argument("--junit-dir", type=Path, default=ROOT.parents[2] / "target/test-role-eval/junit-discovery/lib")
    options = parser.parse_args()
    pairs = json.loads((ROOT / "manifest.json").read_text())["pairs"]
    with tempfile.TemporaryDirectory(prefix="cx-quality-compile-cache-") as cache:
        for pair in pairs:
            if options.language is None or pair["language"] in options.language:
                run_pair(pair, options, cache)


if __name__ == "__main__":
    main()
