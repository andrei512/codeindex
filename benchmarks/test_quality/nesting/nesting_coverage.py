"""Expected findings and reviewed coverage for the nesting controls."""

# Reviewed framework coverage is independent of generated files and returned rows.
FRAMEWORKS_WITH_SETUP = {
    "Bun js", "Bun ts", "Busted", "Cypress js", "Cypress ts", "Dart test", "Flutter test",
    "Ginkgo", "Go testing", "JUnit Jupiter", "JUnit Jupiter Kotlin", "Jasmine js", "Jasmine ts",
    "Jest js", "Jest ts", "Kotest DescribeSpec", "Kotest FreeSpec", "Kotest FunSpec",
    "Kotest ShouldSpec", "Minitest Spec", "Mocha js", "Mocha ts", "NUnit",
    "Node test suites js", "Node test suites ts", "Pest", "Pester", "Playwright js",
    "Playwright ts", "RSpec", "Swift Testing", "Vitest js", "Vitest ts", "pytest",
}
FRAMEWORKS_WITHOUT_SETUP = {
    "AVA attempts js", "AVA attempts ts", "Boost.Test", "Catch2", "Deno steps js",
    "Deno steps ts", "GoogleTest namespaces", "JUnit 4 Enclosed", "MSTest",
    "NixOS Python subtests", "Node async subtests js", "Node async subtests ts",
    "Rust test modules", "ShellSpec", "Test2::V0", "Test::More", "Web Platform Tests steps",
    "XCTest Objective-C activities", "XCTest activities", "actix_rt", "async_std", "doctest",
    "tokio", "unittest subTest", "xUnit.net",
}


def framework_failures(framework, scenarios):
    required = {"quiet", "siblings", "test"}
    if framework in FRAMEWORKS_WITH_SETUP:
        required.update(("setup", "independent"))
    if scenarios.keys() != required:
        yield framework, "scenario coverage", sorted(required.symmetric_difference(scenarios))
    for scenario, levels in scenarios.items():
        expected = (
            []
            if scenario in ("quiet", "siblings", "independent")
            else [("nested regions", 2), ("nesting depth", 4)]
        )
        if levels.keys() != {0, 4} or any(value != expected for value in levels.values()):
            yield framework, scenario, levels


def find_failures(results):
    failures = []
    expected_frameworks = FRAMEWORKS_WITH_SETUP | FRAMEWORKS_WITHOUT_SETUP
    if expected_frameworks != results.keys():
        failures.append(("framework coverage", sorted(expected_frameworks.symmetric_difference(results))))
    for framework, scenarios in results.items():
        failures.extend(framework_failures(framework, scenarios))
    return failures

