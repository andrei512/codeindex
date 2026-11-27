"""Run fixed, independently labelled format-index experiments. No upstream builds."""
import argparse
from dataclasses import dataclass
import json
from pathlib import Path
import re
import shutil
import tempfile

from language_fixture_positions import advance
from language_conformance import ROOT, QuerySession, expectation, git

from language_format_cases import BOUNDARIES, CONTEXT, LAYOUT, PREFIXES

# «name» marks a written searchable declaration; ‹name› marks a use of it.
# Coordinates are calculated from source, never copied from cx.
# One isolated project per language and experiment keeps independent bindings apart.
BASE = {
    "css": ("style.css", ".«card» { color: red; }\n"),
    "html": ("index.html", '<div id="«card»"></div>\n'),
    "sql": ("schema.sql", "CREATE TABLE «records» (id integer);\n"),
    "dockerfile": ("Dockerfile", "FROM scratch AS «stage»\nFROM ‹stage› AS final\n"),
    "makefile": ("Makefile", "all: ‹build›\n«build»:\n\t@:\n"),
    "cmake": ("CMakeLists.txt", "function(«build»)\nendfunction()\n‹build›()\n"),
    "procfile": ("Procfile", "«worker»: echo done\n"),
    "hcl": ("main.tf", 'variable "«zone»" { default = "a" }\noutput "out" { value = var.‹zone› }\n'),
    "nix": ("default.nix", "let «setting» = 1; in ‹setting›\n"),
    "assembly": ("main.s", ".text\nentry: call ‹helper›\nret\n«helper»: ret\n"),
    "mako": ("page.mako", '<%def name="«greet»()">ok</%def>\n${local.‹greet›()}\n'),
    "plpgsql": ("schema.plpgsql", "CREATE FUNCTION run() RETURNS void LANGUAGE plpgsql AS $$\n<<«finish»>> BEGIN EXIT ‹finish›; END;\n$$;\n"),
}

# Each spelling is a literal user-defined name allowed in that syntax.
# The SQL case uses a PostgreSQL/SQLite dollar sign inside an unquoted name.
SPELLINGS = {
    "css": ".«nav-item» { color: red; }",
    "html": '<section id="«nav.item»"></section>',
    "sql": 'CREATE TABLE «item$part» (id int);',
    "dockerfile": "FROM scratch AS «base-stage»\nFROM ‹base-stage› AS final",
    "makefile": "all: ‹bundle.tar›\n«bundle.tar»:\n\t@:",
    "cmake": "add_custom_target(«bundle-one»)\nadd_custom_target(main)\nadd_dependencies(main ‹bundle-one›)",
    "procfile": "«worker_2»: echo ok",
    "hcl": 'variable "«zone-name»" { default = 1 }\noutput "out" { value = var.‹zone-name› }',
    "nix": "let «valid-name» = 1; in ‹valid-name›",
    "assembly": ".text\nentry: call ‹helper.internal›\nret\n«helper.internal»: ret",
    "mako": '<%def name="«render_2»()">ok</%def>\n${local.‹render_2›()}',
    "plpgsql": 'CREATE FUNCTION «fn_2»() RETURNS void AS $$ BEGIN NULL; END; $$ LANGUAGE plpgsql;',
}

# Token boundaries can move across lines without changing these declarations.
# Procfile, assembly and Makefile are line-oriented; use their supported
# whitespace/continuation forms instead of breaking required statement boundaries.
WAVES = {
    "spellings": SPELLINGS,
    "layout": LAYOUT,
    "unrelated-projects": None,
    "surviving-copy": None,
    "boundaries": BOUNDARIES,
    "context": CONTEXT,
    "name-prefixes": PREFIXES,
}

MARK = re.compile(r"«([^»]+)»|‹([^›]+)›")


