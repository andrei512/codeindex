"""Development controls for conditional expectations and assertion guards."""

import argparse
import json
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory


def cases():
    # Each translation has the same two independent decisions. A fixed expected
    # count, a single decision, and overwriting the accumulator are controls.
    for label, path, unit, init, branch, read in [
        (
            "Python",
            "test_rules.py",
            "def test_sample():\n{}",
            "total = 0",
            "if {}:\n    total += 1",
            "assert total == {}",
        ),
        (
            "Go",
            "example_test.go",
            'package sample\nimport "testing"\nfunc TestSample(t *testing.T) {{\n{}\n}}',
            "total := 0",
            "if {} {{ total++ }}",
            'if total != {} {{ t.Errorf("count differs") }}',
        ),
        (
            "C++",
            "test/sample.cc",
            "#include <gtest/gtest.h>\nTEST(Sample, Count) {{\n{}\n}}",
            "int total = 0;",
            "if ({}) {{ total++; }}",
            "EXPECT_EQ(total, {});",
        ),
        (
            "Swift",
            "SampleTests.swift",
            "import XCTest\nclass SampleTests: XCTestCase {{\nfunc testSample() {{\n{}\n}}\n}}",
            "var total = 0",
            "if {} {{ total += 1 }}",
            "XCTAssertEqual(total, {})",
        ),
        (
            "JS",
            "sample.test.js",
            "import {{test,expect}} from 'vitest';\ntest('sample', () => {{{}\n}});",
            "let total = 0;",
            "if ({}) {{ total++; }}",
            "expect(total).toEqual({});",
        ),
        (
            "Java",
            "SampleTest.java",
            "import org.junit.jupiter.api.*;\nclass SampleTest {{\n@Test void sample() {{\n{}\n}}\n}}",
            "int total = 0;",
            "if ({}) {{ total++; }}",
            "Assertions.assertEquals(total, {});",
        ),
    ]:
        for scenario, want in [
            ("assembled", 1),
            ("fixed", 0),
            ("one", 0),
            ("overwritten", 0),
        ]:
            lines = [init, branch.format("first")]
            if scenario != "one":
                lines.append(branch.format("second"))
            if scenario == "overwritten":
                lines.append("total = 0" + (";" if label in ("Java", "C++") else ""))
            lines.append(read.format("2" if scenario == "fixed" else "actual"))
            yield (
                label + " " + scenario,
                path,
                unit.format(
                    "\n".join("    " + s.replace("\n", "\n    ") for s in lines)
                ),
                "conditional test expectation",
                want,
            )
    yield (
        "value used only for diagnostic",
        "test_rules.py",
        """def test_sample():
    total = 0
    if first:
        total += 1
    if second:
        total += 1
    assert actual == 2, total
""",
        "conditional test expectation",
        0,
    )
    for condition, second, want in [
        ("format", "items", 1),
        ("format", "other", 0),
        ("items", "items", 0),
    ]:
        yield (
            "indexed " + condition + " " + second,
            "test/sample.cc",
            """
#include <gtest/gtest.h>
TEST(Sample, Encoding) {
    for (int i=0; i<count; ++i) {
        if (format[i]=='X') { EXPECT_GT(items[i], 0); }
        else { EXPECT_EQ(0, ARRAY[i]); }
    }
}
""".replace("ARRAY", second).replace("format[i]", condition + "[i]"),
            "conditional test expectation",
            want,
        )
    for separate, want in [(False, 0), (True, 1)]:
        yield (
            "alternatives " + str(separate),
            "SampleTests.swift",
            """
import XCTest
class SampleTests: XCTestCase {
    func testFlags() {
        if enabled {
            if cached { useCached() } else { refresh() }
            EXTRA
        }
    }
}""".replace("EXTRA", "if ready { deliver() }" if separate else ""),
            "nested regions",
            want,
        )
    for body, want in [
        ('throw "unexpected"', 0),
        ('update()\n        throw "unexpected"', 1),
    ]:
        yield (
            "guard " + str(want),
            "Sample.Tests.ps1",
            f"""
Describe "sample" {{
  It "checks input" {{
    if ($a -or $b -or $c) {{
        {body}
    }}
  }}
}}""",
            "compound conditions",
            want,
        )
    for sibling, want in [(True, 0), (False, 1)]:
        unit = 'It "many" {\n' + ("if ($flag) { Write-Output 1 }\n" * 9) + "}"
        if sibling:
            unit = "\n".join(
                'It "case %d" { if ($flag) { Write-Output 1 } }' % n for n in range(9)
            )
        yield (
            "Pester ownership " + str(sibling),
            "Sample.Tests.ps1",
            'Describe "sample" {\n' + unit + "\n}",
            "branch complexity",
            want,
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/cx"))
    args = parser.parse_args()
    with TemporaryDirectory() as tmp:
        root = Path(tmp)
        labels = {}
        for n, (label, path, source, rule, want) in enumerate(cases()):
            rel = f"{n}/{path}"
            dest = root / rel
            dest.parent.mkdir(parents=True)
            dest.write_text(source)
            labels[rel] = (label, rule, want)
        rows = json.loads(
            subprocess.check_output(
                [args.binary.resolve(), "--score", "--json"], cwd=root
            )
        )["files"]
        failures = []
        for row in rows:
            label, rule, want = labels.pop(row["path"])
            got = sum(f["rule"] == rule for f in row["findings"])
            if row["score"] is None or got != want:
                failures.append((label, got, want))
        print(f"{len(rows)} files, {len(failures)} failed", failures)
        if labels or failures:
            raise SystemExit(1)


if __name__ == "__main__":
    main()
