"""Replay the 3SX published target-function measurements and CodeScene's gc.cpp fixture."""

from collections import Counter
from pathlib import Path
import json
import os
import re
import subprocess
import tempfile
from urllib.request import urlopen
from score import RULES
from score_response import finding_line, observed_findings, physical_lines, scored_paths

CAMPAIGN = "codescene-research/3sx"
PIN = "fea568087f13ddb01ac6c53b5945265ed5ef7eeb"
EDITOR = "codescene-oss/codescene-vscode"
EDITOR_PIN = "281bfd88337a95857dce242726df4c835298db1f"


def download(url):
    with urlopen(url, timeout=90) as res:
        return res.read()


def campaign_labels(doc, report, source):
    lines = source.split("\n")
    count = physical_lines(source)
    labels = []
    for name, line, *values in re.findall(
        r"^\| \d+ \| `([^`]+)` \| (\d+) \| ([\d-]+) \| ([\d-]+) \| ([\d-]+) \|",
        report, re.M,
    ):
        line = int(line)
        if not 1 <= line <= count or not re.search(r"\b" + re.escape(name) + r"\b", lines[line - 1]):
            raise ValueError(f"Unverified anchor {doc}: {line} {name}")
        labels.extend(
            dict(unit=name, line=line, rule=rule, value=int(value))
            for rule, value in zip(("branch complexity", "nesting depth", "nested regions"), values)
            if value != "-"
        )
    if not labels:
        raise ValueError(f"Missing target labels: {doc}")
    return labels


def campaign_cases():
    raw = f"https://raw.githubusercontent.com/{CAMPAIGN}/{PIN}/"
    tree = json.loads(download(f"https://api.github.com/repos/{CAMPAIGN}/git/trees/{PIN}?recursive=1"))["tree"]
    for doc in sorted(x["path"] for x in tree if re.fullmatch(r"docs/refactoring/tasks/R\d+-[^/]+\.md", x["path"])):
        report = download(raw + doc).decode()
        path = re.search(r"^# .*?`([^`]+\.(?:c|cpp))`", report).group(1)
        source = download(raw + path).decode()
        yield dict(path=path, language="C" if path.endswith(".c") else "C++",
                   repository=CAMPAIGN, revision=PIN, report=doc, source=source,
                   findings=campaign_labels(doc, report, source))


def editor_case():
    base = f"https://raw.githubusercontent.com/{EDITOR}/{EDITOR_PIN}/src/test/fixtures/"
    reference = json.loads(download(base + "gc.cpp.expected.json"))
    findings = []
    for f in reference["function-level-code-smells"]:
        for smell in f["code-smells"]:
            findings.append(
                dict(
                    unit=f["function"],
                    line=smell["highlight-range"]["start-line"],
                    rule=RULES.get(smell["category"], smell["category"]),
                    details=smell["details"],
                )
            )
    return dict(
        path="gc.cpp",
        language="C++",
        repository=EDITOR,
        revision=EDITOR_PIN,
        report="src/test/fixtures/gc.cpp.expected.json",
        source=download(base + "gc.cpp").decode(),
        findings=findings,
        file_findings=reference["file-level-code-smells"],
    )


def prepare(folder):
    cases = [*campaign_cases(), editor_case()]
    (folder / "published.json").write_text(json.dumps(cases) + "\n")


def finding_counts(row, lines):
    actual, functions = Counter(), Counter()
    for finding in observed_findings(row["findings"], lines):
        site = finding["rule"], finding["line"]
        actual[site] += 1
        if finding["unit"] != "<file>":
            functions[site] += 1
    return actual, functions


def score_case(case, binary):
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "src"
        subprocess.run(["git", "init", "-q", root], check=True)
        path = root / case["path"]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(case["source"])
        answer = subprocess.check_output(
            [binary.resolve(), "--score", "--ignore-test-rules", "--json", case["path"]],
            cwd=root,
            env=dict(os.environ, HOME=td, XDG_CONFIG_HOME=td + "/.config"),
        )
    return scored_paths(json.loads(answer), [case["path"]])[case["path"]]


def evaluate(folder, binary):
    cases = json.loads((folder / "published.json").read_text())
    totals = {}
    for c in cases:
        row = score_case(c, binary)
        lines = physical_lines(c["source"])
        actual, function_hits = finding_counts(row, lines)
        labels = Counter((f["rule"], finding_line(f, lines)) for f in c["findings"])
        stats = totals.setdefault(c["language"], Counter())
        stats["files"] += 1
        stats["labeled"] += sum(labels.values())
        stats["matched_rule_and_line"] += sum((labels & actual).values())
        if "file_findings" in c:
            stats["matched_in_function"] += sum((function_hits & labels).values())
            stats["unmatched_function_findings"] += sum(
                (function_hits - labels).values()
            )
            stats["unmatched_file_categories"] += len(c["file_findings"])
    return totals


if __name__ == "__main__":
    import argparse

    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("action", choices=["prepare", "run"])
    p.add_argument("--dir", type=Path, default=Path("target/finding-eval"))
    p.add_argument("--binary", type=Path, default=Path("target/release/cx"))
    a = p.parse_args()
    a.dir.mkdir(parents=True, exist_ok=True)
    if a.action == "prepare":
        prepare(a.dir)
    else:
        print(json.dumps(evaluate(a.dir, a.binary), indent=2))
