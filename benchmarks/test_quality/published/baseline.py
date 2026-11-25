"""Run the compatibility scorer on AromaLIA's pinned source fixtures."""
import argparse
import collections
import hashlib
import json
import os
import subprocess
import tempfile
import urllib.request
import zipfile
from pathlib import Path

from panel import AROMA, DEFAULT, HERE, aroma_panel, check_dev_only

ROOT = HERE.parents[2]


def source_archive(source, cache):
    slug, revision = source["repository"], source["revision"]
    archive = cache / (revision + ".zip")
    if not archive.exists():
        with urllib.request.urlopen(f"https://codeload.github.com/{slug}/zip/{revision}", timeout=60) as r:
            content = r.read()
        with tempfile.NamedTemporaryFile(dir=cache, delete=False) as tmp:
            tmp.write(content)
        Path(tmp.name).replace(archive)
    return zipfile.ZipFile(archive)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--out", type=Path, default=DEFAULT)
    parser.add_argument("--candidate", action="store_true", help="run default scoring instead of the frozen policy")
    args = parser.parse_args()
    binary = args.binary.resolve()
    cache = args.out.resolve()
    (cache / "blobs").mkdir(parents=True, exist_ok=True)
    source = json.loads((HERE / "sources.json").read_text())[AROMA]
    rows = aroma_panel(cache / "blobs", source)
    check_dev_only(rows)
    with tempfile.TemporaryDirectory() as tmp:
        repo = Path(tmp) / "repo"
        home = Path(tmp) / "home"
        repo.mkdir()
        home.mkdir()
        digest = hashlib.sha256()
        with source_archive(source, cache) as archive:
            prefix = source["repository"].split("/")[1] + "-" + source["revision"]
            for row in rows:
                path = row["path"]
                b = archive.read(f"{prefix}/{path}")
                digest.update(path.encode() + b"\0" + hashlib.sha256(b).digest())
                dest = repo / path
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(b)
        if digest.hexdigest() != source["fixture_bundle_sha256"]:
            raise ValueError(f"fixture bytes differ from frozen bundle: {digest.hexdigest()}")
        env = {k:v for k,v in os.environ.items() if not k.startswith("GIT_")}
        env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"))
        # Plain directory mode builds a private in-memory index and returns all files.
        flags = [] if args.candidate else ["--ignore-test-rules"]
        result = subprocess.check_output([binary, "--score", *flags, "--json"],
                                         env=env, cwd=repo)
    files = {f["path"]: f for f in json.loads(result)["files"]}
    if set(files) != {r["path"] for r in rows}:
        raise ValueError("scored paths differ from expected paths")
    findings = collections.Counter()
    any_finding = 0
    for f in files.values():
        findings.update(hit["rule"] for hit in f["findings"])
        any_finding += bool(f["findings"])
    report = dict(dataset=AROMA, files_scored=len(files), files_with_findings=any_finding,
                  files_without_findings=len(files)-any_finding, findings_by_rule=dict(sorted(findings.items())),
                  published_file_rule_decisions=sum(len(row["labels"]) for row in rows),
                  directly_equivalent_published_rules=[], binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  fixture_bundle_sha256=digest.hexdigest())
    mode = "candidate" if args.candidate else "baseline"
    (cache / f"{mode}.observations.json").write_bytes(result)
    (cache / f"{mode}.json").write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
