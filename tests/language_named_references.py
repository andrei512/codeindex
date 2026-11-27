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
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

from language_fixture_positions import SourceLines, utf16_prefix
from language_conformance import FIXTURES, ROOT, check_response, prepare_fixture


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


@dataclass(frozen=True)
class Probe:
    binary: Path
    repo: Path
    home: Path

    def invoke(self, *args):
        return subprocess.run(
            [self.binary, *args], cwd=self.repo, text=True, capture_output=True, timeout=120,
            env={**os.environ, "HOME": str(self.home), "GIT_CONFIG_GLOBAL": "./.missing-config",
                 "GIT_CONFIG_NOSYSTEM": "1"},
        )


def location(repo, anchor, name):
    parts = anchor.split("|", 1)[0].split(":")
    path, line = parts[:2]
    text = SourceLines((repo / path).read_bytes()).at(int(line))
    if len(parts) == 3:
        column = int(parts[2])
    elif text.count(name) == 1:
        column = len(text[:text.index(name)].encode("utf-16-le")) // 2 + 1
    else:
        return None
    prefix = utf16_prefix(text, column)
    return path, line, column, len(prefix.encode("utf-8")) + 1


def ambiguous(probe, name, anchors):
    attempt = probe.invoke("--reference", name)
    result = {"exit": attempt.returncode, "diagnostic": attempt.stderr.strip(), "diff": {}}
    established = (attempt.returncode == 2, "ambiguous" in attempt.stderr.lower(), not attempt.stdout)
    if not all(established):
        result["diff"]["ambiguity"] = "expected exit 2, ambiguity on stderr and empty stdout"
        return result
    # Replay only the supported advice instead of evaluating arbitrary diagnostics.
    match = re.search(r"\bcx --symbol " + re.escape(name) + r"(?=\s|$)((?:\s+--(?:column|json))*)", attempt.stderr)
    if not match:
        result["diff"]["advice"] = "no symbol lookup command for the ambiguous name"
        return result
    flags = match.group(1).split()
    replay = probe.invoke("--symbol", name, *flags)
    if replay.returncode:
        result["diff"]["advice"] = f"recommended symbol lookup exited {replay.returncode}"
        return result
    actual = symbol_locations(replay.stdout, flags)
    if any(not contains_anchor(actual, probe.repo, anchor, name) for anchor in anchors):
        result["diff"]["advice"] = "recommended command omits a verified candidate or its byte column"
    return result


def symbol_locations(output, flags):
    if "--json" in flags:
        return {(hit["path"], str(hit["line"]), hit["byte_column"]) for hit in json.loads(output)["hits"]}
    return {(m[1], m[2], int(m[3])) for line in output.splitlines()
            if (m := re.match(r"^([^:]+):(\d+):(\d+):", line))}


def contains_anchor(actual, repo, anchor, name):
    pos = location(repo, anchor, name)
    if pos is None:
        # A line-only expectation still establishes the need for a choice.
        path, line = anchor.split("|", 1)[0].split(":")
        return any(x[:2] == (path, line) for x in actual)
    return (pos[0], pos[1], pos[3]) in actual


def unique(probe, subject, by_site):
    name, (anchor,), subset = subject
    pos = location(probe.repo, anchor, name)
    anchor = ":".join(map(str, pos[:3])) if pos else None
    if subset or anchor not in by_site:
        return {"mode": "unpaired"}
    expected_sites, status, partial = by_site[anchor]
    result = {"mode": "unique", "anchor": anchor, "expected": expected_sites, "diff": {}}
    expected_exit = 2 if status.partition("/")[0] in {"unindexed", "unsupported"} else int(expected_sites == "-")
    query = dict(op="references", subject=name, expected=expected_sites, status=status, subset=partial)
    for entry, argv in (("named", [name]), ("positional", [pos[0], pos[1], str(pos[3])])):
        answer = probe.invoke("--reference", *argv, "--json")
        try:
            result[entry] = check_response(answer, query, "fresh")
        except (ValueError, KeyError, TypeError) as exc:
            result[entry] = {"error": str(exc), "stderr": answer.stderr.strip()}
        if answer.returncode != expected_exit:
            result[entry]["exit"] = [expected_exit, answer.returncode]
    result["diff"] = {arm: result[arm] for arm in ("named", "positional") if result[arm]}
    return result


def checked_subjects(rows):
    for op, name, expected, _, subset in rows:
        if op == "symbol" and expected != "-":
            yield name, expected.split(","), subset


def inspect_subject(probe, subject, by_site):
    name, anchors, _ = subject
    if len(anchors) >= 2:
        result = ambiguous(probe, name, anchors)
        result["mode"] = "ambiguous"
        return result
    return unique(probe, subject, by_site)


def run_case(case, binary, cache, records):
    rows = list(fixture_rows(case))
    case_name = str(case.relative_to(FIXTURES))
    by_site = {subject: (expected, status, subset) for op, subject, expected, status, subset in rows
               if op == "references"}
    for anchor, sites in SUPPLEMENTAL_REFERENCES.get(case_name, {}).items():
        by_site.setdefault(anchor, (sites, "resolved", False))
    subjects = list(checked_subjects(rows))
    if not subjects:
        return
    with tempfile.TemporaryDirectory(prefix="cx-named-") as directory:
        temp = Path(directory)
        probe = Probe(binary, temp / "repo", temp / "home")
        probe.home.mkdir()
        prepare_fixture(case, probe.repo, cache)
        for subject in subjects:
            records.append(dict(case=case_name, name=subject[0], **inspect_subject(probe, subject, by_site)))


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
