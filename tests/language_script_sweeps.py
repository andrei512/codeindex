"""Run independent discovery waves against script language name handling.

The expected sites come from labels in the test input, before cx is run. Two
independent declarations (alpha and omega) make a wrong binding observable.
Starlark checks declarations and explicit unsupported status. The module holdout
separates supported cross-file lookup from exploratory probes for local-only
profiles. PowerShell 7.4.6 runs the three holdout commands as module/local/local;
MRI runs both singleton methods across the reopened class. Batch chaining is
specified in Microsoft's cmd and call command references.
"""
import argparse
import json
import os
from pathlib import Path
import re
import tempfile

from language_conformance import ROOT, check, git


# The delimiter «tag» marks the start of the following source token.
# The first field of each tuple is the filename. The template has one literal
# statement slot inside a caller, plus one top-level data declaration slot.
TEMPLATES = {
    "js": ("main.mjs", "$data\nexport function «a»alpha() {}\nexport function «b»omega() {}\nexport function invoke() { $body }\n", "«u»alpha();", "const pattern = /function bait\\(\\)/;", '"🌱"; ', "export function «d»alpha() {}"),
    "python": ("main.py", "$data\ndef «a»alpha(): pass\ndef «b»omega(): pass\ndef invoke():\n    $body\n", "«u»alpha()", 'pattern = f"def bait(): {{}}"', '"🌱"; ', "def «d»alpha(): pass"),
    "starlark": ("main.bzl", "$data\ndef «a»alpha():\n    pass\ndef «b»omega():\n    pass\ndef invoke():\n    $body\n", "«u»alpha()", 'pattern = """def bait():\n    pass"""', 'text = "🌱"; ', "def «d»alpha():\n    pass"),
    "shell": ("main.bash", "$data\n«a»alpha() { :; }\n«b»omega() { :; }\ninvoke() { $body; }\n", "«u»alpha", "pattern='x\nbait() { :; }\ny'", "printf '%s' '🌱' >/dev/null; ", "«d»alpha() { :; }"),
    "powershell": ("main.ps1", "$data\nfunction «a»alpha { return 1 }\nfunction «b»omega { return 2 }\nfunction Invoke-Example { $body }\n", "«u»alpha", "$pattern = @'\nfunction bait { echo 1 }\n'@", "$s = '🌱'; ", "function «d»alpha { return 3 }"),
    "php": ("main.php", "<?php\nnamespace Worker;\n$data\nfunction «a»alpha() {}\nfunction «b»omega() {}\nfunction invoke() { $body }\n", "«u»alpha();", "$pattern = 'function bait() {}';", "$s = '🌱'; ", "<?php\nnamespace Decoy;\nfunction «d»alpha() {}"),
    "hack": ("main.hack", "<?hh\n$data\nfunction «a»alpha(): void {}\nfunction «b»omega(): void {}\nfunction invoke(): void { $body }\n", "«u»alpha();", "const string PATTERN = 'function bait(): void {}';", "$s = '🌱'; ", "<?hh\nclass Decoy { public static function «d»alpha(): void {} }"),
    "ruby": ("main.rb", "$data\nclass Worker\n  def self.«a»alpha; end\n  def self.«b»omega; end\n  def self.invoke\n    $body\n  end\nend\n", "Worker.«u»alpha()", "pattern = %q{def bait; end}", "s = '🌱'; ", "class Decoy\n  def self.«d»alpha; end\nend"),
    "lua": ("main.lua", "$data\nlocal function «a»alpha() end\nlocal function «b»omega() end\nlocal function invoke()\n  $body\nend\n", "«u»alpha()", "local pattern = [==[function bait() end]==]", "local s = '🌱'; ", "local function «d»alpha() end"),
    "perl": ("main.pl", "use strict;\npackage Worker;\n$data\nsub «a»alpha {}\nsub «b»omega {}\nsub invoke { $body }\n", "Worker::«u»alpha();", "my $pattern = q{sub bait {}};", "my $s = '🌱'; ", "package Decoy;\nsub «d»alpha {}"),
    "batchfile": ("main.cmd", "@echo off\n$data\ngoto :invoke\n:«a»alpha\nexit /b\n:«b»omega\nexit /b\n:invoke\n$body\nexit /b\n", "call :«u»alpha", "echo :bait", "echo 🌱 & ", ":«d»alpha\nexit /b"),
}


