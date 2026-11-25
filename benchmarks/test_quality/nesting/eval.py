"""Generate isolated dev controls. No repository input or network access."""

import argparse
import json
import os
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path

BODY = "%"


def wrap(template, body):
    return template.replace(
        BODY, body.replace("\n", "\n" + template.split(BODY)[0].rsplit("\n", 1)[-1])
    )


@dataclass
class Case:
    framework: str
    path: str
    outer: str
    wrapper: str
    test: str
    setup: str
    branch: str
    action: str

    def source(self, depth, scenario):
        logic = self.action
        if scenario not in ("quiet", "siblings"):
            # Two separate deep regions qualify for both published findings.
            for _ in range(4):
                logic = wrap(self.branch, logic)
            logic += "\n" + logic
        leaf = wrap(self.test, logic if scenario != "setup" else self.action)
        if scenario == "setup":
            leaf = wrap(self.setup, logic) + "\n" + leaf
        if scenario == "independent":
            one_region = wrap(self.branch, wrap(self.branch, self.action))
            leaf = wrap(self.test, one_region) + "\n" + wrap(self.setup, one_region)
        for n in range(depth):
            leaf = wrap(self.wrapper.replace("GROUP", f"Group{n}"), leaf)
        if scenario == "siblings":
            leaf = "\n".join(
                wrap(self.wrapper.replace("GROUP", label), leaf)
                for label in ("Left", "Right")
            )
        return wrap(self.outer, leaf) + "\n"


