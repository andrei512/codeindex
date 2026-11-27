"""Check written Python declarations in pinned Django packages against CPython."""
import argparse
import ast
import dis
from collections import Counter, defaultdict
from dataclasses import dataclass
import io
from itertools import tee
import json
from pathlib import Path
import re
import shutil
import sys
import tempfile
import tokenize
from types import CodeType

from language_fixture_positions import utf16_width
from language_conformance import symbol_error, FIXTURES, ROOT, QuerySession, expectation, checkout, git

# Independent populations: migration operations, template engine, form declarations.
# Frozen after independent source review on CPython 3.9 and 3.12.
# File, declaration, local-call and imported-call counts expose accidental loss of coverage.
PACKAGES = {
    "django/db/migrations": (19, 530, 27, 16),
    "django/template": (27, 481, 27, 12),
    "django/forms": (9, 491, 9, 8),
}
KINDS = {"class": "class_definition", "def": "function_definition"}


def global_reads(code):
    """Pairs of line and spelling loaded from the module's globals by CPython."""
    starts = dict(dis.findlinestarts(code))
    line = code.co_firstlineno
    for instr in dis.get_instructions(code):
        line = starts.get(instr.offset, line)
        if instr.opname == "LOAD_GLOBAL":
            yield line, instr.argval
    children = (const for const in code.co_consts if isinstance(const, CodeType))
    for child in children:
        yield from global_reads(child)


def binding_name(node):
    if isinstance(node, ast.Name) and isinstance(node.ctx, (ast.Store, ast.Del)):
        return node.id
    elif isinstance(node, ast.alias):
        return node.asname or node.name.split(".")[0]
    elif isinstance(node, ast.arg):
        return node.arg
    elif isinstance(node, ast.ExceptHandler) and node.name:
        return node.name
    return pattern_capture(node)


def pattern_capture(node):
    # Capture patterns bind names stored as strings, with no ast.Name(Store).
    if type(node).__name__ in {"MatchAs", "MatchStar", "MatchMapping"}:
        return getattr(node, "name", None) or getattr(node, "rest", None)


@dataclass
class Module:
    tree: ast.AST
    source: str
    bindings: Counter
    declarations: Counter
    occurrences: Counter
    calls: list
    functions: set
    imports: list

    @classmethod
    def parse(cls, path, source):
        tree = ast.parse(source, str(path))
        module = cls(tree, source, Counter(), Counter(), Counter(), [], set(), [])
        for node in ast.walk(tree):
            module.observe_node(node)
        for node in tree.body:
            module.observe_top_level(node)
        return module

    def observe_node(self, node):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            self.declarations[node.name, node.lineno] += 1
            self.bindings[node.name] += 1
            return
        if name := binding_name(node):
            self.bindings[name] += 1
        if isinstance(node, ast.Name):
            self.occurrences[node.lineno, node.id] += 1
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            self.calls.append(node.func)

    def observe_top_level(self, node):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and not node.decorator_list:
            self.functions.add((node.name, node.lineno))
        elif isinstance(node, ast.ImportFrom) and node.module:
            self.imports.append(node)


def imported_module(path, node, modules):
    parent = path.split("/")[:-1]
    if node.level > len(parent):
        return None
    prefix = parent[:len(parent) - node.level + 1] if node.level else []
    stem = "/".join([*prefix, *node.module.split(".")])
    return stem + ".py" if stem + ".py" in modules else stem + "/__init__.py"


def call_targets(path, modules, exports):
    info = modules[path]
    targets = {name: (path, name) for name, _ in info.functions}
    for node in info.imports:
        module = imported_module(path, node, modules)
        for alias in node.names:
            key = module, alias.name
            if key in exports:
                targets[alias.asname or alias.name] = key
    return targets


def call_site(path, name, lines):
    line = lines[name.lineno - 1]
    width = name.col_offset if line.isascii() else utf16_width(line.encode()[:name.col_offset].decode())
    return f"{path}:{name.lineno}:{width + 1}"


