"""Join TestRoutes MSR 2020 labels to exact Java declarations in pinned revisions.

Paper: https://doi.org/10.1145/3379597.3387488 (section 3.1).
Dataset: https://doi.org/10.5281/zenodo.3741674. No role is inferred from a filename.
"""
import collections
import csv
from functools import lru_cache
import hashlib
import io
import re
import subprocess
import urllib.request
import zipfile

URL = "https://zenodo.org/api/records/3741674/files/testroutes/manual-dataset-msr.zip/content"
DIGEST = "7d077baa75cfcd2deb1ef20130fd10ce90ef463ca2a136cc8f447190607ee30f"
PROJECTS = [
    ("dev", "CommonsLang", "apache/commons-lang", "4777c3a5e4291af2420db57d008152c70c4a8f24"),
    ("dev", "Gson", "google/gson", "e9ead40302b3466f557a82f924907de59462deb9"),
    ("test", "JFreeChart", "jfree/jfreechart", "b3f5f21ba0fe32a8f7eccb6760a79df30628be3e"),
    ("test", "Joda-Time", "JodaOrg/joda-time", "d7d1620197aa8dfd0e9462bcec1523156779a3b9"),
]
ROLE = {"focal": "production", "contextual": "production", "helper": "test-support"}


def git(directory, *args):
    return subprocess.check_output(["git", "-C", directory, *args])


def source_paths(cache, repo, revision):
    directory = cache / "testroutes-sources" / revision
    directory.mkdir(parents=True, exist_ok=True)
    if not (directory / "HEAD").exists():
        git(directory, "init", "--bare", "-q")
    if subprocess.run(["git", "-C", directory, "cat-file", "-e", revision + "^{commit}"],
                      capture_output=True).returncode:
        git(directory, "fetch", "--quiet", "--no-tags", "--depth=1",
            f"https://github.com/{repo}.git", revision)
    files = git(directory, "ls-tree", "-rz", "--name-only", revision).decode().split("\0")
    return directory, [f for f in files if f.endswith(".java")]


def csv_rows(archive, name, delimiter):
    file, = [f for f in archive.namelist() if f.endswith("/" + name)]
    return csv.DictReader(io.StringIO(archive.read(file).decode()), delimiter=delimiter)


class OwnerPaths:
    def __init__(self, files):
        self.by_filename = collections.defaultdict(list)
        for path in files:
            self.by_filename[path.rsplit("/", 1)[-1]].append(path)

    def resolve(self, owner):
        # A '$' can be part of a top-level class name. Only fall back when
        # the literal enclosing name has no matches, never when it is ambiguous.
        qualified = owner.replace(".", "/")
        while qualified:
            suffix = qualified + ".java"
            candidates = self.by_filename.get(suffix.rsplit("/", 1)[-1], ())
            matches = [p for p in candidates if p == suffix or p.endswith("/" + suffix)]
            if matches:
                return matches[0] if len(matches) == 1 else None
            qualified = qualified.rpartition("$")[0]
        return None


def locate(row, source_for_owner, source_lines):
    owner, _, name = row["method_qualified_name"].split("(")[0].rpartition(".")
    constructor = name == "<init>"
    path = source_for_owner(owner)
    if path is None:
        return None, "missing or ambiguous source"
    lines = source_lines(path)
    if lines is None:
        return None, "non-UTF-8 source"
    start, end = int(row["method_start_line"]), int(row["method_end_line"])
    if not (1 <= start <= end <= len(lines)):
        return None, "invalid source range"
    if constructor:
        name = row["method_header"].split("(")[0].split()[-1]
    # Metadata can point at an annotation. Stay inside the pin's written range.
    header = declaration_header(lines[start - 1:end])
    expected = row["method_header"].split("(", 1)[1].rsplit(")", 1)[0]
    match = declaration_match(header, name, expected, constructor)
    if match is not None:
        return (path, start + header.count("\n", 0, match)), None
    return None, "declaration disagrees with pinned source"


def declaration_header(lines):
    def ignore_comment(match):
        # Keep offsets and line numbers stable while masking comments.
        return "".join("\n" if c == "\n" else " " for c in match[0])

    return re.sub(r"//[^\n]*|/\*.*?\*/", ignore_comment, "\n".join(lines), flags=re.S).split("{", 1)[0]


def parameter_tokens(params):
    return re.findall(r"[\w$]+|[^\w\s]", re.sub(r"\bfinal\b", "", params))


