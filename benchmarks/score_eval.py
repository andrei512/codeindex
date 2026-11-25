"""Build the public score corpus, inspect its coverage, or evaluate one frozen split."""

import argparse
from collections import Counter, defaultdict
import hashlib
import json
import os
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
        dropped = Counter()
        for origin, family, path, lang, data, score, findings in rows(paths):
            if score is not None and not 1 <= score <= 10:
                raise ValueError((origin, path, score))
            split = "test" if origin in TEST else "dev" if origin in DEV else "train"
            # Ignore line-ending and surrounding whitespace differences for identity only.
            key = digest(data.replace(b"\r\n", b"\n").strip())
            existing = db.execute(
                "SELECT split,score FROM cases WHERE hash=?", (key,)
            ).fetchone()
            if existing:
                if existing[0] != split:
                    # Keep this source out of evaluation if it occurs in another split.
                    db.execute("UPDATE cases SET split='overlap' WHERE hash=?", (key,))
                dropped[origin] += 1
                continue
            db.execute(
                "INSERT INTO cases VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    split,
                    origin,
                    family,
                    path,
                    lang,
                    data,
                    score,
                    None if findings is None else json.dumps(findings, sort_keys=True),
                    key,
                ),
            )
        # Fingerprint membership, provenance and labels separately from SQLite's page layout.
        h = hashlib.sha256()
        for row in db.execute(
            "SELECT split,origin,family,path,language,score,findings,hash FROM cases ORDER BY hash"
        ):
            h.update(json.dumps(row, ensure_ascii=True).encode() + b"\n")
        summary = dict(
            inputs=checksums,
            corpus_sha256=h.hexdigest(),
            coverage=coverage(db),
            duplicates=dict(dropped),
            cross_split_excluded=db.execute(
                "SELECT count(*) FROM cases WHERE split='overlap'"
            ).fetchone()[0],
        )
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


def run(folder, split, binary):
    binary = binary.resolve()
    db = sqlite3.connect(
        (folder / "cases.db").resolve().as_uri() + "?mode=ro", uri=True
    )
    stats = defaultdict(list)
    counts = defaultdict(Counter)
    rules = defaultdict(Counter)
    roles = defaultdict(list)
    bands = defaultdict(list)
    cursor = db.execute(
        "SELECT language,source,score,path,findings FROM cases WHERE split=? ORDER BY hash",
        (split,),
    )
    while batch := cursor.fetchmany(256):
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
                    [binary, "--score", "--json"],
                    cwd=repo,
                    env=dict(
                        os.environ,
                        HOME=str(home),
                        XDG_CONFIG_HOME=str(home / ".config"),
                    ),
                )
            )
            actual = {f["path"]: f for f in result["files"]}
            for (lang, _, expected, path, labels), name in zip(batch, names):
                observed = actual.get(name)
                score = observed["score"] if observed else None
                counts[lang]["cases"] += 1
                counts[lang]["reference_null_cx_null"] += int(
                    expected is None and score is None
                )
                counts[lang]["missing_score"] += int(
                    expected is not None and score is None
                )
                counts[lang]["reference_null_cx_scored"] += int(
                    expected is None and score is not None
                )
                if expected is not None and score is not None:
                    error = abs(expected - score)
                    stats[lang].append(error)
                    roles[path_role(path)].append(error)
                    band = (
                        "perfect"
                        if expected == 10
                        else "9..<10"
                        if expected >= 9
                        else "4..<9"
                        if expected >= 4
                        else "below 4"
                    )
                    bands[band].append(error)
                if labels is not None:
                    ref = Counter(
                        (
                            RULES.get(f["code-smell"], f["code-smell"]),
                            f["location"]["start-line"],
                        )
                        for f in json.loads(labels)
                    )
                    found = Counter(
                        (f["rule"], f["line"])
                        for f in (observed or {}).get("findings", [])
                    )
                    for label, values in [
                        ("matched", ref & found),
                        ("missed", ref - found),
                        ("extra", found - ref),
                    ]:
                        rules[lang][label] += sum(values.values())
    if not counts:
        raise ValueError("Empty split")
    by_lang = {}
    for lang, count in sorted(counts.items()):
        errors = stats[lang]
        by_lang[lang] = dict(
            count,
            compared=len(errors),
            mae=statistics.mean(errors) if errors else None,
            within_half=sum(e <= 0.5 for e in errors),
            findings=rules.get(lang),
        )
    means = [v["mae"] for v in by_lang.values() if v["mae"] is not None]
    return dict(
        split=split,
        corpus=json.loads((folder / "manifest.json").read_text())["corpus_sha256"],
        binary_sha256=digest(binary.read_bytes()),
        languages=by_lang,
        macro_language_mae=statistics.mean(means) if means else None,
        by_path_role={
            k: dict(n=len(v), mae=statistics.mean(v)) for k, v in roles.items()
        },
        by_reference_score={
            k: dict(n=len(v), mae=statistics.mean(v)) for k, v in bands.items()
        },
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
