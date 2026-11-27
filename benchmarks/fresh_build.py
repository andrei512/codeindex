"""Time durable fresh builds and standalone reopen on fixed local checkouts.

Source file data is evicted and checked before each arm by default (macOS/Linux).
Use --warm-source-cache for the original warm-filesystem benchmark.

python3 benchmarks/fresh_build.py --baseline /path/to/old/cx --candidate /path/to/new/cx \
    --repo /path/to/checkout --witness src/lib.rs --rounds 7 --output result.json
"""
import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import NamedTuple


def git(repo, *args):
    return subprocess.check_output(["git", "-C", repo, *args], text=True).strip()


def invoke(binary, repo, env, *args):
    # Collect each child separately. RUSAGE_CHILDREN's high-water mark would
    # leak the previous arm's peak RSS into subsequent observations.
    with tempfile.TemporaryFile() as out:
        start = time.monotonic_ns()
        with subprocess.Popen([binary, *args], cwd=repo, env=env, stdout=out) as p:
            _, status, usage = os.wait4(p.pid, 0)
            end = time.monotonic_ns()
            p.returncode = os.waitstatus_to_exitcode(status)
            if p.returncode:
                raise subprocess.CalledProcessError(p.returncode, p.args)
        out.seek(0)
        answer = json.load(out)
    return answer, dict(wall_ms=(end - start) / 1e6, user_ms=usage.ru_utime * 1000,
                        system_ms=usage.ru_stime * 1000,
                        peak_rss_bytes=usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024))


class Arm(NamedTuple):
    binary: Path
    prefix: list[str]

    def query(self, repo, env, *arguments):
        return invoke(self.binary, repo, env, *self.prefix, *arguments)


class Checkout(NamedTuple):
    path: Path
    revision: str

    @classmethod
    def clean(cls, path):
        if git(path, "status", "--porcelain"):
            return None
        return cls(path, git(path, "rev-parse", "HEAD"))

    def verify(self):
        if Checkout.clean(self.path) != self:
            raise ValueError("checkout changed during benchmark")


class Trial(NamedTuple):
    checkout: Checkout
    witness: str
    source_cache: object
    checks: dict

    def valid_witness(self, reply, reused):
        generation = reply["generation"]
        if type(generation) is not int or generation != 1:
            return False
        if reply["reused"] is not reused:
            return False
        hits = reply["hits"]
        if reused:
            return [hit["path"] for hit in hits] == [self.witness]
        return hits == [{"path": self.witness}]

    def measure(self, arm):
        with tempfile.TemporaryDirectory() as home:
            env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=str(Path(home) / ".config"),
                       GIT_CONFIG_GLOBAL="./.cx-bench-absent-config", GIT_CONFIG_NOSYSTEM="1")
            cache_check = self.source_cache.prepare() if self.source_cache else None
            repo = self.checkout.path
            start = time.monotonic_ns()
            first, build = arm.query(repo, env, "path-exact", self.witness)
            reopened, reopen = arm.query(repo, env, "path-exact", self.witness)
            elapsed = (time.monotonic_ns() - start) / 1e6
            if not self.valid_witness(first, False) or not self.valid_witness(reopened, True):
                raise ValueError("fresh publication and reopen verification failed")
            signatures = query_signatures(arm, repo, env, self.checks)
        return dict(total_ms=elapsed, build=build, reopen=reopen, source_cache_check=cache_check), signatures


def query_signatures(arm, repo, env, checks):
    signatures = {}
    for query, arguments in checks.items():
        answer, _ = arm.query(repo, env, *arguments)
        if isinstance(answer, dict):
            answer.pop("generation", None)
            answer.pop("reused", None)
        data = json.dumps(answer, ensure_ascii=False, sort_keys=True,
                          separators=(",", ":")).encode()
        signatures[query] = hashlib.sha256(data).hexdigest()
    return signatures


def mac_hardware():
    if sys.platform != "darwin":
        return None
    hardware = subprocess.check_output(
        ["sysctl", "hw.model", "machdep.cpu.brand_string", "hw.memsize", "hw.logicalcpu"], text=True).strip()
    levels = subprocess.run(["sysctl", "-n", "hw.nperflevels"], capture_output=True, text=True)
    if levels.returncode == 0:
        for i in range(int(levels.stdout)):
            hardware += "\n" + subprocess.check_output(
                ["sysctl", f"hw.perflevel{i}.name", f"hw.perflevel{i}.logicalcpu"], text=True).strip()
    return hardware


def sample_pairs(report, arms, trial, rounds):
    expected = None
    for rep in range(rounds + 1):
        for name in (list(arms) if rep % 2 == 0 else list(reversed(arms))):
            row, signatures = trial.measure(arms[name])
            if expected is None:
                expected = signatures
                report["query_sha256"] = signatures
                report["queries"] = trial.checks
            if expected != signatures:
                diff = sorted(k for k in trial.checks if signatures[k] != expected[k])
                raise ValueError(f"query results differ for {name} on {diff}")
            report["rows"].append(dict(round=rep, warmup=rep == 0, arm=name, **row))


def valid_arguments(args):
    return isinstance(args, list) and bool(args) and all(isinstance(value, str) for value in args)


def read_checks(path, parser):
    checks = json.loads(path.read_text()) if path else {}
    if not isinstance(checks, dict) or any(not valid_arguments(args) for args in checks.values()):
        parser.error("checks must map query names to nonempty arrays of strings")
    return checks


def benchmark_arm(path):
    binary = path.resolve()
    help_text = subprocess.check_output([binary, "--help"])
    prefix = ["--cx"] if b"--cx COMMAND" in help_text else []
    return Arm(binary, prefix)


def benchmark_trial(args, parser):
    repo = args.repo.resolve()
    checkout = Checkout.clean(repo)
    if checkout is None:
        parser.error("benchmark requires a clean checkout")
    checks = read_checks(args.checks, parser)
    source_cache = None
    if not args.warm_source_cache:
        from source_cache import ColdSourceCache
        source_cache = ColdSourceCache(repo)
    trial = Trial(checkout, args.witness, source_cache, checks)
    return trial


def run_benchmark(args, parser):
    trial = benchmark_trial(args, parser)
    checkout = trial.checkout
    cache_mode = "cold" if trial.source_cache else "warm"
    arms = {name: benchmark_arm(getattr(args, name)) for name in ("baseline","candidate")}
    report = dict(command=sys.argv, revision=checkout.revision, os=platform.platform(),
                  logical_cpus=os.cpu_count(), cache=f"{cache_mode} source cache, empty index, standalone CLI",
                  binary_sha256={k: hashlib.sha256(arm.binary.read_bytes()).hexdigest() for k, arm in arms.items()},
                  rows=[])
    hardware = mac_hardware()
    if hardware:
        report["hardware"] = hardware
    sample_pairs(report, arms, trial, args.rounds)
    checkout.verify()
    report["median_ms"] = {k: statistics.median(r["total_ms"] for r in report["rows"]
                                                if r["arm"] == k and not r["warmup"]) for k in arms}
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["baseline", "candidate", "repo", "output"]:
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--witness", required=True, help="indexed repository-relative file")
    parser.add_argument("--checks", type=Path,
                        help="JSON object of named CLI argument arrays verified after timing")
    parser.add_argument("--rounds", type=int, default=7)
    parser.add_argument("--warm-source-cache", action="store_true",
                        help="keep source data cached instead of evicting and verifying before each arm")
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("rounds must be positive")
    report = run_benchmark(args, parser)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["median_ms"]))


if __name__ == "__main__":
    main()