def declaration_match(header, name, expected, constructor):
    pattern = r"(?<![\w$.])" + re.escape(name) + r"\s*\(([^()]*)\)"
    expected_tokens = parameter_tokens(expected)
    matches = (found for found in re.finditer(pattern, header)
               if parameter_tokens(found[1]) == expected_tokens)
    return next((found.start() for found in matches
                 if declaration_prefix(header[:found.start()].rstrip(), constructor)), None)


def declaration_prefix(prefix, constructor):
    if re.search(r"\b(return|throw|new|if|while|switch)\b|[.;=]\s*$", prefix):
        return False
    # Constructors can lack a written return type.
    return bool(prefix or constructor)


def cases(cache, split):
    file = cache / "testroutes.zip"
    if not file.exists():
        with urllib.request.urlopen(URL, timeout=60) as stream:
            data = stream.read()
        if hashlib.sha256(data).hexdigest() != DIGEST:
            raise ValueError("TestRoutes download checksum mismatch")
        file.write_bytes(data)
    if hashlib.sha256(file.read_bytes()).hexdigest() != DIGEST:
        raise ValueError("cached TestRoutes checksum mismatch")
    rows, omitted = [], []
    with zipfile.ZipFile(file) as archive:
        for project in filter(lambda project: project[0] == split, PROJECTS):
            repo_rows, repo_omitted = project_cases(archive, cache, project)
            rows.extend(repo_rows)
            omitted.extend(repo_omitted)
    return rows, omitted


class ProjectSources:
    def __init__(self, cache, repo, revision):
        self.revision = revision
        self.directory, files = source_paths(cache, repo, revision)
        self.source_for_owner = lru_cache(maxsize=None)(OwnerPaths(files).resolve)
        self.lines = {}
        self.blobs = {}

    def source_lines(self, path):
        if path not in self.lines:
            data = git(self.directory, "show", self.revision + ":" + path)
            self.blobs[path] = data
            self.lines[path] = decoded_lines(data)
        return self.lines[path]


def decoded_lines(data):
    try:
        return data.decode().splitlines()
    except UnicodeDecodeError:
        return None


def method_roles(archive, project):
    roles = collections.defaultdict(set)
    # The misspelling is present in the deposited ZIP.
    stem = "JFrreChart" if project == "JFreeChart" else project
    for link in csv_rows(archive, stem + "_routes.csv", ";"):
        roles[link["test_case_id"]].add("test")
        roles[link["method_id"]].add(ROLE[link["method_role"]])
    return roles


def method_points(rows, sources, roles, omissions):
    omitted, repo = omissions
    points = collections.defaultdict(list)
    for row in rows:
        ident = row["method_id"]
        labels = roles.pop(ident, set())
        location, why = routed_location(row, sources, labels)
        if why:
            omitted.append([repo, ident, why])
            continue
        points[location].append((labels.pop(), ident))
    for ident in roles:
        omitted.append([repo, ident, "missing method metadata"])
    return points


def routed_location(row, sources, labels):
    if not labels:
        return None, "no route"
    if len(labels) != 1:
        return None, "conflicting roles"
    return locate(row, sources.source_for_owner, sources.source_lines)


def labels_by_file(points, omitted, repo):
    by_file = collections.defaultdict(list)
    for (path, line), matches in sorted(points.items()):
        if len({role for role, _ in matches}) != 1:
            omitted.append([repo, path + ":" + str(line), "conflicting roles on source line"])
            continue
        role = matches[0][0]
        ids = ", ".join(ident for _, ident in matches)
        by_file[path].append([line, role, "TestRoutes " + ids])
    return by_file


def pinned_rows(cache, sources, by_file, entry):
    split, _, repo, revision = entry
    for path, labels in by_file.items():
        data = sources.blobs[path]
        digest = hashlib.sha256(data).hexdigest()
        blob = cache / "blobs" / digest
        if not blob.exists():
            blob.write_bytes(data)
        yield dict(repository=repo, revision=revision, path=path, sha256=digest,
                   language="java", split=split, labels=labels)


def project_cases(archive, cache, entry):
    _, project, repo, revision = entry
    sources = ProjectSources(cache, repo, revision)
    roles = method_roles(archive, project)
    omitted = []
    metadata = csv_rows(archive, project + "_data.csv", ",")
    points = method_points(metadata, sources, roles, (omitted, repo))
    by_file = labels_by_file(points, omitted, repo)
    return list(pinned_rows(cache, sources, by_file, entry)), omitted
