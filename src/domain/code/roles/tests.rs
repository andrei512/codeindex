use super::{Role::*, *};

fn at(path: &str, text: &str, needle: &str) -> Role {
    let pos = text.find(needle).unwrap();
    let r = classify(path, text);
    r.regions
        .iter()
        .rev()
        .find(|r| r.start <= pos && pos < r.end)
        .map_or(r.base.0, |r| r.evidence.0)
}

#[test]
fn same_bytes_change_role_when_moved_and_ordinary_names_are_safe() {
    for filename in [
        "contest.rs",
        "contest.tsx",
        "contest.py",
        "contest.bzl",
        "contest.sh",
        "contest.ps1",
        "Contest.cs",
        "Contest.swift",
        "contest.c",
        "contest.cc",
        "contest.proto",
        "contest.css",
        "contest.html",
        "contest.sql",
        "contest.go",
        "Contest.java",
        "Contest.kt",
        "Contest.m",
        "contest.dart",
        "contest.php",
        "contest.hack",
        "Dockerfile",
        "Makefile",
        "contest.cmd",
        "CMakeLists.txt",
        "Procfile",
        "contest.tf",
        "contest.nix",
        "contest.rb",
        "contest.lua",
        "contest.pl",
        "contest.S",
        "contest.mako",
        "contest.pgsql",
        "README.md",
    ] {
        let src = format!("src/{filename}");
        let source = "ordinary";
        assert_eq!(at(&src, source, source), Production, "{src}");
        assert_eq!(
            at(&format!("tests/fixtures/{filename}"), source, source),
            Support,
            "{src}"
        );
        assert_eq!(
            at(&format!("examples/{filename}"), source, source),
            Unknown,
            "{src}"
        );
    }
}

#[test]
fn rust_module_attributes_are_nested_and_exclude_strings_and_comments() {
    let text = "const NOTE: &str = \"#[test] fn pretend() {}\";\n// #[cfg(test)]\nfn actual() {}\n#[cfg(test)]\nmod checks {\n fn helper() {}\n #[test]\n fn roundtrip() { actual(); }\n}\nfn after() {}";
    for (needle, role) in [
        ("pretend", Production),
        ("fn actual", Production),
        ("helper", Support),
        ("actual();", Test),
        ("after", Production),
    ] {
        assert_eq!(at("src/lib.rs", text, needle), role, "{needle}");
    }
}

#[test]
fn registrations_overrides_and_lookalikes() {
    let cases = [
        (
            "pkg/thing_test.go",
            "func TestRead(t *testing.T) { exercise() }",
            "exercise",
            Test,
        ),
        (
            "pkg/thing_test.go",
            "func TestRead(x string) { fixture() }",
            "fixture",
            Support,
        ),
        (
            "src/thing.go",
            "func TestRead(t *testing.T) { exercise() }",
            "exercise",
            Production,
        ),
        (
            "src/thing.ts",
            "import { test } from 'node:test';\ntest('a', () => exercise());\nfunction app() {}",
            "exercise",
            Test,
        ),
        (
            "src/thing.ts",
            "const words = \"test('a', run)\"; function it(label, run) {}\nit('a', run);",
            "it('a'",
            Production,
        ),
        (
            "tests/test_api.py",
            "@pytest.fixture\ndef setup():\n    return make()\n\ndef test_api():\n    exercise()\n",
            "make",
            Support,
        ),
        (
            "tests/test_api.py",
            "def test_api():\n    exercise()\n\ndef helper():\n    build()",
            "exercise",
            Test,
        ),
        (
            "tests/test_api.py",
            "def test_api():\n    exercise()\n\ndef helper():\n    build()",
            "build",
            Support,
        ),
        (
            "src/Foo.java",
            "import org.junit.Test;\nclass Foo { @Test public void run() { exercise(); } void helper() {} }",
            "exercise",
            Test,
        ),
        (
            "src/Foo.java",
            "class Foo { String x = \"@Test\"; void run() { exercise(); } }",
            "exercise",
            Production,
        ),
        (
            "tests/x.cpp",
            "TEST_F(Case, A) { exercise(); }",
            "exercise",
            Test,
        ),
        (
            "tests/x.cpp",
            "#define TEST_F(x,y) impl()\nvoid helper() { build(); }",
            "build",
            Support,
        ),
        (
            "ci/check.bats",
            "@test \"runs\" {\n run cmd\n}",
            "cmd",
            Test,
        ),
        (
            "infra/x.tftest.hcl",
            "run \"empty\" { command = plan }",
            "plan",
            Test,
        ),
        (
            "pkg/CMakeLists.txt",
            "add_test(NAME smoke COMMAND demo)",
            "demo",
            Test,
        ),
        (
            "src/BUILD",
            "py_test(name = 'unit', srcs = ['x.py'])",
            "unit",
            Test,
        ),
        ("tests/x.ps1", "It 'parses' { Check }", "Check", Test),
        (
            "src/guide.md",
            "A test named `test_api` runs here.",
            "test_api",
            Production,
        ),
    ];
    for (path, src, probe, want) in cases {
        assert_eq!(at(path, src, probe), want, "{path} {probe}");
    }
}

