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
    repo = args.repo.resolve()
    if git(repo, "status", "--porcelain"):
        parser.error("benchmark requires a clean checkout")
    revision = git(repo, "rev-parse", "HEAD")
    checks = json.loads(args.checks.read_text()) if args.checks else {}
    if not isinstance(checks, dict) or any(not isinstance(args, list) or not args or
                                           not all(isinstance(v, str) for v in args)
                                           for args in checks.values()):
        parser.error("checks must map query names to nonempty arrays of strings")
    source_cache = None
    if not args.warm_source_cache:
        from source_cache import ColdSourceCache
        source_cache = ColdSourceCache(repo)
    cache_mode = "cold" if source_cache else "warm"
    expected = None
    arms = {k: getattr(args, k).resolve() for k in ("baseline", "candidate")}
    # Old releases use positional commands. Probe before timing either arm.
    prefix = {k: (["--cx"] if b"--cx COMMAND" in subprocess.check_output([v, "--help"]) else [])
              for k, v in arms.items()}
    report = dict(command=sys.argv, revision=revision, os=platform.platform(),
                  logical_cpus=os.cpu_count(), cache=f"{cache_mode} source cache, empty index, standalone CLI",
                  binary_sha256={k: hashlib.sha256(p.read_bytes()).hexdigest() for k, p in arms.items()},
                  rows=[])
    if sys.platform == "darwin":
        report["hardware"] = subprocess.check_output(
            ["sysctl", "hw.model", "machdep.cpu.brand_string", "hw.memsize", "hw.logicalcpu"], text=True).strip()
        levels = subprocess.run(["sysctl", "-n", "hw.nperflevels"], capture_output=True, text=True)
        if levels.returncode == 0:
            for i in range(int(levels.stdout)):
                report["hardware"] += "\n" + subprocess.check_output(
                    ["sysctl", f"hw.perflevel{i}.name", f"hw.perflevel{i}.logicalcpu"], text=True).strip()
    for rep in range(args.rounds + 1):
        for name in (list(arms) if rep % 2 == 0 else list(reversed(arms))):
            with tempfile.TemporaryDirectory() as home:
                env = dict(os.environ, HOME=home, XDG_CONFIG_HOME=str(Path(home) / ".config"),
                           GIT_CONFIG_GLOBAL="./.cx-bench-absent-config", GIT_CONFIG_NOSYSTEM="1")
                cache_check = source_cache.prepare() if source_cache else None
                start = time.monotonic_ns()
                first, build = invoke(arms[name], repo, env, *prefix[name], "path-exact", args.witness)
                result, reopen = invoke(arms[name], repo, env, *prefix[name], "path-exact", args.witness)
                elapsed = (time.monotonic_ns() - start) / 1e6
                if (first["generation"] != 1 or first["reused"] or first["hits"] != [{"path": args.witness}]
                        or result["generation"] != 1 or not result["reused"]
                        or [hit["path"] for hit in result["hits"]] != [args.witness]):
                    raise ValueError("fresh publication and reopen verification failed")
                signatures = {}
                for query, arguments in checks.items():
                    answer, _ = invoke(arms[name], repo, env, *prefix[name], *arguments)
                    if isinstance(answer, dict):
                        answer.pop("generation", None)
                        answer.pop("reused", None)
                    data = json.dumps(answer, ensure_ascii=False, sort_keys=True,
                                      separators=(",", ":")).encode()
                    signatures[query] = hashlib.sha256(data).hexdigest()
                if expected is None:
                    expected = signatures
                    report["query_sha256"] = signatures
                    report["queries"] = checks
                if expected != signatures:
                    diff = sorted(k for k in checks if signatures[k] != expected[k])
                    raise ValueError(f"query results differ for {name} on {diff}")
                report["rows"].append(dict(round=rep, warmup=rep == 0, arm=name,
                                           total_ms=elapsed, build=build, reopen=reopen,
                                           source_cache_check=cache_check))
    if git(repo, "rev-parse", "HEAD") != revision or git(repo, "status", "--porcelain"):
        raise ValueError("checkout changed during benchmark")
    report["median_ms"] = {k: statistics.median(r["total_ms"] for r in report["rows"]
                                                if r["arm"] == k and not r["warmup"]) for k in arms}
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report["median_ms"]))


if __name__ == "__main__":
    main()