def verified_reads(module, global_reads):
    for call in module.calls:
        site = call.lineno, call.id
        if module.bindings[call.id] == module.occurrences[site] == 1 and site in global_reads:
            yield call


def module_calls(path, modules, declarations, exports):
    module = modules[path]
    bound = module.bindings
    if not module.calls or "*" in bound:
        return
    targets = call_targets(path, modules, exports)
    reads = set(global_reads(compile(module.tree, path, "exec")))
    lines = module.source.splitlines()
    for f in verified_reads(module, reads):
        key = targets.get(f.id)
        if key not in declarations:
            continue
        site = call_site(path, f, lines)
        yield "local" if key[0] == path else "import", site, declarations[key]


def direct_calls(modules, declarations):
    """Select statically named globals once, independently of the index."""
    exports = {(path, name) for path, name in declarations
               if "*" not in modules[path].bindings and modules[path].bindings[name] == 1}
    return [call for path in modules for call in module_calls(path, modules, declarations, exports)]


def adjacent_tokens(tokens):
    left, right = tee(tokens)
    next(right, None)
    return zip(left, right)


def declaration_tokens(tokens):
    for keyword, name in adjacent_tokens(tokens):
        if keyword.type == name.type == tokenize.NAME and keyword.string in KINDS:
            yield keyword, name


def collect_declarations(module, rel, declarations, expected):
    ast_names = module.declarations
    tokens = tokenize.generate_tokens(io.StringIO(module.source).readline)
    sites = Counter()
    for keyword, name in declaration_tokens(tokens):
        declared = name.string, keyword.start[0]
        sites[declared] += 1
        line, idx = name.start
        prefix = name.line[:idx]
        col = 1 + utf16_width(prefix)
        byte_col = 1 + (idx if prefix.isascii() else len(prefix.encode()))
        if declared in module.functions:
            declarations[(rel, name.string)] = f"{rel}:{line}:{col}"
        expected[name.string].append((rel, line, col, KINDS[keyword.string], byte_col))
    if ast_names != sites:
        raise ValueError(f"CPython AST/token anchor mismatch in {rel}: {ast_names - sites}, {sites - ast_names}")


def collect(repo, package):
    expected = defaultdict(list)
    candidates = set()
    modules, declarations = {}, {}
    for path in sorted((repo / package).rglob("*.py")):
        with tokenize.open(path) as f:
            source = f.read()
        module = Module.parse(path, source)
        rel = str(path.relative_to(repo))
        modules[rel] = module
        collect_declarations(module, rel, declarations, expected)
        candidates.update(re.findall(r"\b(?:def|class)\s+([^\W\d]\w*)", source))
    return expected, sorted(candidates | set(expected)), direct_calls(modules, declarations), len(modules)


def verify_symbol_columns():
    # Byte and UTF-16 columns differ when there is an astral character before a name.
    want = [("fixture.py", 1, 8, "function_definition", 10)]
    hit = dict(path="fixture.py", line=1, col=8, byte_column=10, name="f", kind="function_definition")
    payload = dict(mode="symbol", hits=[hit], truncated=False)
    if symbol_mismatch("f", want, 0, payload):
        raise RuntimeError("valid symbol rejected")
    hit["byte_column"] = 8
    if not symbol_mismatch("f", want, 0, payload):
        raise RuntimeError("incorrect byte column accepted")
    hit.update(line=True, col=1, byte_column=1)
    if not symbol_mismatch("f", [("fixture.py", 1, 1, "function_definition", 1)], 0, payload):
        raise RuntimeError("boolean coordinate accepted")


