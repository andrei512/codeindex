# /// script
# requires-python = ">=3.9"
# dependencies = ["libclang==18.1.1"]
# ///
"""Check native cx against independently selected Clang AST locations in real code.

Run: uv run tests/language_compiled_oracle.py
The compiler and its system headers must be installed. Only named definitions and
direct calls with an unambiguous, source-spelled, same-file definition qualify.
No checkout is built or executed. An invalid translation unit is unverified.
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

from language_reverse_witness import reverse_witness
from clang import cindex
from language_conformance import ROOT, checkout, check, compare, git


# Profiles and units chosen before querying cx. Consecutive files are holdouts.
PROJECTS = {
    "log": ("https://github.com/rxi/log.c", "f9ea34994bd58ed342d2245cd4110bb5c6790153",
            ["src/log.c"], []),
    "nanopb": ("https://github.com/nanopb/nanopb", "2de40e37c89e5bef88f92639e309860f0239ac26",
               ["pb_common.c", "pb_encode.c", "pb_decode.c"], []),
    "robin": ("https://github.com/martinus/robin-hood-hashing", "7697343363af4cc3f42cab17be49e6af9ab181e2",
              ["src/test/app/fmt/mup.cpp", "src/test/app/fmt/streamstate.cpp", "src/test/app/benchmark.cpp"],
              ["src/test", "src/include"]),
    "cjson": ("https://github.com/DaveGamble/cJSON.git", "acc76239bee01d8e9c858ae2cab296704e52d916",
              ["cJSON.c", "cJSON_Utils.c"], []),
    "kostya": ("https://github.com/kostya/benchmarks.git", "5bc68b3705fddd0fdb16b8748c4f2c73e1032a43",
               ["primes/primes.cpp", "brainfuck/bf.cpp"], ["common/libnotify"]),
    "sds": ("https://github.com/antirez/sds.git", "5347739b1581fcba74fd5cab1fc21d2aef317d71",
            ["sds.c"], []),
    "zlib": ("https://github.com/madler/zlib.git", "51b7f2abdade71cd9bb0e7a373ef2610ec6f9daf",
             ["adler32.c", "trees.c", "inftrees.c", "zutil.c"], []),
    "fmt": ("https://github.com/fmtlib/fmt.git", "40626af88bd7df9a5fb80be7b25ac85b122d6c21",
            ["src/os.cc"], ["include"]),
    "tinyexpr": ("https://github.com/codeplea/tinyexpr.git", "ba2e32bbcc2d1e644a7e132d24c2f542821cabe0",
                 ["tinyexpr.c", "smoke.c"], []),
    "inih": ("https://github.com/benhoyt/inih.git", "5cc5e2c24642513aaa5b19126aad42d0e4e0923e",
             ["ini.c", "cpp/INIReader.cpp"], ["cpp"]),
    "linenoise": ("https://github.com/antirez/linenoise.git", "a473823d74b93eab2ba83480df16ed37617493f2",
                  ["linenoise.c"], []),
    "hashmap": ("https://github.com/tidwall/hashmap.c.git", "ad4dfbc9e13ba3beb8863b9cbd511f0f7e517723",
                ["hashmap.c"], []),
    "lodepng": ("https://github.com/lvandeve/lodepng.git", "7b2893e35891c0c85bee5595caafaf3c2044b409",
                ["lodepng.cpp"], []),

    "double-conversion": ("https://github.com/google/double-conversion.git", "ae0dbfeb9744efd216c95b30555049d75d47116a",
                          ["double-conversion/bignum.cc", "double-conversion/cached-powers.cc", "double-conversion/fast-dtoa.cc"], []),
}


def run(*args, **kwargs):
    return subprocess.run(args, text=True, capture_output=True, check=True, **kwargs)


def compiler(lang):
    driver = shutil.which("clang++" if lang == "c++" else "clang")
    if not driver:
        raise RuntimeError("install Clang and system headers for " + lang)
    search = run(driver, "-E", "-x", lang, "-v", "-", input="").stderr
    paths = search.split("#include <...> search starts here:", 1)[1].split("End of search list.", 1)[0]
    args = ["-x", lang, "-std=c++17" if lang == "c++" else "-std=c11"]
    for directory in paths.strip().splitlines():
        directory = directory.strip()
        if directory.endswith(" (framework directory)"):
            args.extend(["-F", directory.removesuffix(" (framework directory)")])
        else:
            args.extend(["-isystem", directory])
    if sys.platform == "darwin":
        args.extend(["-isysroot", run("xcrun", "--show-sdk-path").stdout.strip()])
    return args


def source_sites(unit, location, source, path):
    """Accept a token only when its spelling matches literal bytes at Clang's offset."""
    name = unit.spelling
    if not (location.file and Path(str(location.file)) == path and re.fullmatch(r"[A-Za-z_]\w*", name)):
        return None
    offset = location.offset
    if source[offset:offset + len(name)] != name.encode():
        return None
    end = offset + len(name)
    if re.match(rb"\w", source[end:end + 1]):
        return None
    start = source.rfind(b"\n", 0, offset) + 1
    col = len(source[start:offset].decode().encode("utf-16-le")) // 2 + 1
    return f"{location.line}:{col}"


