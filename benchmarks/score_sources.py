"""Pinned public inputs for the score conformance corpus. Never execute corpus code."""

import csv
import hashlib
import json
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor
import subprocess

from score_repository_sources import (
    DEV, TEST, ZIP_IDS, EXTENSIONS, icsme_repository, java_archive_member, scored_repository,
)

FORGE = "codescene-research/ai-friendliness-forge-2026/c7e7afd437cfaecb282b9e7c366ed1dbae06ca2d"
SCAM = "codescene-research/test-generation-scam-2026/7baf08bc1b458047fd01fb06336fabc50c36ec20"
ICSME = "codescene-research/maintainability-benchmarking-icsme-2024/0fb79230f1d9353aa3f4de3620ffd61c49f8e050"
RW = "repowise-dev/repowise-bench/0fb3c03aab35fb67dc72540fd6a70e2c2c599878"
RAW = "https://raw.githubusercontent.com/"
REPOS = json.loads(Path(__file__).with_name("score_repositories.json").read_text())
MODELS = "claude gemma glm gpt granite qwen3 sonnet".split()


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


def json_rows(path):
    with path.open() as stream:
        yield from map(json.loads, stream)


def original_rows(path, origin, language, extension):
    for index, row in enumerate(json_rows(path)):
        yield (
            origin, row["name"], f"{index}.{extension}", language, row["task"].encode(),
            row.get("score", row.get("task-score")), row.get("smells"),
        )


def model_rows(path, model):
    for index, row in enumerate(json_rows(path)):
        score, source = row.get("ref-score"), row.get("ref-task")
        if not isinstance(score, (int, float)) or not 1 <= score <= 10:
            continue
        if isinstance(source, str) and source.strip():
            yield model, row["name"], f"{index}.py", "Python", source.encode(), score, None


def rows(paths):
    for key, lang, ext in [
        ("original", "Python", "py"),
        ("java", "Java", "java"),
        ("cpp", "C++", "cpp"),
    ]:
        yield from original_rows(paths[key], key, lang, ext)
    for model in MODELS:
        yield from model_rows(paths[model], model)
    for repo in REPOS:
        yield from scored_repository(paths, repo)
    with paths["icsme"].open(encoding="utf-8-sig") as f:
        labels = list(csv.DictReader(f, delimiter=";"))
    for repo in ZIP_IDS:
        yield from icsme_repository(paths, repo, labels)
