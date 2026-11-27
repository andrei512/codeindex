"""Framework templates used to generate isolated nesting controls."""
from dataclasses import dataclass
from itertools import product
import json
from pathlib import Path


BODY = "%"


def wrap(template, body):
    if template.count(BODY) != 1:
        raise ValueError(f"expected one body placeholder in nesting template: {template!r}")
    prefix, suffix = template.split(BODY)
    indent = prefix.rsplit("\n", 1)[-1]
    return prefix + body.replace("\n", "\n" + indent) + suffix


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

    def variants(self):
        scenarios = ["quiet", "siblings", "test"]
        if self.setup:
            scenarios.extend(("setup", "independent"))
        for scenario, depth in product(scenarios, (0, 4)):
            yield scenario, depth, self.source(depth, scenario)

    def test_body(self, scenario):
        if scenario in ("quiet", "siblings"):
            return wrap(self.test, self.action)
        if scenario == "independent":
            region = wrap(self.branch, wrap(self.branch, self.action))
            return wrap(self.test, region) + "\n" + wrap(self.setup, region)

        # Two separate deep regions qualify for both published findings.
        region = self.action
        for _ in range(4):
            region = wrap(self.branch, region)
        logic = region + "\n" + region
        if scenario == "setup":
            return wrap(self.setup, logic) + "\n" + wrap(self.test, self.action)
        return wrap(self.test, logic)

    def source(self, depth, scenario):
        leaf = self.test_body(scenario)
        for n in range(depth):
            leaf = wrap(self.wrapper.replace("GROUP", f"Group{n}"), leaf)
        if scenario == "siblings":
            leaf = "\n".join(
                wrap(self.wrapper.replace("GROUP", label), leaf)
                for label in ("Left", "Right")
            )
        return wrap(self.outer, leaf) + "\n"


BRACED_BRANCH = 'if (true) {\n  %\n}'


def suite_callbacks():
    common = ("describe", "it", "beforeEach")
    runners = [
        (framework, f"import {{describe,it,beforeEach}} from '{module}';", *common)
        for framework, module in (("Jest", "@jest/globals"), ("Vitest", "vitest"),
                                  ("Mocha", "mocha"), ("Bun", "bun:test"), ("Node test suites", "node:test"))
    ]
    runners.extend((
        ("Playwright", "import {test} from '@playwright/test';", "test.describe", "test", "test.beforeEach"),
        ("Jasmine", "", *common),
        ("Cypress", "", *common),
    ))
    for framework, imp, group, test, hook in runners:
        for ext in ("js", "ts"):
            yield Case(
                framework + " " + ext,
                "sample.test." + ext,
                imp + "\n%",
                group + "('GROUP', () => {\n  %\n});",
                test + "('example', () => {\n  %\n});",
                hook + "(() => {\n  %\n});",
                BRACED_BRANCH,
                "void 0;",
            )


def script_suites():
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
            BRACED_BRANCH,
            "expect(true, isTrue);",
        )


def catalog_cases(*frameworks):
    for framework in frameworks:
        yield Case(framework=framework, **TEMPLATES[framework])


TEMPLATES = json.loads(Path(__file__).with_name("templates.json").read_text())


def native_suites():
    yield from catalog_cases(
        'Go testing', 'pytest', 'unittest subTest', 'JUnit Jupiter', 'Kotest DescribeSpec',
        'Swift Testing', 'XCTest activities', 'Rust test modules', 'NUnit'
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
    yield from catalog_cases(
        'Boost.Test', 'Pest'
    )


def async_subtests():
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
                BRACED_BRANCH,
                "void 0;",
            )
        yield Case(
            "AVA attempts " + ext,
            "sample.test." + ext,
            "import test from 'ava';\ntest('root', async t => {\n  %\n});",
            "(await t.try(async t => {\n  %\n})).commit();",
            "%",
            "",
            BRACED_BRANCH,
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


def framework_fixtures():
    yield from catalog_cases(
        'Ginkgo', 'JUnit 4 Enclosed', 'XCTest Objective-C activities'
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
            BRACED_BRANCH,
            "System.GC.KeepAlive(true);",
        )
    yield from catalog_cases(
        'GoogleTest namespaces', 'Web Platform Tests steps', 'NixOS Python subtests',
        'ShellSpec', 'Test2::V0'
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
            BRACED_BRANCH,
            "check(true)",
        )
    yield from catalog_cases(
        'JUnit Jupiter Kotlin'
    )


def cases():
    yield from suite_callbacks()
    yield from script_suites()
    yield from native_suites()
    yield from async_subtests()
    yield from framework_fixtures()