def collect(path, flags):
    tu = cindex.Index.create().parse(str(path), args=flags)
    errors = [str(d) for d in tu.diagnostics if d.severity >= cindex.Diagnostic.Error]
    if errors:
        raise RuntimeError("\n".join(errors))
    k = cindex.CursorKind
    routines = {k.FUNCTION_DECL, k.CXX_METHOD}
    source = path.read_bytes()
    defs, calls = set(), set()
    stack = list(reversed(list(tu.cursor.get_children())))
    while stack:
        node = stack.pop()
        if not node.location.file or Path(str(node.location.file)) != path:
            continue
        children = list(node.get_children())
        stack.extend(reversed(children))
        if node.kind in routines and node.is_definition():
            target = source_sites(node, node.location, source, path)
            if target:
                defs.add((node.spelling, target))
        if node.kind != k.CALL_EXPR or not children:
            continue
        # Only descend through the callee expression, never call arguments.
        target = node.referenced
        if not target or target.kind not in routines:
            continue
        definition = target.get_definition()
        if not definition or target.location != definition.location:
            continue
        decl = source_sites(definition, definition.location, source, path)
        if not decl:
            continue
        for expr in children[0].walk_preorder():
            if expr.kind not in (k.DECL_REF_EXPR, k.MEMBER_REF_EXPR) or expr.referenced != target:
                continue
            site = source_sites(expr, expr.location, source, path)
            if site:
                calls.add((expr.spelling, site, decl))
    return sorted(defs), sorted(calls)


