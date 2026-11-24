# /// script
# requires-python = ">=3.9"
# dependencies = ["html5lib==1.1"]
# ///
"""Run with uv run tests/language_format_oracle.py after cargo build --release."""
import argparse
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import quote
from urllib.request import urlopen

import html5lib

from language_conformance import ROOT, git

# Cohorts frozen before measuring. Sample MDN by a hash of the path irrespective of source.
CORPUS = (
    ("andybrewer/mvp", "629ba5eb91362afe2504fdd09a1ac039e2fca420", "", None),
    ("StartBootstrap/startbootstrap-clean-blog", "1ebc4f8f3b6194335df237a2a7837955a5a4a9aa", "dist/", None),
    ("mdn/dom-examples", "a2c57197c219bd1c87d48aa901f4c2a24b249658", "", 48),
)
NAME = r"[A-Za-z_][A-Za-z0-9_.:-]*"
# Match a possible literal id assignment anywhere in source, including inert strings.
CANDIDATE = re.compile(r"""(?<![\w:-])id\s*=\s*(?:"(""" + NAME + r""")"|'(""" + NAME
                       + r""")'|(""" + NAME + r""")(?=[\s>]))""", re.IGNORECASE | re.ASCII)


def fetch(url):
    with urlopen(url, timeout=45) as response:
        return response.read()


def sources(project, commit, prefix, limit, cache):
    directory = cache / commit
    directory.mkdir(parents=True, exist_ok=True)
    tree_file = directory / "tree.json"
    if not tree_file.exists():
        tree_file.write_bytes(fetch(f"https://api.github.com/repos/{project}/git/trees/{commit}?recursive=1"))
    tree = json.loads(tree_file.read_bytes())
    if tree["truncated"]:
        raise ValueError(f"incomplete source tree for {project}")
    rows = [(r["path"], r["sha"]) for r in tree["tree"] if r["type"] == "blob"
            and r["path"].startswith(prefix) and r["path"].lower().endswith((".html", ".htm"))]
    rows.sort(key=lambda row: hashlib.sha256(row[0].encode()).digest())

    def read(row):
        path, blob = row
        cached = directory / blob
        body = cached.read_bytes() if cached.is_file() else fetch(
            f"https://raw.githubusercontent.com/{project}/{commit}/{quote(path)}")
        identity = hashlib.sha1(b"blob " + str(len(body)).encode() + b"\0" + body).hexdigest()
        if identity != blob:
            raise ValueError(f"blob mismatch for {project}/{path}")
        if not cached.exists():
            cached.write_bytes(body)
        return path, body.decode("utf-8")
    with ThreadPoolExecutor(max_workers=6) as pool:
        return list(pool.map(read, rows[:limit]))


def dom_ids(source):
    return [e.attrib["id"] for e in html5lib.parse(source).iter() if "id" in e.attrib]


def witness(source):
    # Replace each lexical candidate's VALUE with a unique inert identifier, then
    # map only the identifiers observed in the parsed DOM back to original spans.
    prefix = "cxOracleTag"
    while prefix in source:
        prefix += "Z"
    sites = {}
    names = set()

    def mark(match):
        start, end = match.span(match.lastindex)
        name = source[start:end]
        names.add(name)
        line = 1 + source.count("\n", 0, start)
        col = 1 + len(source[source.rfind("\n", 0, start) + 1:start].encode("utf-16-le")) // 2
        tag = prefix + str(len(sites))
        sites[tag] = (name, line, col)
        return source[match.start():start] + tag + source[end:match.end()]

    marked = CANDIDATE.sub(mark, source)
    expected = defaultdict(list)
    reconstructed = []
    for tag in dom_ids(marked):
        if tag in sites:
            name, line, col = sites[tag]
            expected[name].append((line, col))
            reconstructed.append(name)
        else:
            reconstructed.append(tag)
    original_ids = dom_ids(source)
    if Counter(reconstructed) != Counter(original_ids):
        raise ValueError("source annotation changed the HTML5 DOM id multiset")
    # Entity spellings can decode to the same name as a literal candidate.
    # A missing independent source location leaves that name unverified.
    complete = Counter(original_ids)
    unanchored = sorted(n for n in names | set(original_ids) if len(expected[n]) != complete[n])
    return {n: sorted(expected[n]) for n in sorted(names) if n not in unanchored}, unanchored


def query_file(binary, label, body):
    expected, unverified = witness(body)
    diffs = []
    with tempfile.TemporaryDirectory(prefix="cx-html-oracle-") as tmp:
        repo, home = Path(tmp) / "repo", Path(tmp) / "home"
        repo.mkdir()
        home.mkdir()
        git("init", "-q", cwd=repo)
        path = "sample.html"
        (repo / path).write_bytes(body.encode())
        for name, sites in expected.items():
            result = subprocess.run([binary, "--symbol", name, "--json"], cwd=repo,
                                    env={**os.environ, "HOME":str(home), "GIT_CONFIG_GLOBAL":"./.missing-config",
                                         "GIT_CONFIG_NOSYSTEM":"1"}, capture_output=True, text=True, timeout=60)
            answer = json.loads(result.stdout)
            # Embedded JS and CSS may have independent declarations of this name.
            rows = [(r["line"], r["col"]) for r in answer["hits"] if r["kind"] == "attribute"]
            if answer["truncated"]:
                unverified.append(name)
            elif (sorted(rows) != sites or result.returncode != int(not answer["hits"])
                  or any(r["path"] != path or r["name"] != name for r in answer["hits"])):
                diffs.append(dict(name=name, expected=sites, actual=rows, exit_code=result.returncode))
    return dict(path=label, queries=len(expected), witnessed_ids=sum(map(len, expected.values())),
                negative_queries=sum(not s for s in expected.values()), unverified=unverified, diffs=diffs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-format-oracle.json")
    args = parser.parse_args()
    if html5lib.__version__ != "1.1":
        raise ValueError("expected html5lib 1.1")
    # A false id in raw text and duplicate real ids must survive the independent
    # mapping as three distinct source candidates, including the rejected one.
    control = '<iframe><a id="x">x</a></iframe>\n<div id="y"></div><p id=y></p>'
    expected, gaps = witness(control)
    if expected != {"x": [], "y": [(2, 10), (2, 25)]} or gaps:
        raise ValueError(f"HTML5 witness failed its controls: {expected} {gaps}")
    result = {}
    for project, commit, prefix, limit in CORPUS:
        rows = sources(project, commit, prefix, limit, ROOT / "target/html-oracle-sources")
        reports = [query_file(args.binary.resolve(), path, source) for path, source in rows]
        result[project] = dict(commit=commit, files=reports)
        print(project, len(rows), "files", sum(r["queries"] for r in reports), "queries",
              sum(len(r["diffs"]) for r in reports), "mismatches", flush=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=2) + "\n")
    return int(any(f["diffs"] or f["unverified"] for pkg in result.values() for f in pkg["files"]))


if __name__ == "__main__":
    raise SystemExit(main())
