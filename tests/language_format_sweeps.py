"""Run fixed, independently labelled format-index experiments. No upstream builds."""
import argparse
import json
from pathlib import Path
import re
import shutil
import tempfile

from language_conformance import ROOT, check, git

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
LAYOUT = {
    "css": ".«card»\n,\n.secondary\n{ color: red; }",
    "html": '<div\n ID\n =\n "«card»">ok</div>',
    "sql": 'CREATE\n TEMPORARY\n TABLE IF NOT EXISTS\n «records»(id int);',
    "dockerfile": 'FROM scratch \\\n AS «stage»\nFROM \\\n ‹stage› AS deliver',
    "makefile": 'all: \\\n  ‹build›\n«build»:\n\t@:',
    "cmake": 'function(\n «build»\n)\nendfunction()\n‹build›(\n)',
    "procfile": '«worker»:\t echo done\n',
    "hcl": 'variable "«zone»" {\n default = "z"\n}\noutput "out" {\n value = (\n   var.‹zone›\n )\n}',
    "nix": 'let\n «setting»\n =\n 1;\n in\n ‹setting›',
    "assembly": '.text\nentry:\n\tcall\t‹helper›\n\tret\n«helper»:\n\tret',
    "mako": '<%def\n name="«greet»()"\n>ok</%def>\n${local.‹greet›()}',
    "plpgsql": 'CREATE\n FUNCTION\n «routine»() RETURNS void\n AS $$ BEGIN NULL; END; $$\n LANGUAGE plpgsql;',
}

# Dialect-native syntax that changes what a parser treats as code. The
# "phantom" negative covers a raw region, literal, comment or escaped name.
# Each file also has a declaration outside that region as a positive control.
BOUNDARIES = {
    "css": '.«card»[data-x=".phantom"] { color: green; }',
    "html": '<iframe><div id="phantom"></div></iframe>\n<div id="«card»"></div>',
    "sql": "CREATE TABLE «records»(x text DEFAULT 'CREATE TABLE phantom(x)');",
    "dockerfile": 'FROM scratch AS «stage»\nLABEL description="FROM scratch AS phantom"\nFROM ‹stage› AS final',
    "makefile": 'all: ‹build›\n«build»:\n\t@echo phantom:',
    "cmake": 'function(«build»)\nendfunction()\nmessage("function(phantom)")\n‹build›()',
    "procfile": '«worker»: echo "phantom: ok"',
    "hcl": 'variable "«zone»" { default = "phantom" }\noutput "out" { value = var.‹zone› }',
    "nix": 'let «setting» = 1; text = "phantom = 2;"; in ‹setting›',
    "assembly": '.text\nentry: call ‹helper›\nret\n«helper»: ret\n.data\n.asciz "phantom: ret"',
    "mako": '<%def name="«greet»()">ok</%def>\n<%text><%def name="phantom()">bad</%def></%text>\n${local.‹greet›()}',
    "plpgsql": "CREATE FUNCTION «routine»() RETURNS void LANGUAGE plpgsql AS $$ BEGIN RAISE NOTICE 'CREATE FUNCTION phantom()'; END; $$;",
}

# References are chosen to distinguish a written static reference from an
# unrelated same-spelling literal or an indirect/computed command.
CONTEXT = {
    "css": ':root { --color: red; }\n.«card»::before { content: "var(--color)"; }',
    "html": '<script src="external.js">function phantom() {}</script>\n<div id="«card»"></div>',
    "sql": 'CREATE TABLE previous (k integer);\nCREATE VIEW «records» AS SELECT previous.k FROM previous;',
    "dockerfile": 'FROM scratch AS «stage»\nFROM scratch AS final\nCOPY --from=‹stage› /from/stage /stage',
    "makefile": 'all: | ‹build›\n«build»:\n\t@echo build',
    "cmake": 'function(«build»)\nendfunction()\nset(text "build()")\n‹build›()',
    "procfile": '«worker»: worker --label worker',
    "hcl": 'variable "«zone»" { default = "zone" }\noutput "out" { value = "zone ${var.‹zone›}" }',
    "nix": 'let «setting» = 1; obj = { other = "setting"; }; in ‹setting›',
    "assembly": '.text\nentry: call ‹helper›\ncall *%rax\nret\n«helper»: ret',
    "mako": '<%def name="«greet»()">ok</%def>\n${"local.greet()"} ${local.‹greet›()}',
    "plpgsql": "CREATE FUNCTION run() RETURNS void AS $$ <<«finish»>> BEGIN RAISE NOTICE 'EXIT finish'; EXIT ‹finish›; END; $$ LANGUAGE plpgsql;",
}

