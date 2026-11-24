"""Audit cx --reference NAME using independent symbol and reference fixture rows.

An ambiguous name needs two independently labelled declarations. A unique name
needs an exhaustive single symbol result and a matching positional reference
answer. A line-only declaration anchor is admitted only when that spelling
occurs once on the source line, at the labelled positional query. Partial lists
can establish ambiguity or positive reference membership, never uniqueness.
Three small closed fixtures also have independently checked reference lists,
recorded below. Run with --public to include the pinned checkouts. Only
baseline files are used.
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

from language_conformance import FIXTURES, ROOT, checkout, compare, git


# vscode-langservers-extracted 4.10.0 forwards position and stylesheet to
# vscode-css-languageservice (6.3.10 verified). With either includeDeclaration
# setting it returns the .card selector including its dot: line 2, column 1.
# The HTML id live has no
# fragment link or script use. Each PL/pgSQL routine contains only labelled
# blocks and EXIT statements. Neither routine is invoked by the other.
SUPPLEMENTAL_REFERENCES = {
    "css/discovery_nested_var": {"style.css:2:2": "style.css:2:1"},
    "html/discovery_inert_script": {"index.html:3:10": "-"},
    "plpgsql/scoped_block_labels": {"work.plpgsql:1:17": "-", "work.plpgsql:7:17": "-"},
}


def fixture_rows(case):
    for line in (case / "checks.tsv").read_text().splitlines():
        if not line or line.startswith(("#", "?")):
            continue
        op, subject, expected, *options = line.split("\t")
        yield op, subject, expected, options[0] if options else "", options[1:] == ["contains"]


def invoke(binary, repo, home, *args):
    return subprocess.run([binary, *args], cwd=repo, text=True, capture_output=True, timeout=120,
                          env={**os.environ, "HOME": str(home), "GIT_CONFIG_GLOBAL": "./.missing-config",
                               "GIT_CONFIG_NOSYSTEM": "1"})


def location(repo, anchor, name):
    parts = anchor.split("|", 1)[0].split(":")
    path, line = parts[:2]
    text = (repo / path).read_text().splitlines()[int(line) - 1]
    if len(parts) == 3:
        column = int(parts[2])
    elif text.count(name) == 1:
        column = len(text[:text.index(name)].encode("utf-16-le")) // 2 + 1
    else:
        return None
    prefix = text.encode("utf-16-le")[:2 * (column - 1)].decode("utf-16-le")
    return path, line, column, len(prefix.encode("utf-8")) + 1


def ambiguous(binary, repo, home, name, anchors):
    attempt = invoke(binary, repo, home, "--reference", name)
    result = {"exit": attempt.returncode, "diagnostic": attempt.stderr.strip(), "diff": {}}
    if attempt.returncode != 2 or "ambiguous" not in attempt.stderr.lower() or attempt.stdout:
        result["diff"]["ambiguity"] = "expected exit 2, ambiguity on stderr and empty stdout"
        return result
    # Replay only the supported advice instead of evaluating arbitrary diagnostics.
    match = re.search(r"\bcx --symbol " + re.escape(name) + r"(?=\s|$)((?:\s+--(?:column|json))*)", attempt.stderr)
    if not match:
        result["diff"]["advice"] = "no symbol lookup command for the ambiguous name"
        return result
    flags = match.group(1).split()
    replay = invoke(binary, repo, home, "--symbol", name, *flags)
    if replay.returncode:
        result["diff"]["advice"] = f"recommended symbol lookup exited {replay.returncode}"
        return result
    actual = set()
    if "--json" in flags:
        for hit in json.loads(replay.stdout)["hits"]:
            actual.add((hit["path"], str(hit["line"]), hit["byte_column"]))
    else:
        for line in replay.stdout.splitlines():
            m = re.match(r"^([^:]+):(\d+):(\d+):", line)
            if m:
                actual.add((m[1], m[2], int(m[3])))
    for anchor in anchors:
        pos = location(repo, anchor, name)
        if pos is None:
            # A line-only expectation can establish the need for a choice even
            # when the expected byte column itself cannot be reconstructed.
            path, line = anchor.split("|", 1)[0].split(":")
            found = any(x[:2] == (path, line) for x in actual)
        else:
            found = (pos[0], pos[1], pos[3]) in actual
        if not found:
            result["diff"]["advice"] = "recommended command omits a verified candidate or its byte column"
            break
    return result


def run_case(case, binary, cache, records):
    rows = list(fixture_rows(case))
    by_site = {subject: (expected, status, subset) for op, subject, expected, status, subset in rows
               if op == "references"}
    for anchor, sites in SUPPLEMENTAL_REFERENCES.get(str(case.relative_to(FIXTURES)), {}).items():
        by_site.setdefault(anchor, (sites, "resolved", False))
    if not any(op == "symbol" and expected != "-" for op, _, expected, _, _ in rows):
        return
    with tempfile.TemporaryDirectory(prefix="cx-named-") as directory:
        temp = Path(directory)
        repo, home = temp / "repo", temp / "home"
        home.mkdir()
        if (case / "public.json").is_file():
            repo.mkdir()
            checkout(json.loads((case / "public.json").read_text()), repo, cache)
        else:
            shutil.copytree(case / "repo", repo)
            git("init", "-q", cwd=repo)
        for op, name, expected, _, subset in rows:
            if op != "symbol" or expected == "-":
                continue
            anchors = expected.split(",")
            if len(anchors) >= 2:
                mode = "ambiguous"
                result = ambiguous(binary, repo, home, name, anchors)
            else:
                mode = "unique"
                pos = location(repo, anchors[0], name)
                anchor = ":".join(map(str, pos[:3])) if pos else None
                if subset or anchor not in by_site:
                    records.append(dict(case=str(case.relative_to(FIXTURES)), name=name, mode="unpaired"))
                    continue
                expected_sites, status, partial = by_site[anchor]
                result = {"anchor": anchor, "expected": expected_sites, "diff": {}}
                expected_exit = 2 if status.partition("/")[0] in {"unindexed", "unsupported"} else int(expected_sites == "-")
                for entry, argv in (("named", [name]), ("positional", [pos[0], pos[1], str(pos[3])])):
                    answer = invoke(binary, repo, home, "--reference", *argv, "--json")
                    try:
                        result[entry] = compare(json.loads(answer.stdout), "references", expected_sites, status, partial)
                    except (ValueError, KeyError, TypeError) as exc:
                        result[entry] = {"error": str(exc), "stderr": answer.stderr.strip()}
                    if answer.returncode != expected_exit:
                        result[entry]["exit"] = [expected_exit, answer.returncode]
                result["diff"] = result["named"]
            records.append(dict(case=str(case.relative_to(FIXTURES)), name=name, mode=mode, **result))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--public", action="store_true")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-named-references.json")
    args = parser.parse_args()
    records, errors = [], []
    for file in sorted(FIXTURES.glob("*/*/checks.tsv")):
        case = file.parent
        if (case / "public.json").exists() and not args.public:
            continue
        try:
            run_case(case, args.binary.resolve(), ROOT / "target/conformance-cache", records)
        except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
            errors.append([str(case.relative_to(FIXTURES)), str(exc)])
    counts = Counter(r["mode"] for r in records)
    failed = Counter(r["mode"] for r in records if r.get("diff"))
    args.report.parent.mkdir(exist_ok=True, parents=True)
    args.report.write_text(json.dumps({"counts": counts, "failed": failed, "records": records, "errors": errors}, indent=2) + "\n")
    print(f"cases by mode: {dict(counts)}; failed: {dict(failed)}; setup errors: {len(errors)}; {args.report}")
    return int(bool(failed or errors))


if __name__ == "__main__":
    raise SystemExit(main())
