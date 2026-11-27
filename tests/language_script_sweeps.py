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
from dataclasses import dataclass
import json
import os
from pathlib import Path
import tempfile

from language_conformance import ROOT, QuerySession
from language_script_fixture import Phase, checkout, write_source
from language_script_module_sweeps import module_results


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


def wave_source(language, wave):
    _, template, call, data, prefix, _ = TEMPLATES[language]
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
    return source


@dataclass
class Wave:
    language: str
    name: str
    phase: Phase
    sites: dict

    def verify(self, active):
        sites = self.sites
        alpha = [sites["a"], sites["d"]] if "d" in sites else [sites["a"]]
        self.phase.record("symbol", "alpha", ",".join(alpha))
        self.phase.record("symbol", "omega", sites["b"])
        if self.name == "quoted_data":
            self.phase.record("symbol", "bait", "-")
        if self.language != "starlark":
            self.relationships(active)
        elif self.name == "caller_partition":
            self.phase.record("symbol", "probe", sites["probe"])
            self.phase.record("callers", sites["a"], "-", "unsupported")

    def relationships(self, active):
        sites = self.sites
        uses = [sites[k] for k in ("u", "v") if k in sites]
        for use in uses:
            self.phase.record("definition", use, sites[active])
        incoming = {active: ",".join(uses)}
        if self.name == "caller_partition":
            incoming["b"] = sites["w"]
        for label in ("a", "b", "d"):
            if label in sites:
                self.phase.record("references", sites[label], incoming.get(label, "-"))
        if self.name not in ("guarded_call", "caller_partition"):
            return
        first_owner = "Invoke-Example" if self.language == "powershell" else "invoke"
        self.phase.record("callers", sites["a"], sites["u"] + "|" + first_owner)
        if self.name == "caller_partition":
            self.phase.record("callers", sites["b"], sites["w"] + "|probe")


def rewrite_same_stamp(path, source, original, rename):
    old, new = ("alpha", "delta") if rename else ("«u»alpha", "«u»omega")
    if old not in source:
        raise ValueError(f"missing edit target in {path.name}: {old!r}")
    before = path.read_bytes()
    sites = write_source(path.parent, path.name, source.replace(old, new))
    if path.read_bytes() == before:
        raise ValueError(f"same-stamp intervention left {path.name} unchanged")
    os.utime(path, ns=(original.st_atime_ns, original.st_mtime_ns))
    updated = path.stat()
    if updated.st_size != original.st_size or updated.st_mtime_ns != original.st_mtime_ns:
        raise ValueError("mtime or size unexpectedly changed")
    return sites


def include_decoy(repo, language, filename, sites):
    decoy = TEMPLATES[language][-1]
    other = "decoy" + Path(filename).suffix
    extra = write_source(repo, other, decoy)
    if duplicates := sites.keys() & extra.keys():
        raise ValueError(f"duplicate labels across {filename} and {other}: {sorted(duplicates)}")
    sites.update(extra)


def replay_preserved_mtime(case, path, source, phase):
    original = path.stat()
    rename = case.language == "starlark"
    sites = case.sites = rewrite_same_stamp(path, source, original, rename)
    case.phase = phase("after")
    if rename:
        case.phase.record("symbol", "alpha", "-")
        case.phase.record("symbol", "delta", sites["a"])
    else:
        case.verify("b")


def replay_definition_rename(case, path, source, phase):
    # Keep the caller unchanged while the declaration edge disappears and returns.
    sites = case.sites
    write_source(path.parent, path.name, source.replace("«a»alpha", "«a»delta"))
    case.phase = phase("renamed")
    case.phase.record("symbol", "delta", sites["a"])
    case.phase.record("symbol", "alpha", "-")
    if case.language != "starlark":
        case.phase.record("definition", sites["u"], "-", "unresolved")
        case.phase.record("references", sites["b"], "-")
        case.phase.record("references", sites["a"], "-")
    write_source(path.parent, path.name, source)
    case.phase = phase("restored")
    case.verify("a")


def run(binary, root, language, wave):
    filename = TEMPLATES[language][0]
    repo, home = checkout(root)
    source = wave_source(language, wave)
    sites = write_source(repo, filename, source)
    if wave == "unloaded_file":
        include_decoy(repo, language, filename, sites)
    records = []

    def phase(name):
        return Phase(QuerySession(binary, repo, home), records, name)

    case = Wave(language, wave, phase("before"), sites)
    case.verify("a")
    if wave == "preserved_mtime":
        replay_preserved_mtime(case, repo / filename, source, phase)
    if wave == "definition_rename":
        replay_definition_rename(case, repo / filename, source, phase)
    return records


def report_sweep(wave, samples, module=False):
    failed = [(lang, row) for lang, rows in samples.items() for row in rows if row["diff"]]
    count = sum(map(len, samples.values()))
    if module:
        print(wave, count, "checks")
    else:
        print(wave, count, "checks;", len(failed), "mismatches")
    for lang, row in failed:
        phase = ("probe" if row.get("probe") else "required") if module else row["phase"]
        print(" ", lang, phase, row["op"], row["subject"], row["diff"])
    return any(not row.get("probe") for _, row in failed)


def script_results(binary, language, wave):
    with tempfile.TemporaryDirectory(prefix="cx-script-sweep-") as tmp:
        return run(binary, Path(tmp), language, wave)


def ordinary_waves(binary):
    for wave in ("quoted_data", "unloaded_file", "same_line", "preserved_mtime", "caller_partition", "guarded_call", "definition_rename"):
        samples = {}
        for language in TEMPLATES:
            samples[language] = script_results(binary, language, wave)
        yield wave, samples


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/cx")
    parser.add_argument("--report", type=Path, default=ROOT / "target/language-script-sweeps.json")
    args = parser.parse_args()
    result = {}
    failed = False
    binary = args.binary.resolve()
    for wave, samples in ordinary_waves(binary):
        result[wave] = samples
        failed |= report_sweep(wave, samples)
    for wave, language, rows in module_results(binary):
        result.setdefault(wave, {})[language] = rows
    for wave in ("loaded_namespace", "redirect_namespace", "missing_dependency", "restored_dependency"):
        failed |= report_sweep(wave, result[wave], module=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=2) + "\n")
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
