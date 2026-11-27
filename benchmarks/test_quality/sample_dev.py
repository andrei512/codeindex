"""Freeze a path-sampled development panel without running any detector."""
import concurrent.futures
import hashlib
from heapq import nsmallest
import json
import re
import subprocess
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
CACHE = HERE.parents[1] / "target/test-role-eval/blobs"
EXT = {
    "assembly": "asm s", "batchfile": "bat cmd", "c": "c", "cmake": "cmake",
    "cpp": "cpp cc cxx", "csharp": "cs", "css": "css", "dart": "dart",
    "dockerfile": "dockerfile", "go": "go", "hack": "hack php", "hcl": "hcl tf",
    "html": "html htm", "java": "java", "js": "js ts jsx tsx mjs cjs",
    "kotlin": "kt kts", "lua": "lua", "makefile": "mk", "mako": "mako",
    "markdown": "md", "nix": "nix", "objectivec": "m mm", "perl": "pl pm t",
    "php": "php", "plpgsql": "pgsql plpgsql", "powershell": "ps1",
    "procfile": "procfile", "proto": "proto", "python": "py", "ruby": "rb",
    "rust": "rs", "shell": "sh bash bats", "sql": "sql", "starlark": "bzl",
    "swift": "swift",
}


def languages_by_suffix():
    result = {}
    for language, suffixes in EXT.items():
        for suffix in suffixes.split():
            result.setdefault(suffix, set()).add(language)
    return result


SUFFIX_LANGUAGES = languages_by_suffix()
SPECIAL_NAMES = {"cmakelists.txt": "cmake", "makefile": "makefile"}


def eligible_languages(path, languages):
    path = path.lower()
    if set(path.split("/")) & {"vendor", "third_party", "node_modules"}:
        return
    if not re.search(r"test|spec|fixture|(^|/)t/", path):
        return
    name = path.rsplit("/", 1)[-1]
    suffix = name.rsplit(".", 1)[-1]
    matches = set(SUFFIX_LANGUAGES.get(suffix, ()))
    matches.add(SPECIAL_NAMES.get(name))
    matches.update(prefix for prefix in ("dockerfile", "procfile") if name.startswith(prefix))
    yield from (language for language in matches if language in languages)


def cache_blob(repo, rev, blob):
    path = blob["path"]
    url = f"https://raw.githubusercontent.com/{repo}/{rev}/{urllib.parse.quote(path)}"
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    oid = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
    if oid != blob["sha"]:
        raise ValueError(f"source mismatch: {repo}/{path}")
    digest = hashlib.sha256(data).hexdigest()
    (CACHE / digest).write_bytes(data)
    return digest


def regular_text_blobs(tree):
    for blob in tree:
        if blob["type"] != "blob" or blob["mode"] != "100644":
            continue
        if 0 < blob["size"] <= 131072:
            yield blob


def ranked_candidates(tree, repo, languages):
    for blob in regular_text_blobs(tree):
        matches = tuple(eligible_languages(blob["path"], languages))
        if matches:
            rank = hashlib.sha256(f"cx-quality-dev-1/{repo}/{blob['path']}".encode()).digest()
            yield matches, (rank, blob)


def candidate_pools(repo, rev, languages):
    tree = json.loads(subprocess.check_output(
        ["gh", "api", f"repos/{repo}/git/trees/{rev}?recursive=1"]))
    if tree["truncated"]:
        raise ValueError(f"incomplete tree for {repo}")
    candidates = {language: [] for language in sorted(languages)}
    for matches, candidate in ranked_candidates(tree["tree"], repo, candidates):
        for language in matches:
            candidates[language].append(candidate)
    return candidates


def freeze(group):
    (repo, rev), languages = group
    candidates = candidate_pools(repo, rev, languages)
    rows, digests = [], {}
    for lang, pool in candidates.items():
        for _, blob in nsmallest(6, pool, key=lambda item: item[0]):
            path = blob["path"]
            if path not in digests:
                digests[path] = cache_blob(repo, rev, blob)
            digest = digests[path]
            rows.append(dict(repository=repo, revision=rev, path=blob["path"], sha256=digest,
                             language=lang, split="dev"))
    return rows


def available_development_rows(paths):
    for path in paths:
        rows = map(json.loads, path.read_text().splitlines())
        yield from (row for row in rows if row["split"] == "dev")


def development_groups(rows, reserved):
    groups = {}
    for row in rows:
        if not any(label[1] in ("test", "test-support") for label in row["labels"]):
            continue
        if row["repository"].lower() in reserved:
            raise ValueError("sealed repository in development selection")
        groups.setdefault((row["repository"], row["revision"]), set()).add(row["language"])
    return groups


def main():
    CACHE.mkdir(parents=True, exist_ok=True)
    reserved = {row["repository"].lower() for row in json.loads((HERE / "blind.lock.json").read_text())}
    paths = [HERE.parent / "test_roles" / f"{name}.jsonl" for name in ("public", "assets")]
    groups = development_groups(available_development_rows(paths), reserved)
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        rows = [row for group in pool.map(freeze, sorted(groups.items())) for row in group]
    (HERE / "dev_sample.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))


if __name__ == "__main__":
    main()
