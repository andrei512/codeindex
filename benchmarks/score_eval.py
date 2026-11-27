"""Build the public score corpus, inspect its coverage, or evaluate one frozen split."""

import argparse
from collections import Counter, defaultdict
from dataclasses import dataclass, field
import hashlib
import json
import os
from score_response import observed_findings, physical_lines, scored_paths
from pathlib import Path
import sqlite3
import statistics
import subprocess
import tempfile

from score import RULES
from score_sources import collect, rows, DEV, TEST

ROOT = Path(__file__).resolve().parents[1]
LOCK = Path(__file__).with_name("score_corpus.lock.json")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def coverage(db):
    return {
        s: {
            lang: dict(total=n, scored=k)
            for lang, n, k in db.execute(
                "SELECT language,count(*),count(score) FROM cases WHERE split=? GROUP BY language ORDER BY language",
                (s,),
            )
        }
        for s in ("train", "dev", "test")
    }


def register_case(db, row):
    origin, family, path, lang, data, score, findings = row
    if score is not None and not 1 <= score <= 10:
        raise ValueError((origin, path, score))
    split = "test" if origin in TEST else "dev" if origin in DEV else "train"
    # Ignore line-ending and surrounding whitespace differences for identity only.
    key = digest(data.replace(b"\r\n", b"\n").strip())
    existing = db.execute("SELECT split FROM cases WHERE hash=?", (key,)).fetchone()
    if existing:
        if existing[0] != split:
            # Keep this source out of evaluation if it occurs in another split.
            db.execute("UPDATE cases SET split='overlap' WHERE hash=?", (key,))
        return False
    labels = None if findings is None else json.dumps(findings, sort_keys=True)
    db.execute(
        "INSERT INTO cases VALUES (?,?,?,?,?,?,?,?,?)",
        (split, origin, family, path, lang, data, score, labels, key),
    )
    return True


def insert_corpus_rows(db, cases):
    dropped = Counter()
    for row in cases:
        if not register_case(db, row):
            dropped[row[0]] += 1
    return dropped


def corpus_manifest(db, checksums, dropped):
    # Fingerprint membership, provenance and labels separately from SQLite's page layout.
    h = hashlib.sha256()
    for row in db.execute(
        "SELECT split,origin,family,path,language,score,findings,hash FROM cases ORDER BY hash"
    ):
        h.update(json.dumps(row, ensure_ascii=True).encode() + b"\n")
    return dict(
        inputs=checksums,
        corpus_sha256=h.hexdigest(),
        coverage=coverage(db),
        duplicates=dict(dropped),
        cross_split_excluded=db.execute(
            "SELECT count(*) FROM cases WHERE split='overlap'"
        ).fetchone()[0],
    )


def prepare(folder):
    paths = collect(folder / "downloads")
    frozen = json.loads(LOCK.read_text()) if LOCK.exists() else None
    checksums = {k: digest(v.read_bytes()) for k, v in paths.items()}
    if frozen and frozen["inputs"] != checksums:
        raise ValueError("Input checksum differs from frozen corpus")
    with tempfile.TemporaryDirectory(dir=folder) as temp:
        staged = Path(temp) / "cases.db"
        db = sqlite3.connect(staged)
        db.execute(
            "CREATE TABLE cases (split TEXT, origin TEXT, family TEXT, path TEXT, language TEXT, source BLOB, score REAL, findings TEXT, hash TEXT PRIMARY KEY)"
        )
        dropped = insert_corpus_rows(db, rows(paths))
        summary = corpus_manifest(db, checksums, dropped)
        if frozen and summary != frozen:
            raise ValueError("Membership or labels differ from frozen corpus")
        db.commit()
        db.close()
        staged.replace(folder / "cases.db")
        (folder / "manifest.json").write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n"
        )
    return summary["coverage"]


def path_role(path):
    # An observable path convention, not an inference that all other code is production.
    parts = Path(path.lower()).parts
    stem = Path(parts[-1]).stem
    if any(p in {"test", "tests", "__tests__", "spec", "specs"} for p in parts[:-1]):
        return "test-path"
    if stem.startswith("test_") or stem.endswith(
        ("_test", ".test", ".spec", "tests", "testcase")
    ):
        return "test-path"
    return "other-path"