def write_source(repo, path, source):
    output = []
    sites = {}
    for line, text in enumerate(source.splitlines(), 1):
        clean = ""
        end = 0
        for marker in re.finditer("«([a-z]+)»", text):
            clean += text[end:marker.start()]
            col = 1 + len(clean.encode("utf-16-le")) // 2
            sites[marker[1]] = f"{path}:{line}:{col}"
            end = marker.end()
        output.append(clean + text[end:])
    (repo / path).write_text("\n".join(output) + "\n", encoding="utf-8")
    return sites


def run(binary, root, language, wave):
    filename, template, call, data, prefix, decoy = TEMPLATES[language]
    repo, home = root / "repo", root / "home"
    repo.mkdir()
    home.mkdir()
    git("init", "-q", cwd=repo)
    body = call
    if wave == "same_line":
        separator = " & " if language == "batchfile" else "; "
        body = prefix + call.rstrip(";") + separator + call.replace("«u»", "«v»")
    if wave == "guarded_call":
        guarded = {
            "js": "if (true) { $call }",
            "python": "if True:\n        $call",
            "starlark": "if True:\n        $call",
            "shell": "if true; then $call; fi",
            "powershell": "if ($true) { $call }",
            "php": "if (true) { $call }",
            "hack": "if (true) { $call }",
            "ruby": "if true\n      $call\n    end",
            "lua": "if true then $call end",
            "perl": "if (1) { $call }",
            "batchfile": "if 1==1 $call",
        }
        body = guarded[language].replace("$call", call)
    source = template.replace("$body", body).replace("$data", data if wave == "quoted_data" else "")
    if wave == "caller_partition":
        # Append a second caller of a different target in the same lexical unit.
        secondary = {
            "js": "export function probe() { $call }",
            "python": "def probe(): $call",
            "starlark": "def «probe»probe():\n    $call",
            "shell": "probe() { $call; }",
            "powershell": "function probe { $call }",
            "php": "function probe() { $call }",
            "hack": "function probe(): void { $call }",
            "ruby": "class Worker\n  def self.probe; $call; end\nend",
            "lua": "local function probe() $call end",
            "perl": "sub probe { $call }",
            "batchfile": ":probe\n$call\nexit /b",
        }[language]
        source += secondary.replace("$call", call.replace("«u»alpha", "«w»omega")) + "\n"
    sites = write_source(repo, filename, source)
    if wave == "unloaded_file":
        other = "decoy" + Path(filename).suffix
        sites.update(write_source(repo, other, decoy))
    records = []

    def assert_query(op, subject, expected, phase, status=None):
        if status is None:
            status = "" if op == "symbol" else "resolved"
        diff = check(binary, repo, home, op, subject, expected, status, False, phase)
        records.append(dict(op=op, subject=subject, expected=expected, status=status, phase=phase, diff=diff))

    def verify(active, phase):
        for name, label in (("alpha", "a"), ("omega", "b")):
            anchors = [sites[label]]
            if name == "alpha" and "d" in sites:
                anchors.append(sites["d"])
            assert_query("symbol", name, ",".join(anchors), phase)
        if wave == "quoted_data":
            assert_query("symbol", "bait", "-", phase)
        if language == "starlark":
            if wave == "caller_partition":
                assert_query("symbol", "probe", sites["probe"], phase)
                assert_query("callers", sites["a"], "-", phase, "unsupported")
            return
        uses = [sites[k] for k in ("u", "v") if k in sites]
        for use in uses:
            assert_query("definition", use, sites[active], phase)
        for label in ("a", "b", "d"):
            if label in sites:
                hits = ",".join(uses) if label == active else "-"
                if wave == "caller_partition" and label == "b":
                    hits = sites["w"]
                assert_query("references", sites[label], hits, phase)
        if wave in ("guarded_call", "caller_partition"):
            first_owner = "Invoke-Example" if language == "powershell" else "invoke"
            assert_query("callers", sites["a"], sites["u"] + "|" + first_owner, phase)
            if wave == "caller_partition":
                assert_query("callers", sites["b"], sites["w"] + "|probe", phase)

    verify("a", "before")
    if wave == "preserved_mtime":
        path = repo / filename
        original = path.stat()
        if language == "starlark":
            # The profile publishes names only. Rename a declaration and its use.
            sites = write_source(repo, filename, source.replace("alpha", "delta"))
            os.utime(path, ns=(original.st_atime_ns, original.st_mtime_ns))
            assert_query("symbol", "alpha", "-", "after")
            assert_query("symbol", "delta", sites["a"], "after")
        else:
            sites = write_source(repo, filename, source.replace("«u»alpha", "«u»omega"))
            os.utime(path, ns=(original.st_atime_ns, original.st_mtime_ns))
            verify("b", "after")
        if path.stat().st_size != original.st_size or path.stat().st_mtime_ns != original.st_mtime_ns:
            raise ValueError("mtime or size unexpectedly changed")
    if wave == "definition_rename":
        # A caller remains byte-for-byte unchanged across a declaration rename.
        # During the gap the unrelated omega declaration must gain no incoming use.
        # After the rename back the original edge must return at the same site.
        write_source(repo, filename, source.replace("«a»alpha", "«a»delta"))
        assert_query("symbol", "delta", sites["a"], "renamed")
        assert_query("symbol", "alpha", "-", "renamed")
        if language != "starlark":
            assert_query("definition", sites["u"], "-", "renamed", "unresolved")
            assert_query("references", sites["b"], "-", "renamed")
            assert_query("references", sites["a"], "-", "renamed")
        write_source(repo, filename, source)
        verify("a", "restored")
    return records


