"""Read the source bytes paired with published whole-repository score labels."""

import json
from pathlib import Path
import tarfile
import zipfile


ZIP_IDS = {
    "aoi": 24262451,
    "argouml": 24262454,
    "diarymanagement": 24262457,
    "jsweet": 24262460,
    "junit4": 24262463,
}
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


def scored_repository(paths, repo):
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


def java_archive_member(names, name, row):
    suffix = name.lower().split("sourcefiles/", 1)[-1]
    matches = [member for member in names if member.lower() == suffix or member.lower().endswith("/" + suffix)]
    if not matches:
        suffix = row["packageandclass"].replace(".", "/").lower() + ".java"
        matches = [member for member in names if member.lower().endswith("/" + suffix)]
    if len(matches) != 1:
        raise ValueError(f"Cannot match {name}: {matches}")
    return matches[0]


def icsme_repository(paths, repo, labels):
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
            member = java_archive_member(names, name, row)
            yield (
                repo,
                repo,
                name,
                "Java",
                archive.read(member),
                float(row["Code_Health"]),
                None,
            )

