"""Check written Python declarations in pinned Django packages against CPython."""
import argparse
import ast
import dis
from collections import Counter, defaultdict
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tokenize
from types import CodeType

from language_conformance import FIXTURES, ROOT, check, checkout, git

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
    reads = set()
    starts = dict(dis.findlinestarts(code))
    line = code.co_firstlineno
    for instr in dis.get_instructions(code):
        line = starts.get(instr.offset, line)
        if instr.opname == "LOAD_GLOBAL":
            reads.add((line, instr.argval))
    for const in code.co_consts:
        if isinstance(const, CodeType):
            reads.update(global_reads(const))
    return reads


def direct_calls(modules, declarations):
    """Select statically named globals once, independently of the index."""
    result = []
    bindings = {}
    for path, (tree, _) in modules.items():
        bound = Counter()
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                bound[node.name] += 1
            elif isinstance(node, ast.Name) and isinstance(node.ctx, (ast.Store, ast.Del)):
                bound[node.id] += 1
            elif isinstance(node, ast.alias):
                bound[node.asname or node.name.split(".")[0]] += 1
            elif isinstance(node, ast.arg):
                bound[node.arg] += 1
            elif isinstance(node, ast.ExceptHandler) and node.name:
                bound[node.name] += 1
        bindings[path] = bound
    for path, (tree, source) in modules.items():
        bound = bindings[path]
        if "*" in bound:
            continue
        body = list(ast.walk(tree))
        occurrences = Counter((n.lineno, n.id) for n in body if isinstance(n, ast.Name))
        targets = {}
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and not node.decorator_list:
                targets[node.name] = (path, node.name)
            elif isinstance(node, ast.ImportFrom) and node.module:
                parent = path.split("/")[:-1]
                if node.level > len(parent):
                    continue
                prefix = parent[:len(parent) - node.level + 1] if node.level else []
                stem = "/".join([*prefix, *node.module.split(".")])
                module = stem + ".py" if stem + ".py" in modules else stem + "/__init__.py"
                for alias in node.names:
                    if (module, alias.name) in declarations and bindings[module][alias.name] == 1 and "*" not in bindings[module]:
                        targets[alias.asname or alias.name] = (module, alias.name)
        reads = global_reads(compile(tree, path, "exec"))
        lines = source.splitlines()
        for node in body:
            if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Name):
                continue
            f = node.func
            if bound[f.id] != 1 or occurrences[(f.lineno, f.id)] != 1 or (f.lineno, f.id) not in reads:
                continue
            key = targets.get(f.id)
            if key not in declarations:
                continue
            prefix = lines[f.lineno - 1].encode()[:f.col_offset].decode()
            site = f"{path}:{f.lineno}:{1 + len(prefix.encode('utf-16-le')) // 2}"
            result.append(("local" if key[0] == path else "import", site, declarations[key]))
    return result


def collect(repo, package):
    expected = defaultdict(list)
    candidates = set()
    modules, declarations = {}, {}
    for path in sorted((repo / package).rglob("*.py")):
        with tokenize.open(path) as f:
            source = f.read()
        tree = ast.parse(source, str(path))
        rel = str(path.relative_to(repo))
        modules[rel] = (tree, source)
        top_functions = {(n.name, n.lineno) for n in tree.body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)) and not n.decorator_list}
        ast_names = Counter((n.name, n.lineno) for n in ast.walk(tree)
                            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)))
        tokens = list(tokenize.generate_tokens(io.StringIO(source).readline))
        sites = Counter()
        for keyword, name in zip(tokens, tokens[1:]):
            if keyword.type != tokenize.NAME or keyword.string not in KINDS or name.type != tokenize.NAME:
                continue
            sites[(name.string, keyword.start[0])] += 1
            line, idx = name.start
            col = 1 + len(name.line[:idx].encode("utf-16-le")) // 2
            if (name.string, keyword.start[0]) in top_functions:
                declarations[(rel, name.string)] = f"{rel}:{line}:{col}"
            expected[name.string].append((str(path.relative_to(repo)), line, col, KINDS[keyword.string]))
        if ast_names != sites:
            raise ValueError(f"CPython AST/token anchor mismatch in {path}: {ast_names - sites}, {sites - ast_names}")
        candidates.update(re.findall(r"\b(?:def|class)\s+([^\W\d]\w*)", source))
    return expected, sorted(candidates | set(expected)), direct_calls(modules, declarations)


