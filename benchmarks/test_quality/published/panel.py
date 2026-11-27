"""Materialize pinned public detection labels under target; read only appendix data."""
import argparse
import collections
import csv
from dataclasses import dataclass
import hashlib
import io
import json
import re
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT = HERE.parents[2] / "target/test-quality/published"
AROMA = "aromalia"
ML = "pontillo"
AROMA_RULES = ("AssertionRoulette", "ConditionalTestLogic", "DuplicateAssert", "EmptyTest",
               "ExceptionHandling", "IgnoredTest", "MagicNumberTest", "RedundantPrint",
               "SleepyTest", "UnknownTest")
ML_RULES = ("EagerTest", "MysteryGuest", "ResourceOptimism", "TestRedundancy")
LANGS = {"java": "java", "python": "python", "csharp": "CSharp/CSharp.Tests/Tests",
         "javascript": "javascript", "typescript": "typescript"}


def read_table(cache, source, path):
    pin = source["files"][path]
    cached = cache / pin
    if cached.exists():
        data = cached.read_bytes()
    else:
        url = f"https://raw.githubusercontent.com/{source['repository']}/{source['revision']}/{path}"
        with urllib.request.urlopen(url, timeout=45) as response:
            data = response.read()
    if hashlib.sha256(data).hexdigest() != pin:
        raise ValueError(f"invalid source hash for {path}")
    if not cached.exists():
        cached.write_bytes(data)
    return list(csv.DictReader(io.StringIO(data.decode("utf-8-sig"))))


def binary(value):
    if value not in ("0", "1"):
        raise ValueError(f"invalid detection label {value!r}")
    return value == "1"


@dataclass(frozen=True)
class AromaOrigin:
    url: str
    repository: str
    language: str

    @classmethod
    def decode(cls, origin):
        parsed = urllib.parse.urlsplit(origin)
        parts = parsed.path.strip("/").split("/")
        github_blob = parsed.hostname == "github.com" and len(parts) >= 5
        if not github_blob or parts[2] != "blob":
            raise ValueError(f"unrecognized pinned origin: {origin}")
        language = {"java": "java", "py": "python"}[parts[-1].rsplit(".", 1)[-1]]
        return cls(origin, "/".join(parts[:2]), language)


def aroma_family(row):
    filename = row["filename"]
    match = re.fullmatch(r"(?:TestSmell|test_smell_)([0-9]{3})(?:\.test)?\.(?:java|py|cs|js|ts)", filename)
    if not match or set(row) != {"filename", *AROMA_RULES}:
        raise ValueError(f"unexpected AromaLIA schema or file: {filename}")
    return int(match[1])


def aroma_translations(data, language):
    families = set()
    for row in data:
        family = aroma_family(row)
        if family in families:
            raise ValueError(f"duplicate {language} family {family}")
        families.add(family)
        yield family, row
    if families != set(range(1, 167)):
        raise ValueError(f"missing translations in {language}")


def aroma_panel(cache, source):
    rows = []
    origin_rows = read_table(cache, source, "single-language-analysis/dataset-files/java/summary.csv")
    origins = {r["FileName"]: r["OriginalURL"] for r in origin_rows}
    if len(origins) != 166:
        raise ValueError("incomplete original-family map")
    for language, directory in LANGS.items():
        data = read_table(cache, source, f"single-language-analysis/dataset-sheets/ground-truth/{language}.csv")
        for family, item in aroma_translations(data, language):
            origin = AromaOrigin.decode(origins[f"TestSmell{family:03}.java"])
            rows.append(dict(origin_url=origin.url, original_repository=origin.repository,
                             translated=language != origin.language, dataset=AROMA, unit="file",
                             language=language, group=f"{AROMA}:{family}",
                             repository=source["repository"], revision=source["revision"],
                             path=f"single-language-analysis/dataset-files/{directory}/{item['filename']}",
                             labels={key: binary(item[key]) for key in AROMA_RULES}))
    return rows


def checked_projects(items):
    projects = {}
    for item in items:
        slug = item["url"].removeprefix("https://github.com/").strip("/")
        name = slug.split("/")[-1].lower()
        sha = item["sha"].strip()
        if name in projects or not re.fullmatch("[a-f0-9]{7,40}", sha):
            raise ValueError(f"ambiguous project ref: {item}")
        projects[name] = (slug, sha)
    return projects


def method_votes(items):
    groups = collections.defaultdict(list)
    for item in items:
        project = item["idProject"].split("_", 1)[1]
        # The appendix spells Hadoop "hdoop" in its combined label sheet.
        project = "hadoop" if project == "hdoop" else project
        groups[project, item["testCase"]].append(item)
    return groups


def agreed_labels(variants, conflicts):
    labels = {}
    for rule in ML_RULES:
        votes = {binary(row[rule]) for row in variants}
        if len(votes) == 1:
            labels[rule] = votes.pop()
        else:
            conflicts[rule] += 1
    return labels


def pontillo_panel(cache, source):
    projects = checked_projects(read_table(cache, source, "dataset/projects.csv"))
    items = read_table(cache, source, "dataset/entireDataset.csv")
    groups = method_votes(items)
    rows = []
    omitted = collections.Counter()
    conflicts = collections.Counter()
    for (project, method), variants in sorted(groups.items()):
        if project not in projects:
            omitted[project] += 1
            continue
        if labels := agreed_labels(variants, conflicts):
            slug, revision = projects[project]
            rows.append(dict(dataset=ML, unit="method", language="java", group=f"{ML}:{slug}",
                             repository=slug, revision=revision, method=method, labels=labels))
    return rows, dict(source_rows=len(items), distinct_method_keys=len(groups),
                      excluded_unpinned_methods=dict(omitted), omitted_conflicting_labels=dict(conflicts))


def check_dev_only(rows):
    lock = HERE.parent / "blind.lock.json"
    reserved = {r["repository"].casefold() for r in json.loads(lock.read_text())}
    for row in rows:
        if any(row[key].casefold() in reserved for key in ("repository", "original_repository") if key in row):
            raise ValueError("development source family overlaps the blind reserve")


def summary(rows):
    counts = collections.defaultdict(collections.Counter)
    for row in rows:
        for rule, present in row["labels"].items():
            counts[rule]["positive" if present else "negative"] += 1
    return dict(units=len(rows), groups=len({r["group"] for r in rows}),
                units_by_language=dict(sorted(collections.Counter(r["language"] for r in rows).items())),
                decisions=sum(sum(c.values()) for c in counts.values()),
                by_rule={k: dict(sorted(v.items())) for k, v in sorted(counts.items())})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=DEFAULT)
    args = parser.parse_args()
    cache = args.out / "blobs"
    cache.mkdir(parents=True, exist_ok=True)
    pins = json.loads((HERE / "sources.json").read_text())
    aroma = aroma_panel(cache, pins[AROMA])
    ml, audit = pontillo_panel(cache, pins[ML])
    rows = aroma + ml
    check_dev_only(rows)
    encoded = "".join(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n" for row in rows).encode()
    report = {AROMA: summary(aroma), ML: dict(**summary(ml), **audit),
              "normalized_sha256": hashlib.sha256(encoded).hexdigest()}
    expected = json.loads((HERE / "coverage.json").read_text())
    if report != expected:
        raise ValueError("published data coverage differs from the checked-in report")
    (args.out / "labels.jsonl").write_bytes(encoded)
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
