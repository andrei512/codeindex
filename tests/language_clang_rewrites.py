"""Source interventions used to identify the causes of Clang oracle mismatches."""
from contextlib import ExitStack, contextmanager
import re
from typing import NamedTuple

from language_compiled_oracle import run, source_change
from language_fixture_positions import utf16_prefix


class Control(NamedTuple):
    name: str
    source: str
    flags: list
    files: tuple = ()

    @contextmanager
    def applied(self, path):
        with ExitStack() as changes:
            for target, text in ((path, self.source), *self.files):
                changes.enter_context(source_change(target, text))
            yield


def qualified_declarators(written, definitions):
    # Move each qualification into an equal-width macro, preserving the
    # qualified compiler definition and original name positions.
    lines = written.lines.copy()
    owners = {}
    for _, site in definitions:
        line, column = site.split(":")
        n = int(line) - 1
        # Compiler sites carry UTF-16 columns; Python slices count Unicode scalars.
        prefix = utf16_prefix(lines[n].rstrip("\r\n"), int(column))
        pos = len(prefix)
        match = re.search(r"([A-Za-z_]\w*)::$", prefix)
        if match:
            owner = match[0]
            alias = owners.setdefault(owner, f"C{len(owners)}")
            lines[n] = lines[n][:match.start()] + alias.ljust(len(owner)) + lines[n][pos:]
    if owners:
        yield Control("qualified method declarator", "".join(lines), [f"-D{alias}={owner}" for owner, alias in owners.items()])


def brainfuck_controls(text):
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
        yield Control("temporary method receiver", rewritten, [])
    # Same constructor and arguments; braces vs parentheses around the object initializer.
    yield Control("direct initialization of named receiver", re.sub(r"(Printer p_(?:left|right))\(true\)", r"\1{true}", text), [])


def receiver_controls(project, path, text):
    if project == "kostya" and path.name == "bf.cpp":
        yield from brainfuck_controls(text)
    if project == "kostya" and path.name == "primes.cpp":
        name = "std::vector<int>"
        alias = path.parent / "cx-test-vector.h"
        header = "#include <vector>\nusing CxVector = std::vector<int>;\n"
        after = re.sub(r"(?<=\d)'(?=\d)", "", text)
        after = re.sub(r"std::vector<int>(?= (?:find|to_list)\()", "CxVector".ljust(len(name)), after)
        yield Control("digit separators plus templated return type", after, ["-include", str(alias)], ((alias, header),))
    if project == "robin" and path.name in ("benchmark.cpp", "mup.cpp"):
        # isValid and fmt each live alone in this anonymous namespace.
        after = text.replace("namespace {", "static", 1).replace("} // namespace", "// namespace", 1)
        yield Control("anonymous namespace visibility", after, [])


def nanopb_controls(written):
    yield Control("opaque feature guard", written.text.replace("#ifndef PB_WITHOUT_64BIT", "#if 1"), [])
    after = written.replace_code(r"\bcheckreturn\b", " " * 11)
    yield Control("return annotation macro", after, [])


TYPE_ALIASES = {
    "sds": {"sds": "SDS"},
    "tinyexpr": {"te_expr": "Te_expr", "te_variable": "Te_variable"},
    "zlib": {"uLong": "ULong"},
    "cjson": {"cJSON": "CJSON", "cJSON_bool": "CJSON_bool"},
}


def type_alias_control(written, aliases):
    # Preserve spelling widths and add exact aliases for Clang.
    after = written.replace_code(r"\b(?:" + "|".join(aliases) + r")\b", lambda word: aliases[word[0]])
    flags = [f"-D{alias}={original}" for original, alias in aliases.items()]
    return Control("lowercase type spelling", after, flags)


def cjson_control(written):
    # Visibility and calling-convention annotations do not affect these bindings.
    after = written.replace_code(r"CJSON_PUBLIC\(([^()\n]+)\)", lambda match: match[1].ljust(len(match[0])))
    return Control("wrapped return type", after, [])


