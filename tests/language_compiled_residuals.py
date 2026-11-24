# /// script
# requires-python = ">=3.9"
# dependencies = ["libclang==18.1.1"]
# ///
"""Causal triage on a deterministic sample of unresolved public Clang discrepancies.

Run after language_compiled_oracle.py, with the same Clang/SDK. Cases are ranked
before examining the new controls. A control counts only if Clang retains the
same written declarations and call targets and cx then produces the expected
answer. A non-recovery leaves that individual mismatch open.
"""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path
import re
import sys
import tempfile

from clang import cindex
from language_compiled_oracle import compiler, collect, run, symbol_probe, write_compile_commands
from language_conformance import ROOT, checkout, check, run_case


def sample_rows(units, waves):
    groups = defaultdict(list)
    excluded = 0
    for unit, data in units.items():
        recovered = {s for key in ("compile_guards", "digit_separators")
                     for s in data.get(key, {}).get("recovered", [])}
        for row in data["rows"]:
            if not row["diff"]:
                continue
            if row["subject"] in recovered:
                excluded += 1
                continue
            ident = "|".join((unit, row["op"], row["subject"], row["expected"]))
            rank = hashlib.sha256(ident.encode()).hexdigest()
            groups[(unit.split("/")[0], row["op"])].append((rank, unit, row))
    selected = defaultdict(list)
    for candidates in groups.values():
        for wave, (_, unit, row) in enumerate(sorted(candidates)[:waves], 1):
            selected[unit].append({"wave": wave, **row, "outcome": "unattributed"})
    return selected, excluded


def rewrite(project, path, text, flags, definitions):
    """List independent controls, each starting from the original source."""
    if path.suffix in (".cpp", ".cc"):
        # Move each qualification into an equal-width macro, preserving the
        # qualified compiler definition and original name positions.
        lines = text.splitlines(keepends=True)
        owners = {}
        for _, site in definitions:
            line, column = site.split(":")
            pos = int(column) - 1
            prefix = lines[int(line)-1][:pos]
            match = re.search(r"([A-Za-z_]\w*)::$", prefix)
            if match:
                owner = match[0]
                alias = owners.setdefault(owner, f"C{len(owners)}")
                n = int(line) - 1
                lines[n] = lines[n][:match.start()] + alias.ljust(len(owner)) + lines[n][pos:]
        if owners:
            yield "qualified method declarator", "".join(lines), [f"-D{alias}={owner}" for owner, alias in owners.items()]
    if project == "kostya" and path.name == "bf.cpp":
        # Keep construction within its lambda and maintain the original run() column.
        raw = "  notifying_invoke([&]() { Program(text, p)"
        alt = "notifying_invoke([&]{Program q{text,p};q"
        if raw in text:
            rewritten = text.replace(raw, alt.ljust(len(raw)))
            # Keep construction of the second temporary just before its run() and
            # leave every eligible direct-call coordinate unchanged.
            rewritten = rewritten.replace("Printer p_left(true);", "Printer p_left(true); Program ql{text,p_left};")
            match = "Program(text, p_left).run()"
            rewritten = rewritten.replace(match, "ql".ljust(len("Program(text, p_left)")) + ".run()")
            yield "temporary method receiver", rewritten, []
        # Same constructor and arguments; braces vs parentheses around the object initializer.
        yield "direct initialization of named receiver", re.sub(r"(Printer p_(?:left|right))\(true\)", r"\1{true}", text), []
    if project == "kostya" and path.name == "primes.cpp":
        name = "std::vector<int>"
        alias = path.parent / "cx-test-vector.h"
        alias.write_text("#include <vector>\nusing CxVector = std::vector<int>;\n")
        after = re.sub(r"(?<=\d)'(?=\d)", "", text)
        after = re.sub(r"std::vector<int>(?= (?:find|to_list)\()", "CxVector".ljust(len(name)), after)
        yield "digit separators plus templated return type", after, ["-include", str(alias)]
    if project == "robin" and path.name in ("benchmark.cpp", "mup.cpp"):
        # isValid and fmt each live alone in this anonymous namespace.
        after = text.replace("namespace {", "static", 1).replace("} // namespace", "// namespace", 1)
        yield "anonymous namespace visibility", after, []
    if project == "nanopb":
        yield "opaque feature guard", text.replace("#ifndef PB_WITHOUT_64BIT", "#if 1"), []
        after = re.sub(r"^(?!\s*#).*\bcheckreturn\b.*$", lambda m: re.sub(r"\bcheckreturn\b", " "*11, m[0]), text, flags=re.M)
        yield "return annotation macro", after, []
    aliases = {
        "sds": {"sds": "SDS"},
        "tinyexpr": {"te_expr": "Te_expr", "te_variable": "Te_variable"},
        "zlib": {"uLong": "ULong"},
        "cjson": {"cJSON": "CJSON", "cJSON_bool": "CJSON_bool"},
    }
    if project in aliases:
        subst = aliases[project]
        # Exact aliases in a forced-include header preserve Clang's program. Preserve
        # byte length to keep the independent source coordinate oracle unchanged.
        after = re.sub(r"^(?!\s*#).*", lambda m: re.sub(r"\b(?:" + "|".join(subst) + r")\b",
                       lambda word: subst[word[0]], m[0]), text, flags=re.M)
        yield "lowercase type spelling", after, [f"-D{alias}={original}" for original, alias in subst.items()]
    if project == "cjson":
        # Visibility/calling-convention annotations do not affect these selected bindings.
        after = re.sub(r"^(?!\s*#).*", lambda m: re.sub(r"CJSON_PUBLIC\(([^()\n]+)\)",
                       lambda match: match[1].ljust(len(match[0])), m[0]), text, flags=re.M)
        yield "wrapped return type", after, []
    if project == "linenoise":
        # Preserve all declarations for the compiler; remove only their duplicate
        # representation from the implementation file for cx.
        def lift(m):
            header = f"cx-prototype-{m[1]}.h"
            (path.parent / header).write_text(m[0] + "\n")
            return f'#include "{header}"'
        after = re.sub(r"^static [^(;]*?\b(refreshLine|refreshLineWithFlags|linenoiseReadLine)\([^;\n]*\);$", lift, text, flags=re.M)
        yield "prototype and definition counted twice", after, []
    if project in ("lodepng", "inih", "fmt", "zlib") or (project == "nanopb" and path.name == "pb_decode.c"):
        yield "source preprocessor conditions", active_guards(path, text, flags), []


