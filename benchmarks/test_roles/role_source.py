"""Pinned source identity and physical coordinates for development role panels."""
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import tempfile
from typing import NamedTuple
import urllib.parse
import urllib.request


NONSPACE = re.compile(rb"\S")
ROLES = ("production", "test", "test-support", "unknown")
ROLE_BYTES = {role: bytes([index]) for index, role in enumerate(ROLES)}


def checked_path(name):
    p = PurePosixPath(name)
    if p.is_absolute() or any(part in {"", ".", "..", ".git"} for part in name.split("/")):
        raise ValueError(f"unsafe fixture path: {name!r}")
    return p


class InputFile(NamedTuple):
    data: bytes

    @classmethod
    def read(cls, path):
        return cls(path.read_bytes())

    def json_lines(self, *, skip_empty=False):
        return [json.loads(line) for line in self.data.decode().splitlines() if line or not skip_empty]

    def digest(self):
        return hashlib.sha256(self.data).hexdigest()


def get_source(cache, r):
    return pinned_source(cache, r)[1]


class SourcePin(NamedTuple):
    repository: str
    revision: str
    path: str
    digest: str

    def valid(self):
        checks = (
            (r"[\w.-]+/[\w.-]+", self.repository),
            (r"[a-f0-9]{40}", self.revision),
            (r"[a-f0-9]{64}", self.digest),
        )
        return all(re.fullmatch(pattern, value) for pattern, value in checks)

    def verify(self, data):
        if hashlib.sha256(data).hexdigest() != self.digest:
            raise ValueError(f"source hash mismatch: {self.repository}/{self.path}")


def fetch_blob(blob, pin):
    data = fetch_bytes(pin.repository, pin.revision, pin.path)
    pin.verify(data)
    with tempfile.NamedTemporaryFile(dir=blob.parent, delete=False) as tmp:
        tmp.write(data)
    Path(tmp.name).replace(blob)
    return data


def pinned_source(cache, r):
    """Return the validated checkout path alongside the verified bytes."""
    pin = SourcePin(*(r[k] for k in ("repository", "revision", "path", "sha256")))
    relative = checked_path(pin.path)
    if not pin.valid():
        raise ValueError(f"invalid pin for {pin.repository}/{pin.path}")
    blob = cache / "blobs" / pin.digest
    try:
        data = blob.read_bytes()
    except FileNotFoundError:
        return relative, fetch_blob(blob, pin)
    pin.verify(data)
    return relative, data


def fetch_bytes(repo, revision, path):
    url = f"https://raw.githubusercontent.com/{repo}/{revision}/{urllib.parse.quote(path)}"
    with urllib.request.urlopen(url, timeout=60) as stream:
        return stream.read()


def byte_span(data, start, end):
    if type(start) is not int or type(end) is not int:
        return False
    if not 0 <= start < end <= len(data):
        return False
    # Neither endpoint may split an encoded scalar.
    return all(offset == len(data) or data[offset] & 0xc0 != 0x80 for offset in (start, end))


class LabelledSource:
    def __init__(self, data):
        self.data = data
        self.starts = [0, *(i + 1 for i, byte in enumerate(data) if byte == 10)]
        if self.starts[-1] != len(data):
            self.starts.append(len(data))

    def valid_region_bounds(self, region):
        starts, data = self.starts, self.data
        start, end = region["start_byte"], region["end_byte"]
        first, last = region["start_line"], region["end_line"]
        return (byte_span(data, start, end) and type(first) is int and type(last) is int
                and 1 <= first <= last < len(starts)
                and starts[first-1] <= start < starts[first] and starts[last-1] <= end <= starts[last])

    def mixed_roles(self, line, default, spans):
        """Distinct effective roles on non-whitespace bytes; catches default-role gaps too."""
        start, end = self.starts[line - 1:line + 1]
        # One cell per interval, irrespective of the length of the source line.
        bounds = sorted({start, end} | {n for region in spans for n in (region["start_byte"], region["end_byte"])
                                        if start < n < end})
        if len(bounds) == 2:
            return False
        index = {offset: i for i, offset in enumerate(bounds)}
        cells = bytearray(ROLE_BYTES[default]) * (len(bounds) - 1)
        for region in spans:
            first = index[max(start, region["start_byte"])]
            last = index[min(end, region["end_byte"])]
            cells[first:last] = ROLE_BYTES[region["role"]] * (last - first)
        visible = {role for role, left, right in zip(cells, bounds, bounds[1:])
                   if NONSPACE.search(self.data, left, right)}
        return len(visible) > 1


def reason_text(value):
    return isinstance(value, str) and bool(value.strip())


def label_rows(rows, location):
    for row in rows:
        labels = row["labels"]
        if not labels and row.get("context") is not True:
            raise ValueError(f"empty label list without context=true: {location}")
        if labels and row.get("context"):
            raise ValueError(f"context-only file also has labels: {location}")
        yield from labels


def labelled_file(entry, cache):
    (repo, revision, path), group = entry
    first = group[0]
    location = f"{repo}/{path}"
    source = LabelledSource(get_source(cache, first))
    line_count = len(source.starts) - 1
    labels = checked_labels(label_rows(group, location), location, line_count)
    return dict(language=first["language"], revision=revision, source=source, labels=labels)


def add_label(labels, item, location, line_count):
    line, role, evidence = item
    if not valid_label(line, line_count, role, evidence):
        raise ValueError(f"invalid label in {location} at {line}")
    original, _ = labels.setdefault(line, (role, evidence))
    if original != role:
        raise ValueError(f"conflicting labels: {location}:{line}")


def checked_labels(items, location, line_count):
    labels = {}
    for item in items:
        add_label(labels, item, location, line_count)
    return labels


def valid_label(line, line_count, role, evidence):
    return type(line) is int and 1 <= line <= line_count and role in ROLES and reason_text(evidence)


def pin_key(row):
    return row["repository"].lower(), row["revision"], row["path"]


def pinned_files(rows):
    groups = {}
    for row in rows:
        key = pin_key(row)
        group = groups.setdefault(key, [])
        if not group:
            checked_path(key[2])
        elif (group[0]["sha256"], group[0]["language"]) != (row["sha256"], row["language"]):
            raise ValueError(f"conflicting file rows: {key}")
        group.append(row)
    return groups
