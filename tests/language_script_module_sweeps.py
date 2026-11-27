"""Independently labelled scenarios for script module and package relationships."""
from dataclasses import dataclass
from pathlib import Path
import tempfile

from language_conformance import QuerySession, expectation
from language_script_fixture import Phase, checkout, write_source


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
    repo, home = checkout(root)
    ext, module, main, scope = MODULES[language]
    filename = {"powershell": "main.ps1", "perl": "main.pl"}.get(language, "main." + ext)
    anchors = {}
    paths = {name: name + "." + ext for name in ("Left", "Right")}
    for name, path in paths.items():
        anchors[name] = write_source(repo, path, module.replace("$module", name))
    fixture = ModuleFixture(language, scope, anchors)
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
        rows = result[wave] = []
        phase = Phase(QuerySession(binary, repo, home), rows, wave)

        fixture.check(phase, position, active, missing=wave == "missing_dependency")
    return result


@dataclass(frozen=True)
class ModuleFixture:
    language: str
    scope: str
    anchors: dict

    def check(self, phase, position, active, *, missing):
        expect = phase.module
        present = [name for name in self.anchors if not missing or name != active]
        declarations = [self.anchors[name]["target"] for name in present]
        if self.language == "powershell":
            declarations.append(position["own"])
            expect(expectation("definition", position["direct"], position["own"], "resolved"))
            expect(expectation("definition", position["folded"], position["own"], "resolved"))
            expect(expectation("references", position["own"], position["direct"] + "," + position["folded"], "resolved"))
        expect(expectation("symbol", "alpha", ",".join(declarations), ""))
        if self.scope == "declarations":
            expect(expectation("callers", self.anchors[present[0]]["target"], "-", "unsupported"))
        elif self.scope == "labels":
            self.module_labels(expect, present)
        else:
            self.module_references(expect, position, active, active in present)

    def module_labels(self, expect, present):
        for name in present:
            anchor = self.anchors[name]
            expect(expectation("definition", anchor["local"], anchor["target"], "resolved"))
            expect(expectation("references", anchor["target"], anchor["local"], "resolved"))

    def module_references(self, expect, position, active, available):
        target = self.anchors[active]["target"] if available else "-"
        expected_status = "resolved" if available else ("unresolved" if self.scope == "required" else "")
        expect(expectation("definition", position["u"], target, expected_status), probe=self.scope == "probe")
        if available:
            expect(expectation("references", target, position["u"], "resolved"), member=True, probe=self.scope == "probe")
        # An unrelated declaration cannot acquire the call when the actual
        # module disappears. Assert this from every reachable decoy.
        for other in sorted(self.anchors.keys() - {active}):
            expect(expectation("callers", self.anchors[other]["target"], "-", "resolved"))

def module_results(binary):
    for language in MODULES:
        with tempfile.TemporaryDirectory(prefix="cx-module-sweep-") as tmp:
            for wave, rows in module_holdout(binary, Path(tmp), language).items():
                yield wave, language, rows
