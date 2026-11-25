"""Pinned public inputs for the score conformance corpus. Never execute corpus code."""

import csv
import hashlib
import io
import json
from pathlib import Path
import tarfile
from concurrent.futures import ThreadPoolExecutor
import subprocess
import zipfile

FORGE = "codescene-research/ai-friendliness-forge-2026/c7e7afd437cfaecb282b9e7c366ed1dbae06ca2d"
SCAM = "codescene-research/test-generation-scam-2026/7baf08bc1b458047fd01fb06336fabc50c36ec20"
ICSME = "codescene-research/maintainability-benchmarking-icsme-2024/0fb79230f1d9353aa3f4de3620ffd61c49f8e050"
RW = "repowise-dev/repowise-bench/0fb3c03aab35fb67dc72540fd6a70e2c2c599878"
RAW = "https://raw.githubusercontent.com/"
REPOS = json.loads(Path(__file__).with_name("score_repositories.json").read_text())
ZIP_IDS = {
    "aoi": 24262451,
    "argouml": 24262454,
    "diarymanagement": 24262457,
    "jsweet": 24262460,
    "junit4": 24262463,
}
MODELS = "claude gemma glm gpt granite qwen3 sonnet".split()
EXTENSIONS = {
    ".proto": "Protobuf",
    ".sh": "Shell",
    ".json": "JSON",
    ".yaml": "YAML",
    ".yml": "YAML",
    ".md": "Markdown",
    ".sql": "SQL",
    ".tf": "Terraform",
    ".toml": "TOML",
    ".hcl": "HCL",
    "": "No extension",
    ".c": "C",
    ".php": "PHP",
    ".rs": "Rust",
    ".go": "Go",
    ".cs": "C#",
    ".js": "JavaScript",
    ".ts": "TypeScript",
    ".cjs": "JavaScript",
    ".kts": "Kotlin",
    ".kt": "Kotlin",
    ".java": "Java",
    ".py": "Python",
    ".cc": "C++",
    ".h": "C++",
}
TEST = {
    "rich",
    "axios",
    "zod",
    "bat",
    "fiber",
    "mockito",
    "coroutines",
    "spdlog",
    "npgsql",
}
DEV = {"cockroach", "fd", "chi", "litestar", "argouml", "jsweet"}


def collect(cache):
    urls = {}
    for lang in ["cpp", "java"]:
        urls[lang] = RAW + SCAM + "/data/" + lang + "_set.jsonl"
    urls["original"] = RAW + FORGE + "/dataset/python_corp_bleud.jsonl"
    for model in MODELS:
        urls[model] = RAW + FORGE + "/results/" + model + "_tested.jsonl"
    urls["icsme"] = RAW + ICSME + "/maintainability_data.csv"
    for name, ident in ZIP_IDS.items():
        urls[name] = f"https://ndownloader.figshare.com/files/{ident}"
    for name, meta in REPOS.items():
        urls[name] = (
            f"https://codeload.github.com/{meta['repository']}/tar.gz/{meta['revision']}"
        )
        urls[name + "-scores"] = (
            RAW + RW + f"/results/health_defect_{name}/codescene_scores.json"
        )

    def fetch(item):
        key, url = item
        path = cache / hashlib.sha256(url.encode()).hexdigest()
        if not path.exists():
            tmp = path.with_suffix(".part")
            subprocess.run(
                [
                    "curl",
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--location",
                    "--retry",
                    "3",
                    "--output",
                    str(tmp),
                    url,
                ],
                check=True,
            )
            tmp.replace(path)
        return key, path

    cache.mkdir(exist_ok=True, parents=True)
    with ThreadPoolExecutor(6) as pool:
        return dict(pool.map(fetch, urls.items()))


def rows(paths):
    def jsonlines(key):
        with paths[key].open() as f:
            for line in f:
                yield json.loads(line)

    for key, lang, ext in [
        ("original", "Python", "py"),
        ("java", "Java", "java"),
        ("cpp", "C++", "cpp"),
    ]:
        for i, r in enumerate(jsonlines(key)):
            yield (
                key,
                r["name"],
                f"{i}.{ext}",
                lang,
                r["task"].encode(),
                r.get("score", r.get("task-score")),
                r.get("smells"),
            )
    for model in MODELS:
        for i, r in enumerate(jsonlines(model)):
            score, source = r.get("ref-score"), r.get("ref-task")
            if (
                isinstance(score, (int, float))
                and 1 <= score <= 10
                and isinstance(source, str)
                and source.strip()
            ):
                yield (
                    model,
                    r["name"],
                    f"{i}.py",
                    "Python",
                    source.encode(),
                    score,
                    None,
                )
    for repo in REPOS:
        scores = json.loads(paths[repo + "-scores"].read_bytes())
        seen = set()
        with tarfile.open(paths[repo], mode="r|gz") as archive:
            for item in archive:
                name = item.name.partition("/")[2]
                if name in scores and item.isfile():
                    yield (
                        repo,
                        repo,
                        name,
                        EXTENSIONS[Path(name).suffix],
                        archive.extractfile(item).read(),
                        scores[name],
                        None,
                    )
                    seen.add(name)
        missing = [
            name
            for name, score in scores.items()
            if score is not None and name not in seen
        ]
        if missing:
            raise ValueError(f"{repo}: missing scored sources {missing[:5]}")
    with paths["icsme"].open(encoding="utf-8-sig") as f:
        labels = list(csv.DictReader(f, delimiter=";"))
    for repo in ZIP_IDS:
        with zipfile.ZipFile(paths[repo]) as archive:
            names = [
                s
                for s in archive.namelist()
                if s.endswith(".java") and not s.startswith("__MACOSX")
            ]
            for row in labels:
                if row["projectname"].lower() != repo:
                    continue
                name = row["path"].replace("\\", "/")
                suffix = name.lower().split("sourcefiles/", 1)[-1]
                matches = [
                    s
                    for s in names
                    if s.lower().endswith("/" + suffix) or s.lower() == suffix
                ]
                if not matches:
                    suffix = row["packageandclass"].replace(".", "/").lower() + ".java"
                    matches = [s for s in names if s.lower().endswith("/" + suffix)]
                if len(matches) != 1:
                    raise ValueError(f"Cannot match {name}: {matches}")
                yield (
                    repo,
                    repo,
                    name,
                    "Java",
                    archive.read(matches[0]),
                    float(row["Code_Health"]),
                    None,
                )
