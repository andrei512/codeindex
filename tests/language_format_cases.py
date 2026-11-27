"""Source templates for format boundaries, embedded language context and name prefixes."""
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
