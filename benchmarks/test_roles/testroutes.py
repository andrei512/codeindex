"""Join TestRoutes MSR 2020 labels to exact Java declarations in pinned revisions.

Paper: https://doi.org/10.1145/3379597.3387488 (section 3.1).
Dataset: https://doi.org/10.5281/zenodo.3741674. No role is inferred from a filename.
"""
import collections
import csv
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


def locate(row, files, read):
    owner, _, name = row["method_qualified_name"].split("(")[0].rpartition(".")
    # A '$' may be part of a top-level class name (e.g. $Gson$Preconditions).
    # Try the literal class first and then successively shorter enclosing types.
    qualified = owner.replace(".", "/")
    while qualified:
        matches = [p for p in files if p.endswith("/" + qualified + ".java") or p == qualified + ".java"]
        if matches:
            break
        qualified, dollar, _ = qualified.rpartition("$")
        if not dollar:
            break
    if len(matches) != 1:
        return None, "missing or ambiguous source"
    path, = matches
    try:
        lines = read(path).decode().splitlines()
    except UnicodeDecodeError:
        return None, "non-UTF-8 source"
    start, end = int(row["method_start_line"]), int(row["method_end_line"])
    if not (1 <= start <= end <= len(lines)):
        return None, "invalid source range"
    if name == "<init>":
        name = row["method_header"].split("(")[0].split()[-1]
    # Metadata starts at the first annotation or modifier in some files.
    # Require the actual declaration before the method body. Never relocate
    # a mismatching annotation to another overload just because names match.
    def ignore_comment(match):
        # Keep offsets and line numbers stable while masking comments.
        return "".join("\n" if c == "\n" else " " for c in match[0])
    header = "\n".join(lines[start - 1:end])
    header = re.sub(r"//[^\n]*|/\*.*?\*/", ignore_comment, header, flags=re.S).split("{", 1)[0]
    expected = row["method_header"].split("(", 1)[1].rsplit(")", 1)[0]
    def parameter_tokens(params):
        return re.findall(r"[\w$]+|[^\w\s]", re.sub(r"\bfinal\b", "", params))
    for found in re.finditer(r"(?<![\w$.])" + re.escape(name) + r"\s*\(([^()]*)\)", header):
        if parameter_tokens(found[1]) != parameter_tokens(expected):
            continue
        prefix = header[:found.start()].rstrip()
        if re.search(r"\b(return|throw|new|if|while|switch)\b|[.;=]\s*$", prefix):
            continue
        # Constructors may lack a return type, but ordinary declarations need one.
        # The public coordinates sometimes point to a call to the same overload.
        if not prefix and row["method_qualified_name"].split("(")[0].split(".")[-1] != "<init>":
            continue
        return (path, start + header.count("\n", 0, found.start())), None
    return None, "declaration disagrees with pinned source"



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
        for partition, project, repo, revision in PROJECTS:
            if partition != split:
                continue
            directory, files = source_paths(cache, repo, revision)
            roles = collections.defaultdict(set)
            # The misspelling is present in the deposited ZIP.
            stem = "JFrreChart" if project == "JFreeChart" else project
            for link in csv_rows(archive, stem + "_routes.csv", ";"):
                roles[link["test_case_id"]].add("test")
                roles[link["method_id"]].add(ROLE[link["method_role"]])
            blobs, points = {}, collections.defaultdict(list)

            def read(path):
                if path not in blobs:
                    blobs[path] = git(directory, "show", revision + ":" + path)
                return blobs[path]

            for row in csv_rows(archive, project + "_data.csv", ","):
                ident = row["method_id"]
                labels = roles.pop(ident, set())
                if len(labels) != 1:
                    omitted.append([repo, ident, "no route" if not labels else "conflicting roles"])
                    continue
                location, why = locate(row, files, read)
                if why:
                    omitted.append([repo, ident, why])
                    continue
                points[location].append((labels.pop(), ident))
            for ident in roles:
                omitted.append([repo, ident, "missing method metadata"])
            by_file = collections.defaultdict(list)
            for (path, line), matches in sorted(points.items()):
                if len({role for role, _ in matches}) != 1:
                    omitted.append([repo, path + ":" + str(line), "conflicting roles on source line"])
                    continue
                role = matches[0][0]
                ids = ", ".join(ident for _, ident in matches)
                by_file[path].append([line, role, "TestRoutes " + ids])
            for path, labels in by_file.items():
                data = blobs[path]
                digest = hashlib.sha256(data).hexdigest()
                blob = cache / "blobs" / digest
                if not blob.exists():
                    blob.write_bytes(data)
                rows.append(dict(repository=repo, revision=revision, path=path, sha256=digest,
                                 language="java", split=split, labels=labels))
    return rows, omitted
