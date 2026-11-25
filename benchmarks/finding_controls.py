"""Threshold and inert-text controls for profiles without a real-source finding oracle."""


# A row describes one independent source unit and the rule under test. Counts
# come from construction. These are conformance controls, never vendor labels.
def cases():
    # Each header and footer occupies one source line. Make and Procfile keep
    # the final line inside the named target/process.
    sizes = [
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
    branches = [
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
    for length, group in [(True, sizes), (False, branches)]:
        for lang, path, head, body, end, comment in group:
            for target in [69, 70, 71, 72] if length else [8, 9, 10, 11]:
                lines = [head]
                n = target - (2 if length else 1)
                for i in range(n):
                    if comment and target % 2 and i % 5 == 1:
                        lines.append(comment)
                    lines.append(body.replace("N@", str(i)))
                lines.append(end)
                rule = "unit lines" if length else "branch complexity"
                findings = (
                    [dict(unit=head, line=1, rule=rule, value=target)]
                    if target >= (70 if length else 9)
                    else []
                )
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
