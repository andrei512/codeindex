"""Freeze a path-sampled development panel without running any detector."""
import concurrent.futures
import hashlib
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


def eligible(path, language):
    path = path.lower()
    if set(path.split("/")) & {"vendor", "third_party", "node_modules"}:
        return False
    if not re.search(r"test|spec|fixture|(^|/)t/", path):
        return False
    name = path.rsplit("/", 1)[-1]
    return (name.rsplit(".", 1)[-1] in EXT[language].split()
            or name == {"cmake": "cmakelists.txt", "makefile": "makefile"}.get(language)
            or language in {"dockerfile", "procfile"} and name.startswith(language))


def freeze(group):
    (repo, rev), languages = group
    tree = json.loads(subprocess.check_output(
        ["gh", "api", f"repos/{repo}/git/trees/{rev}?recursive=1"]))
    if tree["truncated"]:
        raise ValueError(f"incomplete tree for {repo}")
    blobs = [b for b in tree["tree"] if b["type"] == "blob" and b["mode"] == "100644"
             and 0 < b["size"] <= 131072]
    rows = []
    for lang in sorted(languages):
        def order(b):
            return hashlib.sha256(f"cx-quality-dev-1/{repo}/{b['path']}".encode()).digest()
        for b in sorted((b for b in blobs if eligible(b["path"], lang)), key=order)[:6]:
            url = f"https://raw.githubusercontent.com/{repo}/{rev}/{urllib.parse.quote(b['path'])}"
            with urllib.request.urlopen(url, timeout=60) as response:
                data = response.read()
            oid = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
            if oid != b["sha"]:
                raise ValueError(f"source mismatch: {repo}/{b['path']}")
            digest = hashlib.sha256(data).hexdigest()
            (CACHE / digest).write_bytes(data)
            rows.append(dict(repository=repo, revision=rev, path=b["path"], sha256=digest,
                             language=lang, split="dev"))
    return rows


if __name__ == "__main__":
    CACHE.mkdir(parents=True, exist_ok=True)
    blind = {r["repository"].lower() for r in json.loads((HERE / "blind.lock.json").read_text())}
    groups = {}
    for name in ("public", "assets"):
        for line in (HERE.parent / "test_roles" / f"{name}.jsonl").read_text().splitlines():
            row = json.loads(line)
            if row["split"] != "dev" or not any(l[1] in ("test", "test-support") for l in row["labels"]):
                continue
            if row["repository"].lower() in blind:
                raise ValueError("sealed repository in development selection")
            groups.setdefault((row["repository"], row["revision"]), set()).add(row["language"])
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        rows = [row for group in pool.map(freeze, sorted(groups.items())) for row in group]
    (HERE / "dev_sample.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
