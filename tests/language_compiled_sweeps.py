"""Independent semantic discovery sweeps for compiled-language and Protobuf fixtures.

The marks enclose expected source locations. They are removed before indexing.
A declared location has no dependency on a previous cx run or an index ordinal.
"""
import argparse
import json
from pathlib import Path
import re
import tempfile

from language_conformance import ROOT, run_case


# Path, language preamble, declaration that must be searchable by its written name.
PROFILES = {
    "rust": ("lib.rs", "", "fn NAME() {}"),
    "go": ("sample.go", "package sample\n", "func NAME() {}"),
    "c": ("sample.c", "", "struct NAME { int n; };"),
    "cpp": ("sample.cpp", "", "class NAME {};"),
    "objectivec": ("Sample.m", "", "@interface NAME\n@end"),
    "csharp": ("Sample.cs", "", "class NAME {}"),
    "swift": ("sample.swift", "", "func NAME() {}"),
    "java": ("Sample.java", "", "class NAME {}"),
    "kotlin": ("Sample.kt", "", "fun NAME() = 1"),
    "dart": ("sample.dart", "", "void NAME() {}"),
    "proto": ("sample.proto", 'syntax = "proto3";\n', "message NAME {}"),
}


class Fixture:
    def __init__(self, folder):
        self.folder = folder
        self.sites = {}
        self.checks = []
        (folder / "repo").mkdir()

    def write(self, path, annotated):
        source = ""
        start = 0
        for mark in re.finditer(r"«(\w+)\|([^»]+)»", annotated):
            source += annotated[start:mark.start()]
            line = 1 + source.count("\n")
            col = 1 + len(source.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2
            self.sites[mark[1]] = f"{path}:{line}:{col}"
            source += mark[2]
            start = mark.end()
        target = self.folder / "repo" / path
        target.parent.mkdir(exist_ok=True, parents=True)
        target.write_text(source + annotated[start:] + "\n")

    def check(self, op, subject, *labels, status=None):
        value = subject if op == "symbol" else self.sites[subject]
        expected = ",".join(self.sites[label] for label in labels) or "-"
        self.checks.append("\t".join([op, value, expected] + ([status] if status else [])))

    def ready(self):
        (self.folder / "checks.tsv").write_text("\n".join(self.checks) + "\n")


def comment_boundary(profile, f):
    """Only the live declaration following the closing comment is source code."""
    path, header, declaration = PROFILES[profile]
    nested = profile in {"rust", "swift", "kotlin", "dart"}
    opening = "/* outer /* inner */" if nested else "/* outer"
    f.write(path, header + opening + "\n" + declaration.replace("NAME", "Ghost") + "\n*/\n"
            + declaration.replace("NAME", "«decl|Live»"))
    f.check("symbol", "Ghost")
    f.check("symbol", "Live", "decl")


def isolated_names(profile, f):
    """Distinct packages, modules or types can contain the same written name."""
    path, header, declaration = PROFILES[profile]
    for label in ("first", "second"):
        body = declaration.replace("NAME", f"«{label}|Repeated»")
        if profile in {"csharp", "cpp"}:
            body = f"namespace {label} {{ {body} }}"
        elif profile in {"java", "kotlin", "proto"}:
            end = "" if profile == "kotlin" else ";"
            body = f"package {label}{end}\n" + body
        elif profile == "go":
            f.write(f"{label}/{path}", f"package {label}\n" + body)
            continue
        elif profile == "swift":
            body = f"enum Owner{label} {{ static {body} }}"
        elif profile == "objectivec":
            body = f"@interface Owner{label}\n- (void)«{label}|Repeated»;\n@end"
        f.write(f"{label}/{path}", header + body)
    f.check("symbol", "Repeated", "first", "second")


def same_line_uses(profile, f):
    """Two true uses share a line with an astral character and a false call in prose."""
    path, header, _ = PROFILES[profile]
    expr = "«one|seek»(); /* 🌱 seek() */ «two|seek»();"
    defs = {
        "rust": ("fn «decl|seek»() {}", "fn run() { BODY }"),
        "go": ("func «decl|seek»() {}", "func run() { BODY }"),
        "c": ("static void «decl|seek»(void) {}", "void run(void) { BODY }"),
        "cpp": ("static void «decl|seek»() {}", "void run() { BODY }"),
        "objectivec": ("static void «decl|seek»(void) {}", "void run(void) { BODY }"),
        "csharp": ("class Scope { static void «decl|seek»() {}", "static void run() { BODY } }"),
        "swift": ("func «decl|seek»() {}", "func run() { BODY }"),
        "java": ("class Scope { static void «decl|seek»() {}", "static void run() { BODY } }"),
        "kotlin": ("fun «decl|seek»() {}", "fun runIt() { BODY }"),
        "dart": ("void «decl|seek»() {}", "void run() { BODY }"),
        "proto": ("message «decl|Item» {}", "message Envelope { BODY }"),
    }
    if profile == "proto":
        expr = "«one|Item» a = 1; /* 🌱 Item */ «two|Item» b = 2;"
    decl, use = defs[profile]
    f.write(path, header + decl + "\n" + use.replace("BODY", expr))
    f.check("definition", "one", "decl")
    f.check("definition", "two", "decl")
    f.check("references", "decl", "one", "two")
    if profile != "proto":
        f.check("callers", "decl", "one", "two")


def string_boundary(profile, f):
    """Internal quote and declaration-shaped text in a literal have no source symbol."""
    forms = {
        "rust": 'const TEXT: &str = r#"left " fn Ghost() {} " right"#;',
        "go": 'const text = `left " func Ghost() {} " right`',
        "c": r'const char *text = "left \" struct Ghost { int n; }; \" right";',
        "cpp": 'const char *text = R"tag(left " class Ghost {}; " right)tag";',
        "objectivec": r'const char *text = "left \" static void Ghost(void) {} \" right";',
        "csharp": 'class Payload { const string Text = """left " class Ghost {} " right"""; }',
        "swift": 'let text = #"left " func Ghost() {} " right"#',
        "java": 'class Payload { static String text = """\nleft " class Ghost {} " right\n"""; }',
        "kotlin": 'val text = """left " fun Ghost() = 0 " right"""',
        "dart": 'const text = """left " void Ghost() {} " right""";',
        "proto": r'option java_package = "left \" message Ghost {} \" right";',
    }
    path, header, declaration = PROFILES[profile]
    f.write(path, header + forms[profile] + "\n" + declaration.replace("NAME", "«decl|Live»"))
    f.check("symbol", "Ghost")
    f.check("symbol", "Live", "decl")


def isolated_edges(profile, f):
    """Neither incoming nor outgoing edges may cross two projects' lexical owners."""
    path, header, _ = PROFILES[profile]
    for owner in ("first", "second"):
        decl = f"«{owner}Def|seek»"
        use = f"«{owner}Use|seek»"
        sources = {
            "rust": f"fn {decl}() {{}}\nfn run() {{ {use}(); }}",
            "go": f"func {decl}() {{}}\nfunc run() {{ {use}() }}",
            "c": f"static void {decl}(void) {{}}\nstatic void run(void) {{ {use}(); }}",
            "cpp": f"static void {decl}() {{}}\nstatic void run() {{ {use}(); }}",
            "objectivec": f"static void {decl}(void) {{}}\nstatic void run(void) {{ {use}(); }}",
            "java": f"package {owner};\nclass Sample {{ static void {decl}() {{}}\nstatic void run() {{ {use}(); }} }}",
            "kotlin": f"package {owner}\nfun {decl}() {{}}\nfun runIt() {{ {use}() }}",
            "csharp": f"namespace {owner} {{ class Sample {{ static void {decl}() {{}}\nstatic void Run() {{ {use}(); }} }} }}",
            "swift": f"enum Scope{owner} {{ static func {decl}() {{}}\nstatic func run() {{ {use}() }} }}",
            "dart": f"void {decl}() {{}}\nvoid run() {{ {use}(); }}",
            "proto": f"package {owner};\nmessage {decl} {{}}\nmessage Envelope {{ {use} item = 1; }}",
        }
        preamble = f"package {owner}\n" if profile == "go" else header
        filename = f"{owner}.rs" if profile == "rust" else f"{owner}/{path}"
        f.write(filename, preamble + sources[profile])
        f.check("definition", owner + "Use", owner + "Def")
        f.check("references", owner + "Def", owner + "Use")
        if profile != "proto":
            f.check("callers", owner + "Def", owner + "Use")
    if profile == "rust":
        f.write("lib.rs", "mod first; mod second;")


def noncall_use(profile, f):
    """Taking a function as a value must leave the caller set unchanged."""
    path, header, _ = PROFILES[profile]
    d, u, v = "«decl|seek»", "«invoked|seek»", "«value|seek»"
    sources = {
        "rust": f"fn {d}() {{}}\nfn run() {{ let _f: fn() = {v}; {u}(); }}",
        "go": f"func {d}() {{}}\nfunc run() {{ f := {v}; _ = f; {u}() }}",
        "c": f"static void {d}(void) {{}}\nvoid run(void) {{ void (*f)(void) = {v}; {u}(); }}",
        "cpp": f"static void {d}() {{}}\nvoid run() {{ auto f = &{v}; {u}(); }}",
        "objectivec": f"static void {d}(void) {{}}\nvoid run(void) {{ void (*f)(void) = {v}; {u}(); }}",
        "csharp": f"class Scope {{ static void {d}() {{}}\nstatic void Run() {{ System.Action f = {v}; {u}(); }} }}",
        "swift": f"func {d}() {{}}\nfunc run() {{ let f: () -> Void = {v}; _ = f; {u}() }}",
        "java": f"class Scope {{ static void {d}() {{}}\nstatic void run() {{ Runnable f = Scope::{v}; {u}(); }} }}",
        "kotlin": f"fun {d}() {{}}\nfun runIt() {{ val f = ::{v}; {u}() }}",
        "dart": f"void {d}() {{}}\nvoid run() {{ final f = {v}; {u}(); }}",
        "proto": f"message {d} {{}}\nmessage Envelope {{ {v} item = 1; }}",
    }
    f.write(path, header + sources[profile])
    if profile == "proto":
        f.check("callers", "decl", status="unsupported")
        f.check("references", "decl", "value")
    else:
        f.check("callers", "decl", "invoked")
        f.check("definition", "invoked", "decl")
        # Profiles primarily promise direct call resolution. Keep this different
        # product question visible without changing the required caller result.
        if profile in {"go", "java", "kotlin", "dart", "objectivec"}:
            f.check("definition", "value", "decl")
            f.check("references", "decl", "value", "invoked")
        else:
            f.check("?definition", "value", "decl")


def equal_length_edit(profile, f):
    """Change every bound name in place. Snapshot reuse must retire the old symbol."""
    same_line_uses(profile, f)
    old = "Item" if profile == "proto" else "seek"
    new = "Unit" if profile == "proto" else "peek"
    path = PROFILES[profile][0]
    before = (f.folder / "repo" / path).read_text()
    (f.folder / "edit").mkdir()
    (f.folder / "edit" / path).write_text(before.replace(old, new))
    f.check("symbol", old, "decl")
    f.check("symbol", new)
    updated = []
    for row in f.checks:
        fields = row.split("\t")
        if fields[0] == "symbol":
            fields[2] = f.sites["decl"] if fields[1] == new else "-"
        updated.append("\t".join(fields))
    (f.folder / "edited.tsv").write_text("\n".join(updated) + "\n")


def token_edges(profile, f):
    """An interior UTF-16 column still selects the use. Its trailing delimiter does not."""
    same_line_uses(profile, f)
    f.checks.clear()
    # All use names in the generated corpus are four ASCII characters.
    for mark in ("one", "two"):
        path, line, column = f.sites[mark].rsplit(":", 2)
        for offset, targets in ((3, ("decl",)), (4, ())):
            key = f"{mark}_{offset}"
            f.sites[key] = f"{path}:{line}:{int(column) + offset}"
            f.check("definition", key, *targets, status="resolved" if targets else "unresolved")


WAVES = {
    "comment-boundary": comment_boundary,
    "isolated-names": isolated_names,
    "same-line-uses": same_line_uses,
    "string-boundary": string_boundary,
    "isolated-edges": isolated_edges,
    "noncall-use": noncall_use,
    "equal-length-edit": equal_length_edit,
    "token-edges": token_edges,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wave", choices=WAVES, action="append")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-compiled-sweeps.json")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    args = parser.parse_args()
    results = {}
    for wave in args.wave or WAVES:
        for profile in PROFILES:
            with tempfile.TemporaryDirectory() as tmp:
                case = Fixture(Path(tmp))
                WAVES[wave](profile, case)
                case.ready()
                rows = run_case(case.folder, args.binary.resolve(), ROOT / "target/conformance-cache")
                results[f"{wave}/{profile}"] = rows
            for row in rows:
                if row["diff"] and row["phase"] == "fresh":
                    print(wave, profile, row["op"], row["subject"], row["diff"])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(results, indent=2) + "\n")
    fails = sum(bool(row["diff"]) and not row["probe"] for rows in results.values() for row in rows)
    print(f"{len(results)} cases, {fails} failed observations; {args.report}")
    return int(bool(fails))


if __name__ == "__main__":
    raise SystemExit(main())
