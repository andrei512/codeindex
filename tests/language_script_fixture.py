"""Materialize annotated script sources and record one version's query verdicts."""
from collections.abc import Callable
from dataclasses import dataclass
import re

from language_conformance import git, expectation, query_difference
from language_fixture_positions import advance


def marked_line(text, path, line, sites):
    pieces = []
    end = 0
    col = 1
    for marker in re.finditer("«([a-z]+)»", text):
        segment = text[end:marker.start()]
        pieces.append(segment)
        _, col = advance(line, col, segment)
        if marker[1] in sites:
            raise ValueError(f"duplicate fixture label {marker[1]} in {path}")
        sites[marker[1]] = f"{path}:{line}:{col}"
        end = marker.end()
    pieces.append(text[end:])
    clean = "".join(pieces)
    if "«" in clean or "»" in clean:
        raise ValueError(f"malformed fixture label in {path}:{line}")
    return clean


def write_source(repo, path, source):
    sites = {}
    output = [marked_line(text, path, line, sites) for line, text in enumerate(source.splitlines(), 1)]
    (repo / path).write_text("\n".join(output) + "\n", encoding="utf-8")
    return sites


def checkout(root):
    repo, home = root / "repo", root / "home"
    repo.mkdir()
    home.mkdir()
    git("init", "-q", cwd=repo)
    return repo, home


@dataclass
class Phase:
    query: Callable
    records: list
    name: str

    def observe(self, row):
        row["diff"] = query_difference(self.query, row, self.name)
        return row

    def record(self, op, subject, expected, status=None):
        row = expectation(op, subject, expected, status)
        row["phase"] = self.name
        self.observe(row)
        del row["subset"]  # Ordinary waves omit module membership metadata.
        self.records.append(row)

    def module(self, query, *, member=False, probe=False):
        row = dict(query, phase=self.name, subset=member, probe=probe)
        self.records.append(self.observe(row))