def cases():
    js_branch = "if (true) {\n  %\n}"
    for framework, imp, group, test, hook in [
        (
            "Jest",
            "import {describe,it,beforeEach} from '@jest/globals';",
            "describe",
            "it",
            "beforeEach",
        ),
        (
            "Vitest",
            "import {describe,it,beforeEach} from 'vitest';",
            "describe",
            "it",
            "beforeEach",
        ),
        (
            "Mocha",
            "import {describe,it,beforeEach} from 'mocha';",
            "describe",
            "it",
            "beforeEach",
        ),
        (
            "Bun",
            "import {describe,it,beforeEach} from 'bun:test';",
            "describe",
            "it",
            "beforeEach",
        ),
        (
            "Node test suites",
            "import {describe,it,beforeEach} from 'node:test';",
            "describe",
            "it",
            "beforeEach",
        ),
        (
            "Playwright",
            "import {test} from '@playwright/test';",
            "test.describe",
            "test",
            "test.beforeEach",
        ),
        ("Jasmine", "", "describe", "it", "beforeEach"),
        ("Cypress", "", "describe", "it", "beforeEach"),
    ]:
        for ext in ("js", "ts"):
            yield Case(
                framework + " " + ext,
                "sample.test." + ext,
                imp + "\n%",
                group + "('GROUP', () => {\n  %\n});",
                test + "('example', () => {\n  %\n});",
                hook + "(() => {\n  %\n});",
                js_branch,
                "void 0;",
            )
    yield Case(
        "Pester",
        "sample.Tests.ps1",
        'Describe "suite" {\n  %\n}',
        'Context "GROUP" {\n  %\n}',
        'It "leaf" {\n  %\n}',
        "BeforeEach {\n  %\n}",
        "if ($true) {\n  %\n}",
        "1 | Should -Be 1",
    )
    yield Case(
        "Test::More",
        "t/sample.t",
        "use Test::More;\n%\ndone_testing;",
        "subtest 'GROUP' => sub {\n  %\n};",
        "subtest 'leaf' => sub {\n  %\n};",
        "",
        "if (1) {\n  %\n}",
        "ok(1);",
    )
    for f, imp in [
        ("RSpec", "require 'rspec'"),
        ("Minitest Spec", "require 'minitest/autorun'"),
    ]:
        yield Case(
            f,
            "test/sample_spec.rb",
            imp + "\ndescribe 'suite' do\n  %\nend",
            "describe 'GROUP' do\n  %\nend",
            "it 'leaf' do\n  %\nend",
            "before do\n  %\nend",
            "if true\n  %\nend",
            "raise unless true",
        )
    yield Case(
        "Busted",
        "spec/sample_spec.lua",
        "%",
        'describe("GROUP", function()\n  %\nend)',
        'it("leaf", function()\n  %\nend)',
        "before_each(function()\n  %\nend)",
        "if true then\n  %\nend",
        "assert(true)",
    )
    for f, pkg in [
        ("Dart test", "test/test.dart"),
        ("Flutter test", "flutter_test/flutter_test.dart"),
    ]:
        yield Case(
            f,
            "test/sample_test.dart",
            "import 'package:" + pkg + "';\nvoid main() {\n  %\n}",
            "group('GROUP', () {\n  %\n});",
            "test('leaf', () {\n  %\n});",
            "setUp(() {\n  %\n});",
            js_branch,
            "expect(true, isTrue);",
        )
    yield Case(
        "Go testing",
        "sample_test.go",
        'package sample\nimport "testing"\nfunc TestSuite(t *testing.T) {\n  %\n}',
        't.Run("GROUP", func(t *testing.T) {\n  %\n})',
        't.Run("leaf", func(t *testing.T) {\n  %\n})',
        "t.Cleanup(func() {\n  %\n})",
        "if true {\n  %\n}",
        't.Log("ok")',
    )
    yield Case(
        "pytest",
        "test_example.py",
        "%",
        "class TestGROUP:\n    %",
        "def test_leaf(*args):\n    %",
        "def setup_method(*args):\n    %",
        "if True:\n    %",
        "assert True",
    )
    yield Case(
        "unittest subTest",
        "test_example.py",
        "import unittest\nclass Example(unittest.TestCase):\n    def test_leaf(self):\n        %",
        'with self.subTest(group="GROUP"):\n    %',
        "%",
        "",
        "if True:\n    %",
        "self.assertTrue(True)",
    )
    yield Case(
        "JUnit Jupiter",
        "ExampleTest.java",
        "import org.junit.jupiter.api.*;\nclass ExampleTest {\n  %\n}",
        "@Nested class GROUP {\n  %\n}",
        "@Test void leaf() {\n  %\n}",
        "@BeforeEach void setup() {\n  %\n}",
        js_branch,
        "Assertions.assertTrue(true);",
    )
    yield Case(
        "Kotest DescribeSpec",
        "ExampleTest.kt",
        "import io.kotest.core.spec.style.DescribeSpec\nclass ExampleTest : DescribeSpec({\n  %\n})",
        'describe("GROUP") {\n  %\n}',
        'it("leaf") {\n  %\n}',
        "beforeTest {\n  %\n}",
        js_branch,
        "check(true)",
    )
    yield Case(
        "Swift Testing",
        "SampleTests.swift",
        "import Testing\n@Suite struct Root {\n  %\n}",
        "@Suite struct GROUP {\n  %\n}",
        "@Test func leaf() {\n  %\n}",
        "init() {\n  %\n}",
        "if true {\n  %\n}",
        "#expect(true)",
    )
    yield Case(
        "XCTest activities",
        "SampleTests.swift",
        "import XCTest\nclass Root: XCTestCase {\n  func testLeaf() {\n    %\n  }\n}",
        'XCTContext.runActivity(named: "GROUP") { _ in\n  %\n}',
        "%",
        "",
        "if true {\n  %\n}",
        "XCTAssertTrue(true)",
    )
    yield Case(
        "Rust test modules",
        "src/lib.rs",
        "#[cfg(test)]\nmod tests {\n    %\n}",
        "mod GROUP {\n    %\n}",
        "#[test]\nfn leaf() {\n    %\n}",
        "",
        "if true {\n    %\n}",
        "assert!(true);",
    )
    yield Case(
        "NUnit",
        "TestSuite.cs",
        "using NUnit.Framework;\n[TestFixture] public class Root {\n  %\n}",
        "[TestFixture] public class GROUP {\n  %\n}",
        "[Test] public void Leaf() {\n  %\n}",
        "[SetUp] public void Setup() {\n  %\n}",
        js_branch,
        "Assert.That(true);",
    )
    for f, include, tc, group in [
        ("Catch2", "catch2/catch_test_macros.hpp", "TEST_CASE", "SECTION"),
        ("doctest", "doctest/doctest.h", "TEST_CASE", "SUBCASE"),
    ]:
        yield Case(
            f,
            "test/sample.cpp",
            "#include <" + include + ">\n" + tc + '("suite") {\n  %\n}',
            group + '("GROUP") {\n  %\n}',
            "%",
            "",
            "if (true) {\n  %\n}",
            "CHECK(true);",
        )
    yield Case(
        "Boost.Test",
        "test/sample.cpp",
        "#include <boost/test/unit_test.hpp>\n%",
        "BOOST_AUTO_TEST_SUITE(GROUP)\n%\nBOOST_AUTO_TEST_SUITE_END()",
        "BOOST_AUTO_TEST_CASE(leaf) {\n  %\n}",
        "",
        js_branch,
        "BOOST_TEST(true);",
    )
    yield Case(
        "Pest",
        "tests/SampleTest.php",
        "<?php\n%",
        "describe('GROUP', function () {\n  %\n});",
        "it('leaf', function () {\n  %\n});",
        "beforeEach(function () {\n  %\n});",
        js_branch,
        "expect(true)->toBeTrue();",
    )

    for ext in ("js", "ts"):
        for framework, imp, entry, child in [
            ("Node async subtests", "import test from 'node:test';", "test", "t.test"),
            ("Deno steps", "", "Deno.test", "t.step"),
        ]:
            yield Case(
                framework + " " + ext,
                "sample.test." + ext,
                imp + "\n" + entry + "('root', async t => {\n  %\n});",
                "await " + child + "('GROUP', async t => {\n  %\n});",
                "%",
                "",
                js_branch,
                "void 0;",
            )
        yield Case(
            "AVA attempts " + ext,
            "sample.test." + ext,
            "import test from 'ava';\ntest('root', async t => {\n  %\n});",
            "(await t.try(async t => {\n  %\n})).commit();",
            "%",
            "",
            js_branch,
            "t.pass();",
        )
    for runner in ("tokio", "async_std", "actix_rt"):
        yield Case(
            runner,
            "src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    %\n}",
            "mod GROUP {\n    %\n}",
            "#[" + runner + "::test]\nasync fn leaf() {\n    %\n}",
            "",
            "if true {\n    %\n}",
            "assert!(true);",
        )
    yield Case(
        "Ginkgo",
        "sample_test.go",
        'package sample\nimport . "github.com/onsi/ginkgo/v2"\nvar _ = Describe("suite", func() {\n  %\n})',
        'Context("GROUP", func() {\n  %\n})',
        'It("leaf", func() {\n  %\n})',
        "BeforeEach(func() {\n  %\n})",
        "if true {\n  %\n}",
        'println("ok")',
    )
    yield Case(
        "JUnit 4 Enclosed",
        "ExampleTest.java",
        "import org.junit.*;\nimport org.junit.runner.RunWith;\nimport org.junit.experimental.runners.Enclosed;\n@RunWith(Enclosed.class) public class ExampleTest {\n %\n}",
        "@RunWith(Enclosed.class) public static class GROUP {\n %\n}",
        "public static class Leaf {\n @Test public void leaf() {\n  %\n }\n}",
        "",
        js_branch,
        "Assert.assertTrue(true);",
    )
    yield Case(
        "XCTest Objective-C activities",
        "SampleTests.m",
        "#import <XCTest/XCTest.h>\n@interface SampleTests : XCTestCase\n@end\n@implementation SampleTests\n- (void)testLeaf {\n %\n}\n@end",
        '[XCTContext runActivityNamed:@"GROUP" block:^(id<XCTActivity> activity) {\n %\n}];',
        "%",
        "",
        js_branch,
        "XCTAssertTrue(YES);",
    )
    for f, imports, attr in [
        ("xUnit.net", "using Xunit;", "[Fact]"),
        (
            "MSTest",
            "using Microsoft.VisualStudio.TestTools.UnitTesting;",
            "[TestMethod]",
        ),
    ]:
        cls = "[TestClass] " if f == "MSTest" else ""
        yield Case(
            f,
            "Tests/SampleTests.cs",
            imports + "\npublic class Root {\n %\n}",
            "public class GROUP {\n %\n}",
            cls
            + "public class Fixture {\n "
            + attr
            + " public void Leaf() {\n  %\n }\n}",
            "",
            js_branch,
            "System.GC.KeepAlive(true);",
        )
    yield Case(
        "GoogleTest namespaces",
        "tests/sample.cpp",
        "#include <gtest/gtest.h>\n%",
        "namespace GROUP {\n %\n}",
        "TEST(Example, Leaf) {\n %\n}",
        "",
        js_branch,
        "EXPECT_TRUE(true);",
    )

    yield Case(
        "Web Platform Tests steps",
        "tests/sample.html",
        '<script src="/resources/testharness.js"></script>\n<script>\nasync_test(t => {\n %\n t.done();\n});\n</script>',
        "t.step(() => {\n %\n});",
        "%",
        "",
        js_branch,
        "assert_true(true);",
    )
    yield Case(
        "NixOS Python subtests",
        "tests/sample.nix",
        "{ pkgs }: pkgs.testers.runNixOSTest {\n name = \"sample\";\n nodes.machine = {};\n testScript = ''\n  %\n '';\n}",
        'with subtest("GROUP"):\n    %',
        "%",
        "",
        "if True:\n    %",
        "assert True",
    )
    yield Case(
        "ShellSpec",
        "spec/sample_spec.sh",
        'Describe "root"\n %\nEnd',
        'Context "GROUP"\n %\nEnd',
        'It "leaf"\n %\n When call echo "$input"\n The output should eq 1\nEnd',
        "",
        "if true; then\n %\nfi",
        "input=1",
    )

    yield Case(
        "Test2::V0",
        "t/sample.t",
        "use Test2::V0;\n%\ndone_testing;",
        "subtest 'GROUP' => sub {\n  %\n};",
        "subtest 'leaf' => sub {\n  %\n};",
        "",
        "if (1) {\n  %\n}",
        "ok(1);",
    )
    for style, group, leaf in [
        ("FunSpec", "context", "test"),
        ("FreeSpec", "", ""),
        ("ShouldSpec", "context", "should"),
    ]:
        wrapper = (
            '"GROUP" - {\n %\n}'
            if style == "FreeSpec"
            else group + '("GROUP") {\n %\n}'
        )
        test = '"leaf" {\n %\n}' if style == "FreeSpec" else leaf + '("leaf") {\n %\n}'
        yield Case(
            "Kotest " + style,
            "ExampleTest.kt",
            "import io.kotest.core.spec.style."
            + style
            + "\nclass ExampleTest : "
            + style
            + "({\n %\n})",
            wrapper,
            test,
            "beforeTest {\n %\n}",
            js_branch,
            "check(true)",
        )
    yield Case(
        "JUnit Jupiter Kotlin",
        "ExampleTest.kt",
        "import org.junit.jupiter.api.*\nclass ExampleTest {\n %\n}",
        "@Nested inner class GROUP {\n %\n}",
        "@Test fun leaf() {\n %\n}",
        "@BeforeEach fun setup() {\n %\n}",
        js_branch,
        "check(true)",
    )