# Prefix sharing inside one scope must preserve both definitions. In the
# relationship-capable cases the written use names only the longer spelling.
PREFIXES = {
    "css": '.«chip», .«chipset» { color: green }',
    "html": '<span id="«chip»"></span><span id="«chipset»"></span>',
    "sql": 'CREATE TABLE «chip»(x int); CREATE TABLE «chipset»(x int);',
    "dockerfile": 'FROM scratch AS «chip»\nFROM scratch AS «chipset»\nFROM ‹chipset› AS deliver',
    "makefile": 'all: ‹chipset›\n«chip»: ; @:\n«chipset»: ; @:',
    "cmake": 'function(«chip»)\nendfunction()\nfunction(«chipset»)\nendfunction()\n‹chipset›()',
    "procfile": '«chip»: echo chipset\n«chipset»: echo chip',
    "hcl": 'variable "«chip»" { default = 1 }\nvariable "«chipset»" { default = 2 }\noutput "out" { value = var.‹chipset› }',
    "nix": 'let «chip» = 1; «chipset» = 2; in ‹chipset›',
    "assembly": '.text\nentry: call ‹chipset›\nret\n«chip»: ret\n«chipset»: ret',
    "mako": '<%def name="«chip»()">a</%def>\n<%def name="«chipset»()">b</%def>\n${local.‹chipset›()}',
    "plpgsql": 'CREATE FUNCTION run() RETURNS void AS $$ <<«chip»>> BEGIN <<«chipset»>> BEGIN EXIT ‹chipset›; END; END; $$ LANGUAGE plpgsql;',
}

# Clang assembled the spelling case for x86_64-apple-macos; nm reports
# helper.internal at offset 6. html5lib 1.1 parses the iframe contents in the
# boundary case as a text node; the only element with an id is the outer div.
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
    body, definitions, uses = "", {}, []
    end = 0
    for m in MARK.finditer(annotated):
        body += annotated[end:m.start()]
        word = m[1] or m[2]
        column = len(body.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2 + 1
        site = f"{name}:{1 + body.count(chr(10))}:{column}"
        if m[1]:
            if word in definitions:
                raise ValueError("duplicate definition in template")
            definitions[word] = site
        else:
            uses.append((word, site))
        body += word
        end = m.end()
    body += annotated[end:]
    path = repo / name
    path.parent.mkdir(exist_ok=True, parents=True)
    path.write_text(body, encoding="utf-8")
    return definitions, uses


def sample(binary, profile, wave):
    name, template = BASE[profile]
    with tempfile.TemporaryDirectory(prefix="cx-formats-") as work:
        repo, home = Path(work) / "repo", Path(work) / "home"
        repo.mkdir()
        home.mkdir()
        git("init", "-q", cwd=repo)
        if WAVES[wave] is not None:
            template = WAVES[wave][profile]
        inputs = {}
        names = [name]
        if wave in ("unrelated-projects", "surviving-copy"):
            names = [f"left/{name}", f"right/{name}"]
        for path in names:
            inputs[path] = write_input(repo, path, template)
        if wave == "surviving-copy":
            # Publish both candidates, then remove one. The remaining file and
            # its local uses keep their original identities and source positions.
            word = next(iter(inputs[names[0]][0]))
            expected = ",".join(defs[word] for defs, _ in inputs.values())
            setup_diff = check(binary, repo, home, "symbol", word, expected, "", False, "fresh")
            yield dict(op="symbol-before-delete", subject=word, expected=expected, diff=setup_diff)
            (repo / names[0]).unlink()
            del inputs[names[0]]
        if wave == "boundaries":
            diff = check(binary, repo, home, "symbol", "phantom", "-", "", False, "query")
            yield dict(op="forbidden-symbol", subject="phantom", expected="-", diff=diff)
        if wave == "context" and profile in {"html", "css"}:
            # The external script's body is inert; the CSS string contains no var() use.
            op, subject, status = ("symbol", "phantom", "") if profile == "html" else (
                "references", "style.css:1:9", "resolved")
            diff = check(binary, repo, home, op, subject, "-", status, False, "query")
            yield dict(op=op, subject=subject, expected="-", diff=diff)
        for word in next(iter(inputs.values()))[0]:
            sites = [defs[word] for defs, _ in inputs.values()]
            expected = ",".join(sites)
            diff = check(binary, repo, home, "symbol", word, expected, "", False, "query")
            yield dict(op="symbol", subject=word, expected=expected, diff=diff)
        for defs, uses in inputs.values():
            for word, site in uses:
                expected = defs[word]
                diff = check(binary, repo, home, "definition", site, expected, "resolved", False, "query")
                yield dict(op="definition", subject=site, expected=expected, diff=diff)
            if wave == "name-prefixes" and uses:
                expected = "-"
                anchor = defs["chip"]
                diff = check(binary, repo, home, "references", anchor, expected, "resolved", False, "query")
                yield dict(op="forbidden-references", subject=anchor, expected=expected, diff=diff)
            for word in dict.fromkeys(w for w, _ in uses):
                anchor = defs[word]
                expected = ",".join(site for w, site in uses if w == word)
                diff = check(binary, repo, home, "references", anchor, expected, "resolved", False, "query")
                yield dict(op="references", subject=anchor, expected=expected, diff=diff)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-format-sweeps.json")
    args = parser.parse_args()
    results = {}
    for wave in WAVES:
        results[wave] = {}
        for profile in BASE:
            results[wave][profile] = list(sample(args.binary.resolve(), profile, wave))
        failures = {p: sum(bool(r["diff"]) for r in rows) for p, rows in results[wave].items()}
        print(wave, sum(len(rows) for rows in results[wave].values()), {profile: count for profile, count in failures.items() if count})
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(results, indent=2) + "\n")
    return int(any(r["diff"] for wave in results.values() for rows in wave.values() for r in rows))


if __name__ == "__main__":
    raise SystemExit(main())