def verify_call_admission():
    # A LOAD_GLOBAL elsewhere on the same line cannot establish a lambda parameter's identity.
    sources = {
        "local.py": "def emit(): pass\ndef mixed(): return (lambda emit: emit())(lambda: 0) or emit()\n",
        "provider.py": "def changed(): pass\nchanged = print\ndef stable(): pass\n",
        "consumer.py": "from provider import changed, stable\ndef run():\n    changed()\n    stable()\n",
    }
    modules = {path: Module.parse(path, src) for path, src in sources.items()}
    decls = {("local.py", "emit"): "local.py:1:5", ("provider.py", "changed"): "provider.py:1:5",
             ("provider.py", "stable"): "provider.py:3:5"}
    if sys.version_info >= (3, 10):
        src = "def go(): pass\nmatch None:\n    case go: pass\ndef caller(): go()\n"
        modules["pattern.py"] = Module.parse("pattern.py", src)
        decls["pattern.py", "go"] = "pattern.py:1:5"
    if direct_calls(modules, decls) != [("import", "consumer.py:4:5", "provider.py:3:5")]:
        raise RuntimeError("independent binding oracle admitted a shadowed or rebound name")


def verify_oracle():
    verify_symbol_columns()
    verify_call_admission()


def symbol_mismatch(name, expected, exit_code, payload):
    if error := symbol_error(payload, name, exit_code):
        return dict(name=name, **error)
    want = sorted(expected)
    got = sorted((h["path"], h["line"], h["col"], h["kind"], h["byte_column"]) for h in payload["hits"])
    if got != want[:50] or exit_code != int(not want) or payload["truncated"] != (len(want) > 50):
        return dict(name=name, expected=want[:50], actual=got)


def symbol_differences(query, names, expected):
    mismatches = []
    for name in names:
        proc = query.request("symbol", name, timeout=90)
        payload = json.loads(proc.stdout)
        mismatch = symbol_mismatch(name, expected.get(name, ()), proc.returncode, payload)
        if mismatch:
            mismatches.append(mismatch)
    return mismatches


def check_package(binary, tmp, upstream, package):
    repo = tmp / package.replace("/", "-")
    home = tmp / (repo.name + "-home")
    home.mkdir()
    for original in (upstream / package).rglob("*.py"):
        dest = repo / original.relative_to(upstream)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(original, dest)
    git("init", "-q", cwd=repo)
    expected, names, calls, files = collect(repo, package)
    declarations = sum(map(len, expected.values()))
    kinds = Counter(kind for kind, _, _ in calls)
    observed = (files, declarations, kinds["local"], kinds["import"])
    if observed != PACKAGES[package]:
        raise RuntimeError(f"{package}: CPython sample changed from {PACKAGES[package]} to {observed}")
    run_query = QuerySession(binary, repo, home)
    mismatches = symbol_differences(run_query, names, expected)
    binding_diffs = []
    for kind, site, target in calls:
        diff = run_query(expectation("definition", site, target), "oracle")
        if diff:
            binding_diffs.append(dict(kind=kind, site=site, target=target, diff=diff))
    print(package, "direct calls:", dict(kinds), "binding differences:", len(binding_diffs), flush=True)
    summary = dict(call_queries=dict(kinds), binding_diffs=binding_diffs, files=files,
                            declarations=declarations, names=len(names), mismatches=mismatches)
    print(package, files, "files,", len(names), "queries,", len(mismatches), "mismatches", flush=True)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-python-oracle.json")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    verify_oracle()
    spec = json.loads((FIXTURES / "python/public-django/public.json").read_text())
    results = {}
    with tempfile.TemporaryDirectory(prefix="cx-ast-") as tmp:
        tmp = Path(tmp)
        upstream = tmp / "upstream"
        upstream.mkdir()
        checkout(spec, upstream, ROOT / "target/conformance-cache")
        for package in PACKAGES:
            results[package] = check_package(binary, tmp, upstream, package)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(dict(python=sys.version.split()[0], commit=spec["commit"], packages=results), indent=2) + "\n")
    return int(any(v["mismatches"] or v["binding_diffs"] for v in results.values()))


if __name__ == "__main__":
    raise SystemExit(main())
