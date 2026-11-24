"""Build cx for this CPU using a profile from the toolchain's alloc and core sources.

Requires a Unix host, Git, Cargo, and the rust-src and llvm-tools components.
Example: python3 benchmarks/build_profiled.py --out /tmp/cx-profile
The output directory must be new. Its build.json records flags and binary hashes.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    root = args.source.resolve()
    out = args.out.resolve()
    toolchain = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.split()[1] for line in toolchain.splitlines() if line.startswith("host:"))
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    libraries = sysroot / "lib/rustlib/src/rust/library"
    profdata = sysroot / "lib/rustlib" / host / "bin/llvm-profdata"
    if not profdata.is_file() or not (libraries / "core").is_dir():
        parser.error("install rust-src and llvm-tools for the active Rust toolchain")
    out.mkdir(parents=True)
    raw = out / "raw"
    raw.mkdir()
    report = {"toolchain": toolchain, "host": host, "training": ["alloc", "core"], "builds": {}}

    def build(label, extra):
        flags = "-Ctarget-cpu=native" + extra
        env = dict(os.environ, CARGO_TARGET_DIR=str(out / label), CARGO_PROFILE_RELEASE_LTO="thin",
                   CARGO_PROFILE_RELEASE_CODEGEN_UNITS="1", RUSTFLAGS=flags)
        command = ["cargo", "build", "--target", host, "--release", "--locked"]
        subprocess.run(command, check=True, cwd=root, env=env)
        binary = out / label / host / "release/cx"
        report["builds"][label] = dict(command=command, rustflags=flags, lto="thin", codegen_units=1,
                                       binary=str(binary), sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
        return binary

    build("native", "")
    instrumented = build("training", " -Cprofile-generate=" + str(raw))
    for library in report["training"]:
        with tempfile.TemporaryDirectory(dir=out) as scratch:
            repo = Path(scratch) / "repo"
            shutil.copytree(libraries / library, repo, symlinks=True)
            env = dict(os.environ, HOME=scratch, GIT_CONFIG_GLOBAL="./.cx-profile-absent",
                       GIT_CONFIG_NOSYSTEM="1")
            for cmd in [["git", "init", "-q"], ["git", "add", "-A"],
                        ["git", "-c", "user.name=Benchmark", "-c", "user.email=bench@localhost",
                         "commit", "-qm", "training fixture"],
                        [instrumented, "--cx", "path-exact", "src/lib.rs"], [instrumented, "--cx", "symbol", "Vec"]]:
                subprocess.run(cmd, cwd=repo, env=env, check=True, stdout=subprocess.DEVNULL)
    merged = out / "training.profdata"
    subprocess.run([profdata, "merge", "-o", merged, raw], check=True)
    final = build("pgo", " -Cprofile-use=" + str(merged))
    (out / "build.json").write_text(json.dumps(report, indent=2) + "\n")
    print(final)


if __name__ == "__main__":
    main()