def write_input(repo, name, annotated):
    pieces, definitions, uses = [], {}, []
    line, column = 1, 1
    end = 0
    for m in MARK.finditer(annotated):
        fragment = annotated[end:m.start()]
        pieces.append(fragment)
        line, column = advance(line, column, fragment)
        word = m[1] or m[2]
        site = f"{name}:{line}:{column}"
        if m[1]:
            if word in definitions:
                raise ValueError("duplicate definition in template")
            definitions[word] = site
        else:
            uses.append((word, site))
        pieces.append(word)
        line, column = advance(line, column, word)
        end = m.end()
    pieces.append(annotated[end:])
    path = repo / name
    path.parent.mkdir(exist_ok=True, parents=True)
    path.write_text("".join(pieces), encoding="utf-8")
    return definitions, uses


@dataclass(frozen=True)
class FormatProbe:
    run_query: QuerySession

    def observe(self, op, subject, expected, *, status=None):
        query = expectation(op, subject, expected, status)
        diff = self.run_query(query, "query")
        return dict(op=op, subject=subject, expected=expected, diff=diff)


def observe_transition(probe, inputs, repo, names):
    # Publish both candidates, then remove one. The remaining file and
    # its local uses keep their original identities and source positions.
    first = names[0]
    word = next(iter(inputs[first][0]))
    expected = ",".join(defs[word] for defs, _ in inputs.values())
    diff = probe.run_query(expectation("symbol", word, expected), "fresh")
    yield dict(op="symbol-before-delete", subject=word, expected=expected, diff=diff)
    (repo / first).unlink()
    del inputs[first]


def preflight_checks(probe, wave, profile):
    if wave == "boundaries":
        yield {**probe.observe("symbol", "phantom", "-"), "op": "forbidden-symbol"}
    if wave != "context":
        return
    # An external script body is inert and a CSS string contributes no var() use.
    if profile == "html":
        yield probe.observe("symbol", "phantom", "-", status="")
    if profile == "css":
        yield probe.observe("references", "style.css:1:9", "-", status="resolved")


def relationship_checks(probe, defs, uses, wave):
    by_name = {}
    for word, site in uses:
        yield probe.observe("definition", site, defs[word])
        by_name.setdefault(word, []).append(site)
    if wave == "name-prefixes" and uses:
        yield {**probe.observe("references", defs["chip"], "-"), "op": "forbidden-references"}
    for word, sites in by_name.items():
        yield probe.observe("references", defs[word], ",".join(sites))


def sample(binary, profile, wave):
    name, template = BASE[profile]
    with tempfile.TemporaryDirectory(prefix="cx-formats-") as work:
        repo, home = Path(work) / "repo", Path(work) / "home"
        repo.mkdir()
        home.mkdir()
        git("init", "-q", cwd=repo)
        if WAVES[wave] is not None:
            template = WAVES[wave][profile]
        names = [name]
        if wave in ("unrelated-projects", "surviving-copy"):
            names = [f"left/{name}", f"right/{name}"]
        probe = FormatProbe(QuerySession(binary, repo, home))
        inputs = {path: write_input(repo, path, template) for path in names}
        if wave == "surviving-copy":
            yield from observe_transition(probe, inputs, repo, names)
        yield from preflight_checks(probe, wave, profile)
        for word in next(iter(inputs.values()))[0]:
            expected = ",".join(defs[word] for defs, _ in inputs.values())
            yield probe.observe("symbol", word, expected)
        for defs, uses in inputs.values():
            yield from relationship_checks(probe, defs, uses, wave)


def run_waves(binary):
    results = {}
    for wave in WAVES:
        results[wave] = {profile: list(sample(binary, profile, wave)) for profile in BASE}
        failures = {p: sum(bool(r["diff"]) for r in rows) for p, rows in results[wave].items()}
        print(wave, sum(len(rows) for rows in results[wave].values()), {profile: count for profile, count in failures.items() if count})
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-format-sweeps.json")
    args = parser.parse_args()
    results = run_waves(args.binary.resolve())
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(results, indent=2) + "\n")
    return int(any(r["diff"] for wave in results.values() for rows in wave.values() for r in rows))


if __name__ == "__main__":
    raise SystemExit(main())