def verify_oracle():
    # A LOAD_GLOBAL elsewhere on the same line cannot establish a lambda parameter's identity.
    sources = {
        "local.py": "def emit(): pass\ndef mixed(): return (lambda emit: emit())(lambda: 0) or emit()\n",
        "provider.py": "def changed(): pass\nchanged = print\ndef stable(): pass\n",
        "consumer.py": "from provider import changed, stable\ndef run():\n    changed()\n    stable()\n",
    }
    modules = {path: (ast.parse(src), src) for path, src in sources.items()}
    decls = {("local.py", "emit"): "local.py:1:5", ("provider.py", "changed"): "provider.py:1:5",
             ("provider.py", "stable"): "provider.py:3:5"}
    if direct_calls(modules, decls) != [("import", "consumer.py:4:5", "provider.py:3:5")]:
        raise RuntimeError("independent binding oracle admitted a shadowed or rebound name")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-python-oracle.json")
    args = parser.parse_args()
    verify_oracle()
    spec = json.loads((FIXTURES / "python/public-django/public.json").read_text())
    results = {}
    with tempfile.TemporaryDirectory(prefix="cx-ast-") as tmp:
        tmp = Path(tmp)
        upstream = tmp / "upstream"
        upstream.mkdir()
        checkout(spec, upstream, ROOT / "target/conformance-cache")
        for package in PACKAGES:
            repo = tmp / package.replace("/", "-")
            home = tmp / (repo.name + "-home")
            home.mkdir()
            for original in (upstream / package).rglob("*.py"):
                dest = repo / original.relative_to(upstream)
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(original, dest)
            git("init", "-q", cwd=repo)
            expected, names, calls = collect(repo, package)
            files = len(list((repo / package).rglob("*.py")))
            declarations = sum(map(len, expected.values()))
            kinds = Counter(kind for kind, _, _ in calls)
            observed = (files, declarations, kinds["local"], kinds["import"])
            if observed != PACKAGES[package]:
                raise RuntimeError(f"{package}: CPython sample changed from {PACKAGES[package]} to {observed}")
            mismatches = []
            for name in names:
                proc = subprocess.run([ROOT / "target/release/cx", "--symbol", name, "--json"], cwd=repo,
                                      capture_output=True, text=True, timeout=90, env={**os.environ,
                                      "HOME": str(home), "GIT_CONFIG_GLOBAL": "./.missing-config", "GIT_CONFIG_NOSYSTEM": "1"})
                payload = json.loads(proc.stdout)
                got = [(h["path"], h["line"], h["col"], h["kind"]) for h in payload["hits"]]
                # Scope contains Python sources only. Other formats' symbols must not enter the oracle.
                want = sorted(expected.get(name, []))
                if sorted(got) != want[:50] or proc.returncode != int(not want) or payload["truncated"] != (len(want) > 50):
                    mismatches.append(dict(name=name, expected=want[:50], actual=got))
            binding_diffs = []
            for kind, site, target in calls:
                diff = check(ROOT / "target/release/cx", repo, home, "definition", site, target, "resolved", False, "oracle")
                if diff:
                    binding_diffs.append(dict(kind=kind, site=site, target=target, diff=diff))
            print(package, "direct calls:", dict(Counter(k for k, _, _ in calls)), "binding differences:", len(binding_diffs), flush=True)
            results[package] = dict(call_queries=dict(kinds), binding_diffs=binding_diffs, files=files,
                                    declarations=declarations, names=len(names), mismatches=mismatches)
            print(package, results[package]["files"], "files,", len(names), "queries,", len(mismatches), "mismatches", flush=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(dict(python=sys.version.split()[0], commit=spec["commit"], packages=results), indent=2) + "\n")
    return int(any(v["mismatches"] or v["binding_diffs"] for v in results.values()))


if __name__ == "__main__":
    raise SystemExit(main())