def active_guards(path, text, flags):
    # Ask Clang which branches it actually took using unique markers after each
    # complete directive. The resulting program must independently pass collect().
    pattern = re.compile(r"^([ \t]*#[ \t]*(?:if|ifdef|ifndef|elif)\b[^\n]*(?:\n|$))", re.M)
    lines = text.splitlines(keepends=True)
    marked, spans = [], []
    i = 0
    while i < len(lines):
        line = lines[i]
        marked.append(line)
        start = i
        i += 1
        if not pattern.match(line):
            continue
        while marked[-1].rstrip("\r\n").endswith("\\") and i < len(lines):
            marked.append(lines[i])
            i += 1
        spans.append((start, i))
        marked.append(f"\n#pragma cx_guard_active {start}\n")
    try:
        path.write_text("".join(marked))
        driver = "clang" if path.suffix == ".c" else "clang++"
        output = run(driver, *flags, "-E", "-P", str(path)).stdout
        active = set(map(int, re.findall(r"^#pragma cx_guard_active (\d+)$", output, re.M)))
    finally:
        path.write_text(text)
    for start, end in spans:
        op = "elif" if re.match(r"[ \t]*#[ \t]*elif\b", lines[start]) else "if"
        lines[start] = f"#{op} {int(start in active)}\n"
        for i in range(start + 1, end):
            lines[i] = "\n"
    return "".join(lines)