def sample(rows, cap=24):
    if len(rows) <= cap:
        return rows
    return [rows[i * (len(rows) - 1) // (cap - 1)] for i in range(cap)]


def symbol_probe(binary, repo, home, name, expected, cache):
    if name not in cache:
        result = subprocess.run([binary, "--symbol", name, "--json"], cwd=repo, capture_output=True,
                                text=True, timeout=120, env={**os.environ, "HOME": str(home),
                                "GIT_CONFIG_GLOBAL": "./.missing-config", "GIT_CONFIG_NOSYSTEM": "1"})
        if result.returncode not in (0, 1):
            raise RuntimeError(result.stderr)
        cache[name] = json.loads(result.stdout)
    answer = cache[name]
    diff = compare(answer, "symbol", expected, "", True)
    if diff and answer.get("truncated"):
        return {}, "missing from truncated output"
    return diff, None


def replay_control(repo, path, flags, defs, calls, rows, binary, home, control):
    """Compare an isolated source change after Clang confirms all measured sites agree."""
    before = path.read_text()
    if control == "digit_separators":
        after, count = re.subn(r"(?<=\d)'(?=\d)", "", before)
    else:
        defines = run("clang++", *flags, "-dM", "-E", str(path)).stdout
        known = set(re.findall(r"^#define (LODEPNG_COMPILE_\w+)\b", defines, re.M))
        def replacement(m):
            enabled = (m[2] in known) == (m[1] == "ifdef")
            return "#if " + str(int(enabled))
        after, count = re.subn(r"^[ \t]*#[ \t]*(ifdef|ifndef)[ \t]+(LODEPNG_COMPILE_\w+)\b",
                               replacement, before, flags=re.M)
    original_refs = set(calls)
    path.write_text(after)
    after_defs, after_calls = collect(path, flags)
    if set(defs) != set(after_defs) or original_refs != set(after_calls):
        return {"unverified": "Clang declaration or binding locations changed"}
    cache = {}
    restored = []
    unresolved = []
    for row in rows:
        if not row["diff"] or row["excluded"]:
            continue
        if row["op"] == "symbol":
            diff, excluded = symbol_probe(binary, repo, home, row["name"], row["expected"], cache)
        else:
            diff = check(binary, repo, home, "definition", row["subject"], row["expected"], "resolved", False, "fresh")
            excluded = False
        (unresolved if diff or excluded else restored).append(row["subject"])
    return {"changed_tokens_or_guards": count, "recovered": restored, "remaining": unresolved}


def write_compile_commands(repo, commands):
    path = repo / "compile_commands.json"
    path.write_text(json.dumps(commands))
    # Some upstreams ignore generated build files. Both participants must see
    # the exact same flags. Only the disposable checkout's index is changed.
    git("add", "--force", "--intent-to-add", "--", path.name, cwd=repo)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", choices=PROJECTS, action="append")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-compiled-oracle.json")
    args = parser.parse_args()
    if sys.platform == "darwin":
        # Match the host SDK headers with the Apple frontend, retaining pinned bindings.
        tool = Path(run("xcrun", "--find", "clang").stdout.strip())
        cindex.Config.set_library_file(str(tool.parent.parent / "lib/libclang.dylib"))
    results = {}
    binary = ROOT / "target/release/cx"
    for project in args.project or PROJECTS:
        url, commit, files, includes = PROJECTS[project]
        with tempfile.TemporaryDirectory() as tmp:
            repo, home = Path(tmp) / "repo", Path(tmp) / "home"
            repo.mkdir()
            home.mkdir()
            checkout({"url": url, "commit": commit}, repo, ROOT / "target/conformance-cache")
            flags_by_file = {}
            commands = []
            for filename in files:
                lang = "c" if filename.endswith(".c") else "c++"
                flags = compiler(lang) + (["-std=c++20"] if project == "kostya" else []) + ["-I" + str(repo / d) for d in includes]
                flags_by_file[filename] = flags
                commands.append({"directory": str(repo), "file": filename, "arguments":
                                 ["clang" if lang == "c" else "clang++", *flags, "-c", filename]})
            # Supply exactly the compiler context used for the oracle to cx as well.
            write_compile_commands(repo, commands)
            symbol_cache = {}
            for filename in files:
                path = repo / filename
                entry = results[f"{project}/{filename}"] = {"url": url, "commit": commit, "rows": []}
                try:
                    entry["arguments"] = [v.replace(str(repo), "<checkout>") for v in flags_by_file[filename]]
                    defs, calls = collect(path, flags_by_file[filename])
                except (RuntimeError, cindex.TranslationUnitLoadError) as error:
                    entry["unverified"] = str(error)
                    print(filename, "unverified", error, flush=True)
                    continue
                entry["eligible"] = {"symbol": len(defs), "definition": len(calls)}
                queries = [("symbol", n, n, f"{filename}:{loc}") for n, loc in sample(defs)]
                queries += [("definition", n, f"{filename}:{site}", f"{filename}:{decl}")
                            for n, site, decl in sample(calls)]
                for op, name, subject, expected in queries:
                    excluded = None
                    if op == "symbol":
                        diff, excluded = symbol_probe(binary, repo, home, name, expected, symbol_cache)
                    else:
                        diff = check(binary, repo, home, op, subject, expected, "resolved", False, "fresh")
                    entry["rows"].append(dict(op=op, name=name, subject=subject, expected=expected, diff=diff, excluded=excluded))
                entry["reverse"] = reverse_witness(binary, repo, home, filename, calls, entry["rows"])
                control = {"lodepng.cpp": "compile_guards", "primes/primes.cpp": "digit_separators"}.get(filename)
                if control:
                    entry[control] = replay_control(repo, path, flags_by_file[filename], defs, calls,
                                                    entry["rows"], binary, home, control)
                print(filename, entry["eligible"], Counter(r["op"] for r in entry["rows"] if r["diff"]), flush=True)
    args.report.parent.mkdir(exist_ok=True, parents=True)
    args.report.write_text(json.dumps({"driver": run("clang", "--version").stdout.splitlines()[0],
                                      "units": results}, indent=2) + "\n")
    return int(any(v.get("unverified") or any(r["diff"] for r in v["rows"])
                   or any(r["wrong"] or r["missing"] for r in v.get("reverse", [])) for v in results.values()))


if __name__ == "__main__":
    raise SystemExit(main())