# Each pair exports an identically spelled declaration. The main file selects a
# module explicitly. 'probe' records a true cross-file binding outside the
# documented local-only profile. Starlark promises declarations and Batchfile
# promises labels local to each script.
MODULES = {
    "js": ("mjs", "export function «target»alpha() { return '$module'; }",
           "import * as lib from './$module.mjs';\nlib.«u»alpha();", "required"),
    "python": ("py", "def «target»alpha(): return '$module'",
               "import $module as lib\nlib.«u»alpha()", "required"),
    "shell": ("bash", "«target»alpha() { echo '$module'; }",
              "source ./$module.bash\n«u»alpha", "required"),
    "powershell": ("psm1", "function «target»alpha { return '$module' }",
                   "Import-Module ./$module.psm1\nfunction «own»alpha { return 'local' }\n$module\\«u»alpha (1)\n«direct»alpha (1)\n«folded»ALPHA (1)", "required"),
    "starlark": ("bzl", "def «target»alpha():\n    return '$module'",
                 "load(':$module.bzl', run = 'alpha')\nrun()", "declarations"),
    "php": ("php", "<?php\nnamespace $module;\nfunction «target»alpha() { return '$module'; }",
            "<?php\nrequire './$module.php';\nuse function $module\\alpha as selected;\n«u»selected();", "probe"),
    "hack": ("hack", "<?hh\nnamespace $module;\nfunction «target»alpha(): string { return '$module'; }",
             "<?hh\nuse function $module\\alpha as selected;\nfunction run(): string { return «u»selected(); }", "probe"),
    "ruby": ("rb", "class $module\n  def self.«target»alpha; '$module'; end\nend",
             "require_relative '$module'\n$module.«u»alpha()", "probe"),
    "lua": ("lua", "local M = {}\nfunction M.«target»alpha() return '$module' end\nreturn M",
            "local lib = require('$module')\nlib.«u»alpha()", "probe"),
    "perl": ("pm", "package $module;\nsub «target»alpha { '$module' }\n1;",
             "use lib '.';\nuse $module ();\n${module}::«u»alpha();", "probe"),
    "batchfile": ("cmd", "@echo off\ncall :«local»alpha\nexit /b\n:«target»alpha\necho $module\nexit /b",
                  "@echo off\ncall $module.cmd\nexit /b", "labels"),
}