def audit_unit(repo, temp, filename, source, unit, rows, binary):
    path = repo / filename
    project = unit.split("/")[0]
    flags = [arg.replace("<checkout>", str(repo)) for arg in source["arguments"]]
    # Use the actual current checkout for every query. Never reuse an index
    # across independent source interventions.
    tool = "clang" if path.suffix == ".c" else "clang++"
    def configure(extra):
        write_compile_commands(repo, [{"file": filename, "directory": str(repo), "arguments":
                                  [tool, *flags, *extra, "-c", filename]}])
    configure([])
    original_source = path.read_text()
    original_sites = collect(path, flags)
    controls = {}
    # Materialize controls before edits: some controls ask the compiler about the original.
    changes = list(rewrite(project, path, original_source, flags, original_sites[0]))
    controls_by_name = {name: (text, extra) for name, text, extra in changes}
    if "qualified method declarator" in controls_by_name and "source preprocessor conditions" in controls_by_name:
        qualified, extra = controls_by_name["qualified method declarator"]
        literal_guards = controls_by_name["source preprocessor conditions"][0]
        qlines = qualified.splitlines(keepends=True)
        glines = literal_guards.splitlines(keepends=True)
        original = original_source.splitlines(keepends=True)
        if len(original) == len(qlines) == len(glines):
            combined = "".join(g if g != o else q for o, q, g in zip(original, qlines, glines))
            changes.append(("qualified declarator and preprocessor conditions", combined, extra))
    for index, (name, after, extra) in enumerate(changes):
        if after == original_source:
            continue
        path.write_text(after)
        configure(extra)
        try:
            try:
                preserved = collect(path, flags + extra) == original_sites
            except RuntimeError as error:
                controls[name] = {"unverified": str(error)}
                continue
            if not preserved:
                controls[name] = {"unverified": "compiler locations or bindings changed"}
                continue
            home = temp / f"index-{index}"
            home.mkdir()
            cache = {}
            restored = []
            for row in rows:
                if row["op"] == "symbol":
                    diff, censored = symbol_probe(binary, repo, home, row["name"], row["expected"], cache)
                else:
                    diff = check(binary, repo, home, row["op"], row["subject"], row["expected"], "resolved", False, "fresh")
                    censored = None
                if not diff and not censored:
                    row["outcome"] = "recovered"
                    row.setdefault("controls", []).append(name)
                    restored.append(row["subject"])
            controls[name] = {"recovered": restored}
        finally:
            path.write_text(original_source)
            configure([])
    return controls


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT / "target/language-compiled-oracle.json")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-compiled-residuals.json")
    parser.add_argument("--waves", type=int, default=6)
    opts = parser.parse_args()
    if sys.platform == "darwin":
        tool = Path(run("xcrun", "--find", "clang").stdout.strip())
        cindex.Config.set_library_file(str(tool.parent.parent / "lib/libclang.dylib"))
    units = json.loads(opts.source.read_text())["units"]
    selected, excluded = sample_rows(units, opts.waves)
    evidence = {}
    for unit, rows in selected.items():
        data = units[unit]
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            repo = root / "repo"
            repo.mkdir()
            checkout(data, repo, ROOT / "target/conformance-cache")
            controls = audit_unit(repo, root, unit.split("/", 1)[1], data, unit, rows, ROOT / "target/release/cx")
            evidence[unit] = {"controls": controls, "rows": rows}
    witnesses = [
        ("robin/src/test/app/benchmark.cpp", "src/test/app/benchmark.cpp:29:9", "anonymous_namespace"),
        ("kostya/brainfuck/bf.cpp", "brainfuck/bf.cpp:144:28", "direct_initialized_receiver"),
        ("double-conversion/double-conversion/bignum.cc", "double-conversion/bignum.cc:59:5", "const_method_overload"),
        ("kostya/brainfuck/bf.cpp", "brainfuck/bf.cpp:166:45", "temporary_receiver"),
    ]
    minimized = []
    for unit, subject, fixture in witnesses:
        selected_row = next((r for r in selected.get(unit, []) if r["subject"] == subject), None)
        if not selected_row:
            continue
        path = ROOT / "tests/fixtures/languages/cpp" / ("wave_clang_" + fixture)
        results = run_case(path, ROOT / "target/release/cx", ROOT / "target/conformance-cache")
        minimized.append({"case": str(path.relative_to(ROOT)), "unit": unit, "subject": subject,
                          "wave": selected_row["wave"], "original_site_outcome": selected_row["outcome"],
                          "failed_original_queries": sum(r["phase"] == "fresh" and bool(r["diff"]) for r in results),
                          "failed_control_queries": sum(r["phase"] == "update" and bool(r["diff"]) for r in results)})
    opts.report.parent.mkdir(parents=True, exist_ok=True)
    opts.report.write_text(json.dumps({"excluded_previously_recovered": excluded, "units": evidence,
                                       "minimized": minimized}, indent=2) + "\n")
    for wave in range(1, opts.waves+1):
        print(wave, Counter(r["outcome"] for v in evidence.values() for r in v["rows"] if r["wave"] == wave))


if __name__ == "__main__":
    main()