def run(binary, indexed):
    result = {}
    with tempfile.TemporaryDirectory(prefix="cx-nesting-dev-") as directory:
        root = Path(directory) / "sources"
        root.mkdir()
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        env.update(HOME=directory, XDG_CONFIG_HOME=directory)
        if indexed:
            subprocess.run(["git", "init", "-q"], cwd=root, env=env, check=True)
        keys = {}
        for n, c in enumerate(cases()):
            for scenario in ("quiet", "siblings", "test", "setup", "independent"):
                if scenario in ("setup", "independent") and not c.setup:
                    continue
                for level in (0, 4):
                    path = f"{n}/{scenario}/{level}/{c.path}"
                    dest = root / path
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    dest.write_text(c.source(level, scenario))
                    keys[path] = (c.framework, scenario, level)
        rows = json.loads(
            subprocess.check_output([binary, "--score", "--json"], cwd=root, env=env)
        )["files"]
        for row in rows:
            f, s, d = keys.pop(row["path"])
            result.setdefault(f, {}).setdefault(s, {})[d] = (
                None
                if row["score"] is None
                else sorted(
                    (h["rule"], h["value"])
                    for h in row["findings"]
                    if h["rule"] in ("nesting depth", "nested regions")
                )
            )
        if keys:
            raise ValueError("missing results: " + str(keys))
    return result


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, default=Path("target/release/cx"))
    p.add_argument("--output", type=Path, required=True)
    p.add_argument(
        "--report-only",
        action="store_true",
        help="Record known baseline failures without exiting nonzero",
    )
    p.add_argument(
        "--indexed",
        action="store_true",
        help="Use a temporary Git repository and isolated index",
    )
    args = p.parse_args()
    results = run(args.binary.resolve(), args.indexed)
    failures = []
    for f, scenarios in results.items():
        for s, levels in scenarios.items():
            expected = (
                []
                if s in ("quiet", "siblings", "independent")
                else [("nested regions", 2), ("nesting depth", 4)]
            )
            if any(v != expected for v in levels.values()):
                failures.append((f, s, levels))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(dict(results=results, failures=failures), indent=2) + "\n"
    )
    print(
        len(results), "framework/dialect combinations,", len(failures), "failed pairs"
    )
    for fail in failures:
        print(fail)
    if failures and not args.report_only:
        raise SystemExit(1)