def module_holdout(binary, root, language):
    repo, home = root / "repo", root / "home"
    repo.mkdir()
    home.mkdir()
    git("init", "-q", cwd=repo)
    ext, module, main, scope = MODULES[language]
    filename = {"powershell": "main.ps1", "perl": "main.pl"}.get(language, "main." + ext)
    anchors = {}
    paths = {name: name + "." + ext for name in ("Left", "Right")}
    for name, path in paths.items():
        anchors[name] = write_source(repo, path, module.replace("$module", name))
    result = {}

    for wave, active in (("loaded_namespace", "Left"), ("redirect_namespace", "Right"),
                         ("missing_dependency", "Right"), ("restored_dependency", "Right")):
        if wave in ("loaded_namespace", "redirect_namespace"):
            source = main.replace("${module}", active).replace("$module", active)
            position = write_source(repo, filename, source)
        elif wave == "missing_dependency":
            (repo / paths[active]).unlink()
        else:
            write_source(repo, paths[active], module.replace("$module", active))
        present = [name for name in paths if wave != "missing_dependency" or name != active]
        rows = []
        result[wave] = rows

        def expect(op, subject, answer, status="", member=False, probe=False):
            diff = check(binary, repo, home, op, subject, answer, status, member, wave)
            rows.append(dict(op=op, subject=subject, expected=answer, phase=wave,
                             status=status, subset=member, probe=probe, diff=diff))

        declarations = [anchors[name]["target"] for name in present]
        if language == "powershell":
            declarations.append(position["own"])
            expect("definition", position["direct"], position["own"], "resolved")
            expect("definition", position["folded"], position["own"], "resolved")
            expect("references", position["own"], position["direct"] + "," + position["folded"], "resolved")
        expect("symbol", "alpha", ",".join(declarations))
        if scope == "declarations":
            expect("callers", anchors[present[0]]["target"], "-", "unsupported")
        elif scope == "labels":
            for name in present:
                expect("definition", anchors[name]["local"], anchors[name]["target"], "resolved")
                expect("references", anchors[name]["target"], anchors[name]["local"], "resolved")
        else:
            target = anchors[active]["target"] if active in present else "-"
            expected_status = "resolved" if active in present else ("unresolved" if scope == "required" else "")
            expect("definition", position["u"], target, expected_status,
                   probe=scope == "probe")
            if active in present:
                expect("references", target, position["u"], "resolved", member=True,
                       probe=scope == "probe")
            # An unrelated declaration cannot acquire the call when the actual
            # module disappears. Assert this from every reachable decoy.
            other = "Right" if active == "Left" else "Left"
            expect("callers", anchors[other]["target"], "-", "resolved")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-script-sweeps.json")
    args = parser.parse_args()
    result = {}
    for wave in ("quoted_data", "unloaded_file", "same_line", "preserved_mtime", "caller_partition", "guarded_call", "definition_rename"):
        samples = {}
        for language in TEMPLATES:
            with tempfile.TemporaryDirectory(prefix="cx-script-sweep-") as tmp:
                samples[language] = run(args.binary.resolve(), Path(tmp), language, wave)
        result[wave] = samples
        failed = [(lang, row) for lang, rows in samples.items() for row in rows if row["diff"]]
        print(wave, sum(map(len, samples.values())), "checks;", len(failed), "mismatches")
        for lang, row in failed:
            print(" ", lang, row["phase"], row["op"], row["subject"], row["diff"])
    for language in MODULES:
        with tempfile.TemporaryDirectory(prefix="cx-module-sweep-") as tmp:
            for wave, rows in module_holdout(args.binary.resolve(), Path(tmp), language).items():
                result.setdefault(wave, {})[language] = rows
    for wave in ("loaded_namespace", "redirect_namespace", "missing_dependency", "restored_dependency"):
        samples = result[wave]
        print(wave, sum(map(len, samples.values())), "checks")
        for language, rows in samples.items():
            for row in rows:
                if row["diff"]:
                    print(" ", language, "probe" if row.get("probe") else "required",
                          row["op"], row["subject"], row["diff"])
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=2) + "\n")
    return int(any(row["diff"] and not row.get("probe") for samples in result.values() for rows in samples.values() for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