def linenoise_control(path, written):
    # Preserve all compiler declarations while moving duplicate written prototypes.
    headers = {}

    def lift(match):
        header = f"cx-prototype-{match[1]}.h"
        headers[path.parent / header] = match[0] + "\n"
        return f'#include "{header}"'

    pattern = r"^static [^(;]*?\b(refreshLine|refreshLineWithFlags|linenoiseReadLine)\([^;\n]*\);$"
    after = re.sub(pattern, lift, written.text, flags=re.M)
    return Control("prototype and definition counted twice", after, [], tuple(headers.items()))


def needs_guard_control(project, path):
    if project in ("lodepng", "inih", "fmt", "zlib"):
        return True
    return project == "nanopb" and path.name == "pb_decode.c"


def written_controls(project, path, written, compilation):
    flags, definitions = compilation
    if path.suffix in (".cpp", ".cc"):
        yield from qualified_declarators(written, definitions)
    yield from receiver_controls(project, path, written.text)
    if project == "nanopb":
        yield from nanopb_controls(written)
    if aliases := TYPE_ALIASES.get(project):
        yield type_alias_control(written, aliases)
    if project == "cjson":
        yield cjson_control(written)
    if project == "linenoise":
        yield linenoise_control(path, written)
    if needs_guard_control(project, path):
        yield Control("source preprocessor conditions", active_guard_source(path, written, flags), [])


def active_guard_source(path, written, flags):
    # Ask Clang which branches it actually took using unique markers after each
    # complete directive. The resulting program must independently pass collect().
    pattern = re.compile(r"^([ \t]*#[ \t]*(?:if|ifdef|ifndef|elif)\b[^\n]*(?:\n|$))", re.M)
    lines = written.lines.copy()
    marked, spans = [], []
    cursor = 0
    for start, end in written.directives:
        if not pattern.match(lines[start]):
            continue
        marked.extend(lines[cursor:end])
        cursor = end
        spans.append((start, end))
        marked.append(f"\n#pragma cx_guard_active {start}\n")
    marked.extend(lines[cursor:])
    with source_change(path, "".join(marked)):
        driver = "clang" if path.suffix == ".c" else "clang++"
        output = run(driver, *flags, "-E", "-P", str(path)).stdout
        active = set(map(int, re.findall(r"^#pragma cx_guard_active (\d+)$", output, re.M)))
    for start, end in spans:
        write_guard(lines, (start, end), start in active)
    return "".join(lines)


def write_guard(lines, span, enabled):
    start, end = span
    op = "elif" if re.match(r"[ \t]*#[ \t]*elif\b", lines[start]) else "if"
    lines[start] = f"#{op} {int(enabled)}\n"
    for i in range(start + 1, end):
        lines[i] = "\n"


def compose_lines(original, left, right):
    # Source controls preserve row positions; conflicting edits cannot testify jointly.
    return compose_rows(original.splitlines(keepends=True), left, right)


def merge_line(old, left, right):
    if right == old or left == right:
        return left
    if left == old:
        return right


def compose_rows(original, left, right):
    a, b = (source.splitlines(keepends=True) for source in (left, right))
    if len(original) != len(a) or len(original) != len(b):
        return None
    merged = []
    for old, x, y in zip(original, a, b):
        if (line := merge_line(old, x, y)) is None:
            return None
        merged.append(line)
    return "".join(merged)


def add_combined_control(changes, written):
    by_name = {control.name: control for control in changes}
    qualified = by_name.get("qualified method declarator")
    guards = by_name.get("source preprocessor conditions")
    if qualified and guards:
        combined = compose_rows(written.lines, qualified.source, guards.source)
        if combined is not None:
            changes.append(Control("qualified declarator and preprocessor conditions",
                                   combined, qualified.flags, qualified.files))
