"""Threshold and inert-text controls for profiles without a real-source finding oracle."""


# Each header and footer occupies one source line. Make and Procfile keep
# the final line inside the named target/process.
SIZES = [
    ("CSS", "x.css", ".example {", "  --item-N@: N@;", "}", "/* if while */"),
    (
        "HTML",
        "x.html",
        "<main>",
        "  <p>line N@</p>",
        "</main>",
        "<!-- if while -->",
    ),
    ("HCL", "x.tf", "locals {", "  keyN@ = N@", "}", "# if while"),
    ("Markdown", "x.md", "# Section", "paragraph N@", "final paragraph", ""),
    (
        "Dockerfile",
        "Dockerfile",
        "FROM scratch AS build",
        "RUN echo N@",
        "RUN true",
        "# RUN example",
    ),
    ("Make", "Makefile", "all:", "\t@echo N@", "\t@true", "# example"),
    (
        "Procfile",
        "Procfile",
        "worker: run " + chr(92),
        "  --optionN@ " + chr(92),
        "  --end",
        "",
    ),
]
BRANCHES = [
    ("Assembly", "x.s", "check:", "  jne done", "  ret", ""),
    ("Batch", "x.bat", ":check", "IF EXIST input.txt ECHO found", "EXIT /B", ""),
    (
        "SQL",
        "x.sql",
        "SELECT CASE",
        "WHEN x = N@ THEN N@",
        "ELSE 0 END;",
        "-- WHEN x THEN y",
    ),
    (
        "PL/pgSQL",
        "x.plpgsql",
        "BEGIN",
        "IF x = N@ THEN PERFORM work(); END IF;",
        "END;",
        "-- IF x THEN NULL; END IF;",
    ),
    (
        "Nix",
        "x.nix",
        "{ x }:",
        "if x == N@ then N@ else",
        "0",
        "# if x then y else z",
    ),
    (
        "Mako",
        "x.mako",
        '<%def name="check(x)">',
        "% if x == N@:\nvalue\n% endif",
        "</%def>",
        "",
    ),
]


def control_rows(profile, rule, targets, dimensions):
    threshold, overhead = dimensions
    lang, path, head, body, end, comment = profile
    for target in targets:
        lines = [head]
        annotated = comment and target % 2
        for i in range(target - overhead):
            if annotated and i % 5 == 1:
                lines.append(comment)
            lines.append(body.replace("N@", str(i)))
        lines.append(end)
        findings = [dict(unit=head, line=1, rule=rule, value=target)] if target >= threshold else []
        yield dict(
            project="controls/" + lang,
            revision="1",
            language=lang,
            path=f"{target}/{path}",
            source="\n".join(lines) + "\n",
            findings=findings,
            rules=[rule],
            include_file=True,
        )


def cases():
    # Counts come from construction. These are conformance controls, never vendor labels.
    for profile in SIZES:
        yield from control_rows(profile, "unit lines", range(69, 73), (70, 2))
    for profile in BRANCHES:
        yield from control_rows(profile, "branch complexity", range(8, 12), (9, 1))