def score_batch(binary, batch):
    with tempfile.TemporaryDirectory() as tmp:
        repo = Path(tmp) / "src"
        subprocess.run(["git", "init", "-q", repo], check=True)
        names = []
        for i, (_, data, _, path, _) in enumerate(batch):
            rel = Path(path)
            if rel.is_absolute() or ".." in rel.parts:
                raise ValueError(f"Unsafe corpus path {path}")
            name = f"{i}/{rel.as_posix()}"
            dest = repo / name
            dest.parent.mkdir(parents=True)
            dest.write_bytes(data)
            names.append(name)
        home = Path(tmp) / "home"
        home.mkdir()
        result = json.loads(
            subprocess.check_output(
                [binary, "--score", "--ignore-test-rules", "--json"],
                cwd=repo,
                env=dict(
                    os.environ,
                    HOME=str(home),
                    XDG_CONFIG_HOME=str(home / ".config"),
                ),
            )
        )
        actual = scored_paths(result, names, allow_missing=True)
        return actual, names


def compare_findings(counts, labels, observed, source):
    ref = Counter(
        (
            RULES.get(f["code-smell"], f["code-smell"]),
            f["location"]["start-line"],
        )
        for f in json.loads(labels)
    )
    found = Counter(
        (f["rule"], f["line"])
        for f in observed_findings((observed or {}).get("findings", []), physical_lines(source))
    )
    for label, values in [
        ("matched", ref & found),
        ("missed", ref - found),
        ("extra", found - ref),
    ]:
        counts[label] += sum(values.values())


def reference_band(score):
    if score == 10:
        return "perfect"
    for cutoff, name in ((9, "9..<10"), (4, "4..<9")):
        if score >= cutoff:
            return name
    return "below 4"


@dataclass
class ScoreComparison:
    errors: dict = field(default_factory=lambda: defaultdict(list))
    counts: dict = field(default_factory=lambda: defaultdict(Counter))
    rules: dict = field(default_factory=lambda: defaultdict(Counter))
    roles: dict = field(default_factory=lambda: defaultdict(list))
    bands: dict = field(default_factory=lambda: defaultdict(list))

    def observe(self, case, observed):
        lang, source, expected, path, labels = case
        score = observed["score"] if observed else None
        count = self.counts[lang]
        count["cases"] += 1
        count["reference_null_cx_null"] += int(expected is None and score is None)
        count["missing_score"] += int(expected is not None and score is None)
        count["reference_null_cx_scored"] += int(expected is None and score is not None)
        if labels is not None:
            compare_findings(self.rules[lang], labels, observed, source)
        if expected is None or score is None:
            return
        error = abs(expected - score)
        self.errors[lang].append(error)
        self.roles[path_role(path)].append(error)
        self.bands[reference_band(expected)].append(error)

    def by_language(self):
        summary = {}
        for lang, count in sorted(self.counts.items()):
            errors = self.errors[lang]
            summary[lang] = dict(
                count, compared=len(errors), mae=statistics.mean(errors) if errors else None,
                within_half=sum(error <= 0.5 for error in errors), findings=self.rules.get(lang),
            )
        return summary


def errors_by_group(groups):
    return {key: dict(n=len(errors), mae=statistics.mean(errors)) for key, errors in groups.items()}


def run(folder, split, binary):
    binary = binary.resolve()
    db = sqlite3.connect(
        (folder / "cases.db").resolve().as_uri() + "?mode=ro", uri=True
    )
    cursor = db.execute(
        "SELECT language,source,score,path,findings FROM cases WHERE split=? ORDER BY hash",
        (split,),
    )
    comparison = ScoreComparison()
    while batch := cursor.fetchmany(256):
        actual, names = score_batch(binary, batch)
        for case, name in zip(batch, names):
            comparison.observe(case, actual.get(name))
    by_lang = comparison.by_language()
    if not by_lang:
        raise ValueError("Empty split")
    means = [row["mae"] for row in by_lang.values() if row["mae"] is not None]
    return dict(
        split=split,
        corpus=json.loads((folder / "manifest.json").read_text())["corpus_sha256"],
        binary_sha256=digest(binary.read_bytes()),
        languages=by_lang,
        macro_language_mae=statistics.mean(means) if means else None,
        by_path_role=errors_by_group(comparison.roles),
        by_reference_score=errors_by_group(comparison.bands),
    )


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("action", choices=["prepare", "coverage", "run"])
    p.add_argument("--split", choices=["train", "dev", "test"], default="dev")
    p.add_argument("--dir", type=Path, default=ROOT / "target/score-eval")
    p.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    a = p.parse_args()
    if a.action == "prepare":
        result = prepare(a.dir)
    elif a.action == "coverage":
        result = json.loads((a.dir / "manifest.json").read_text())["coverage"]
    else:
        result = run(a.dir, a.split, a.binary)
    print(json.dumps(result, indent=2))