#[test]
fn framework_boundaries_and_locally_shadowed_macros() {
    let kt =
        "import kotlin.test.Test\n@Test fun smoke() = expect(42)\nfun runtimePath() { work() }";
    assert_eq!(at("src/Foo.kt", kt, "expect"), Test);
    assert_eq!(at("src/Foo.kt", kt, "runtimePath"), Production);
    let c = "#define TEST(expr) if (!(expr))\nvoid reset() { TEST(alive) { restart(); } }";
    assert_eq!(at("src/service.cpp", c, "restart"), Production);
    let rs = "#[tokio::test(flavor = \"current_thread\")]\nasync fn check() { run(); }";
    assert_eq!(at("src/lib.rs", rs, "run();"), Test);
    let go = "func ExampleRead() { fmt.Println(42) }";
    assert_eq!(at("parse_test.go", go, "Println"), Support);
    let go = "func TestBad(t *testing.T, n int) { exercise() }";
    assert_eq!(at("parse_test.go", go, "exercise"), Support);
}

#[test]
fn headers_supply_wrappers_and_their_bytes_are_shared() {
    let mut headers = Context::default();
    let mut reads = 0;
    for path in ["tests/first.c", "tests/second.c"] {
        let result=classify_with(path,"#include \"api.h\"\nWRAPPED_CASE(ok) { run(); }", &mut headers, |h|{
            assert_eq!(h,std::path::Path::new("tests/api.h"));
            reads+=1;
            Ok(Some("#define WRAPPED_CASE(name) INTERMEDIATE(name)\n#define INTERMEDIATE(name) TEST_IMPL(name)\n".into()))
        }).unwrap();
        assert_eq!(result.regions.len(), 1);
        assert_eq!(result.regions[0].evidence.0, Test);
    }
    assert_eq!(reads, 1);
}

#[test]
fn fixture_paths_install_sources_and_script_discovery() {
    let mut ctx = Context::new(["install.sh"].into_iter());
    let install = "install -m 755 \"$ROOT/lib/runner\"/* \"$PREFIX/lib/runner\"\n";
    let mut reads = 0;
    for (path, expected) in [
        ("lib/runner/test_helpers.sh", Production),
        ("lib/other/test_helpers.sh", Support),
    ] {
        let roles = classify_with(path, "function helper() { :; }", &mut ctx, |path| {
            assert_eq!(path, std::path::Path::new("install.sh"));
            reads += 1;
            Ok(Some(install.into()))
        })
        .unwrap();
        assert_eq!(roles.base.0, expected);
    }
    assert_eq!(reads, 1);
    assert_eq!(at("fixtures/web/Procfile", "web: app", "app"), Support);
    assert_eq!(
        at("hack/docs.Dockerfile", "FROM alpine", "alpine"),
        Production
    );
    assert_eq!(
        at(
            "rpc/unit_testing/schema.proto",
            "package rpc.testing; message Input {}",
            "Input"
        ),
        Support
    );
    assert_eq!(
        at(
            "src/schema.proto",
            "package rpc.testing; message Input {}",
            "Input"
        ),
        Production
    );
    let src = "use Test::More;\nok(1, 'passes');\nsub setup { return 1; }";
    assert_eq!(at("t/check.t", src, "passes"), Test);
    assert_eq!(at("t/check.t", src, "setup"), Support);
    let src = "{ nmt.script = ''\nassertFileExists a;\nassertFileExists b\n''; value = true; }";
    assert_eq!(at("tests/foo.nix", src, "assertFileExists b"), Test);
    assert_eq!(at("tests/foo.nix", src, "value"), Support);
    let src = "<script src='/resources/testharness.js'></script>\n<script>async_test(function () { verify(); });</script>\n<div>ordinary</div>";
    assert_eq!(at("web/index.html", src, "verify"), Test);
    assert_eq!(at("web/index.html", src, "ordinary"), Production);
    let fake = "<!-- <script src='testharness.js'></script> --><script>async_test(() => verify());</script>";
    assert_eq!(at("web/index.html", fake, "verify"), Production);
}
