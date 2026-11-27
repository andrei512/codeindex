use super::*;
use crate::domain::code::scored;
mod foreign_abi;

// Every parser profile gets a real syntax specimen and a measurable finding.
// Flat configuration formats use structural size; procedural formats use branches.
const BRANCH_FIXTURES: &[(&str, &str, &str, &str)] = &[
        ("x.rs", "fn run() {", "if ok { work(); }", "}"),
        ("x.js", "function run() {", "if (ok) work();", "}"),
        ("x.tsx", "const Run = () => {", "if (ok) work();", "}"),
        ("x.py", "def run():", "    if ok: work()", ""),
        ("x.bzl", "def run():", "    if ok: work()", ""),
        ("x.sh", "run() {", "if ok; then work; fi", "}"),
        ("x.ps1", "function Run {", "if ($ok) { Work }", "}"),
        ("x.cs", "class C { void Run() {", "if (ok) Work();", "} }"),
        ("x.swift", "func run() {", "if ok { work() }", "}"),
        ("x.c", "void run() {", "if (ok) work();", "}"),
        ("x.cpp", "void run() {", "if (ok) work();", "}"),
        (
            "x.go",
            "package main\nfunc run() {",
            "if ok { work() }",
            "}",
        ),
        ("x.java", "class C { void run() {", "if (ok) work();", "} }"),
        ("x.kt", "fun run() {", "if (ok) work()", "}"),
        ("x.m", "void run() {", "if (ok) work();", "}"),
        ("x.dart", "void run() {", "if (ok) work();", "}"),
        ("x.php", "<?php function run() {", "if ($ok) work();", "}"),
        (
            "x.hack",
            "<?hh function run(): void {",
            "if ($ok) work();",
            "}",
        ),
        ("x.bat", ":run", "IF EXIST input.txt ECHO found", "EXIT /B"),
        (
            "CMakeLists.txt",
            "function(run)",
            "if(OK)\nmessage(ok)\nendif()",
            "endfunction()",
        ),
        ("x.nix", "{ ok, work }:", "if ok then work else", "null"),
        ("x.rb", "def run", "  if ok then work end", "end"),
        ("x.lua", "function run()", "if ok then work() end", "end"),
        ("x.pl", "sub run {", "if ($ok) { work(); }", "}"),
        ("x.s", "run:", "jne run", "ret"),
        ("x.sql", "SELECT CASE", "WHEN x = 1 THEN 2", "END;"),
        (
            "x.plpgsql",
            "BEGIN",
            "IF ok THEN PERFORM work(); END IF;",
            "END;",
        ),
        (
            "x.mako",
            "<%def name=\"run()\">",
            "% if ok:\n work\n% endif",
            "</%def>",
        ),
    ];

#[test]
fn procedural_profiles_measure_branch_growth() {
    for &(path, head, line, end) in BRANCH_FIXTURES {
        let lang = Language::for_path(path).unwrap();
        let small = format!("{head}\n{line}\n{end}\n");
        let small = scored(lang, &small, path, None).1.unwrap();
        assert!(
            !small.findings.iter().any(|f| f.rule == Rule::Complexity),
            "{path}: {:?}",
            small.findings
        );
        let text = format!("{head}\n{}{end}\n", format!("{line}\n").repeat(10));
        let s = scored(Language::for_path(path).unwrap(), &text, path, None)
            .1
            .unwrap();
        let padded = format!("\n\n{text}\n\n");
        let padded = scored(lang, &padded, path, None).1.unwrap();
        assert_eq!(
            s.value(),
            padded.value(),
            "{path}: blank lines changed the score"
        );
        assert!(
            s.findings.iter().any(|f| f.rule == Rule::Complexity),
            "{path}: {:?}",
            s.findings
        );
    }
}

#[test]
fn structural_profiles_measure_size_growth() {
    for (path, head, line, end) in [
        ("x.html", "<main>", "<span>text</span>", "</main>"),
        ("x.css", ".rule {", " color: red;", "}"),
        ("x.proto", "message Payload {", " string field_N = N;", "}"),
        ("Dockerfile", "FROM scratch AS build", "RUN echo N", ""),
        ("Makefile", "all:", "\t@echo N", ""),
        ("Procfile", "worker: run \\", "  --argN \\", "  end"),
        ("x.tf", "locals {", " keyN = N", "}"),
    ] {
        let mut text = format!("{head}\n");
        for i in 1..=75 {
            text.push_str(&line.replace('N', &i.to_string()));
            text.push('\n');
        }
        text.push_str(end);
        let s = scored(Language::for_path(path).unwrap(), &text, path, None)
            .1
            .unwrap();
        assert!(
            s.findings.iter().any(|f| f.rule == Rule::Length),
            "{path}: {:?}",
            s.findings
        );
    }
}
#[test]
fn empirical_penalty_and_disjoint_regions() {
    let src = "def sample(a,b,c,d,e):\n    if a:\n        if b:\n            pass\n    if c:\n        if d:\n            pass\n";
    let s = scored(Language::Python, src, "x.py", None).1.unwrap();
    assert!(
        (s.value() - 9.536386775820924).abs() < 1e-10,
        "{:?}",
        s.findings
    );
}

#[test]
fn condition_reports_the_expression_line_and_exact_body_clones_need_two_units() {
    let s = scored(
        Language::Python,
        "def f(a,b,c):\n    if a and b or c:\n        return 1\n",
        "x.py",
        None,
    )
    .1
    .unwrap();
    assert!(
        s.findings
            .iter()
            .any(|f| f.rule == Rule::Conditionals && f.line == 2 && f.value == 2)
    );
    let body = (0..12)
        .map(|i| format!("    print({i})\n"))
        .collect::<String>();
    let text = format!("def foo():\n{body}def bar():\n{body}");
    let s = scored(Language::Python, &text, "x.py", None).1.unwrap();
    assert_eq!(
        s.findings
            .iter()
            .filter(|f| f.rule == Rule::Duplication)
            .count(),
        2
    );
}

#[test]
fn markdown_section_and_empty_coverage() {
    let text = format!("# Operations\n{}", "instruction\n".repeat(72));
    let score = markdown(
        &text,
        &crate::domain::markdown::Parsed::parse("doc.md", &text),
    )
    .unwrap();
    assert_eq!(score.lines, 73);
    assert!(
        score
            .findings
            .iter()
            .any(|f| f.unit == "Operations" && f.rule == Rule::Length)
    );
    assert_eq!(
        scored(Language::Python, "# comment\n", "x.py", None)
            .1
            .unwrap()
            .lines,
        0
    );
}

#[test]
fn python_continuation_indents_do_not_create_control_flow() {
    let src = "def f():\n    return (\n        [\n            (\n                [\n                    1\n                ]\n            )\n        ]\n    )\n";
    let score = scored(Language::Python, src, "x.py", None).1.unwrap();
    assert!(score.findings.is_empty(), "{:?}", score.findings);
}

#[test]
fn python_counts_short_circuits_comprehensions_and_else_suites() {
    let src = "def f(items):\n    for item in items:\n        if item and item.a or item.b:\n            yield [v for v in item]\n        else:\n            yield item.a or item.b or item.c or item.d or item.e\n";
    let score = scored(Language::Python, src, "x.py", None).1.unwrap();
    assert!(
        score
            .findings
            .iter()
            .any(|f| f.rule == Rule::Complexity && f.value == 11),
        "{:?}",
        score.findings
    );
}

#[test]
fn data_literals_are_not_control_flow() {
    for (lang, src) in [
        (
            Language::Go,
            "package p\nfunc f() { x := [][][][]int{{{{1}}}}; _ = x }",
        ),
        (
            Language::Js,
            "function f() { return {x:{y:{z:{a:{b:1}}}}}; }",
        ),
        (
            Language::Rust,
            "fn f() { let _ = A { b: B { c: C { d: D { x: 1 } } } }; }",
        ),
        (
            Language::Java,
            "class T { void f() { int[][][][] x = {{{{1}}}}; } }",
        ),
        (
            Language::Csharp,
            "class T { void F() { var x = new A { B = new B { C = new C { D = new D { X = 1 } } } }; } }",
        ),
        (Language::Cpp, "void f() { int x[1][1][1][1] = {{{{1}}}}; }"),
    ] {
        let score = scored(lang, src, "fixture", None).1.unwrap();
        assert!(
            !score
                .findings
                .iter()
                .any(|f| matches!(f.rule, Rule::Bumps | Rule::Nesting)),
            "{lang:?} {:?}",
            score.findings
        );
    }
}

#[test]
fn parenthesized_for_headers_and_wrapped_guards_retain_their_scope() {
    let src = "void f(int n) { for (int i=0;i<n;i++) { if (n) { if (i) { if (n &&\n i && n) { work(); } } } } }";
    let score = scored(Language::C, src, "x.c", None).1.unwrap();
    assert!(
        score
            .findings
            .iter()
            .any(|f| f.rule == Rule::Nesting && f.value == 4)
    );
    assert!(
        score
            .findings
            .iter()
            .any(|f| f.rule == Rule::Conditionals && f.value == 2 && f.line == 1)
    );
}

#[test]
fn independent_callbacks_do_not_accumulate_decisions_in_the_parent() {
    for (lang, src) in [
        (
            Language::Go,
            format!(
                "package p\nvar hooks=[]func(bool){{\n{}\n}}",
                "func(x bool) { if x { work() } },\n".repeat(12)
            ),
        ),
        (
            Language::Js,
            format!(
                "const hooks=[{}];",
                "function(x){ if(x){work();} },\n".repeat(12)
            ),
        ),
    ] {
        let score = scored(lang, &src, "fixture", None).1.unwrap();
        assert!(
            !score.findings.iter().any(|f| f.rule == Rule::Complexity),
            "{:?}",
            score.findings
        );
    }
}

#[test]
fn ruby_postfix_conditions_do_not_open_unclosed_suites() {
    let src = format!("def check(x)\n{}end\n", "  work if x\n".repeat(12));
    let score = scored(Language::Ruby, &src, "x.rb", None).1.unwrap();
    assert!(!score.findings.iter().any(|f| f.rule == Rule::Nesting));
    assert!(score.findings.iter().any(|f| f.rule == Rule::Complexity));
}

#[test]
fn starlark_and_python_share_control_semantics_for_their_common_syntax() {
    let src = "def f(items):\n    for x in items:\n        if (x.a and\n            x.b and x.c):\n            print([y for y in x])\n";
    let py = scored(Language::Python, src, "x.py", None).1.unwrap();
    let star = scored(Language::Starlark, src, "x.bzl", None).1.unwrap();
    assert_eq!(py.value(), star.value());
    for score in [py, star] {
        assert!(
            score
                .findings
                .iter()
                .any(|f| f.rule == Rule::Conditionals && f.line == 3 && f.value == 2)
        );
    }
}

#[test]
fn four_nested_suites_inside_named_ruby_and_lua_functions() {
    for (lang, head, branch) in [
        (Language::Ruby, "def check(x)", "if x"),
        (Language::Lua, "function check(x)", "if x then"),
    ] {
        let src = format!(
            "{head}\n{}work()\n{}",
            format!("{branch}\n").repeat(4),
            "end\n".repeat(5)
        );
        let score = scored(lang, &src, "fixture", None).1.unwrap();
        assert!(
            score
                .findings
                .iter()
                .any(|f| f.rule == Rule::Nesting && f.value == 4),
            "{lang:?} {:?}",
            score.findings
        );
    }
}

#[test]
fn parameter_count_excludes_only_the_method_receiver() {
    let src = "class Example:\n    def bound(self,a,b,c,d): pass\n    @staticmethod\n    def standalone(self,a,b,c,d): pass\ndef regular(self,a,b,c,d): pass\n";
    let score = scored(Language::Python, src, "x.py", None).1.unwrap();
    let units: Vec<_> = score
        .findings
        .iter()
        .filter(|f| f.rule == Rule::Arguments)
        .map(|f| f.unit.as_str())
        .collect();
    assert_eq!(units, ["standalone", "regular"]);
    let rust = "struct S; impl S { fn f(&mut self,a:i32,b:i32,c:i32,d:i32) {} }";
    let score = scored(Language::Rust, rust, "x.rs", None).1.unwrap();
    assert!(!score.findings.iter().any(|f| f.rule == Rule::Arguments));
}

#[test]
fn generic_type_commas_are_not_function_arguments() {
    for (lang, src) in [
        (
            Language::Java,
            "class A { void f(Map<String,List<Integer>> a, int b, int c, int d) {} }",
        ),
        (
            Language::Rust,
            "fn f(a:Map<Key,Vec<Value>>,b:i32,c:i32,d:i32) {}",
        ),
        (
            Language::Kotlin,
            "fun f(a:Map<String,List<Int>>, b:Int, c:Int, d:Int) {}",
        ),
        (
            Language::Csharp,
            "class A { void F(Dictionary<string,List<int>> a, int b, int c, int d) {} }",
        ),
        (
            Language::Cpp,
            "void f(std::map<int,std::vector<int>> a, int b, int c, int d) {}",
        ),
    ] {
        let score = scored(lang, src, "fixture", None).1.unwrap();
        assert!(
            !score.findings.iter().any(|f| f.rule == Rule::Arguments),
            "{lang:?}: {:?}",
            score.findings
        );
    }
}

#[test]
fn measured_cpp_bodies_do_not_require_an_active_build_configuration() {
    let body = "if (a) { work(); }\n".repeat(8);
    let src = format!("#if defined(OPTIONAL_BACKEND)\nvoid run(int a) {{\n{body}}}\n#endif\n");
    let (facts, score) = scored(Language::Cpp, &src, "x.cpp", None);
    assert!(
        !facts
            .declarations
            .iter()
            .any(|d| &src[d.span.clone()] == "run")
    );
    let score = score.unwrap();
    assert!(
        score
            .findings
            .iter()
            .any(|f| f.rule == Rule::Complexity && f.unit == "run" && f.line == 2 && f.value == 9)
    );
}

#[test]
fn lua_data_and_cmake_documentation_cannot_end_or_open_functions() {
    let lua = format!(
        "function sample(x)\nlocal defaults={{}}\n{}end\n",
        "x = x and defaults or false\n".repeat(4)
    );
    let cmake = format!(
        "#[==[ example(fake( |\n]==]\nfunction(sample x)\nset(y ${{x}})\n{}endfunction()\n",
        "if(x)\nmessage(OK)\nendif()\n".repeat(8)
    );
    for (lang, src, line) in [(Language::Lua, lua, 1), (Language::CMake, cmake, 3)] {
        let score = scored(lang, &src, "fixture", None).1.unwrap();
        assert!(
            score.findings.iter().any(|f| f.rule == Rule::Complexity
                && f.unit == "sample"
                && f.line == line
                && f.value == 9),
            "{lang:?} {:?}",
            score.findings
        );
    }
}

#[test]
fn preprocessor_choices_and_runtime_decisions_are_counted_separately() {
    let directives = "#if defined(FAST) && defined(V2)\n#endif\n".repeat(10);
    let src = format!(
        "void work() {{\n{directives}{}\n}}",
        "x = ready ? a : b;\n".repeat(8)
    );
    let score = scored(Language::Cpp, &src, "x.cpp", None).1.unwrap();
    assert!(
        score
            .findings
            .iter()
            .any(|f| f.unit == "work" && f.rule == Rule::Complexity && f.value == 9)
    );
    assert!(!score.findings.iter().any(|f| f.rule == Rule::Conditionals));
    assert!(
        scored(Language::C, &directives, "x.h", None)
            .1
            .unwrap()
            .lines
            > 0
    );
}

#[test]
fn constructors_and_shell_case_arms_own_their_findings() {
    let java = "class Job { Job(int a, int b, int c, int d, int e) {} }";
    let score = scored(Language::Java, java, "Job.java", None).1.unwrap();
    assert!(
        score
            .findings
            .iter()
            .any(|f| f.unit == "Job" && f.rule == Rule::Arguments && f.value == 5)
    );
    let shell = format!(
        "run() {{\ncase \"$1\" in\n{}esac\n}}",
        (0..8)
            .map(|i| format!("{i}) echo 'case ;;& )' ;;\n"))
            .collect::<String>()
    );
    let score = scored(Language::Shell, &shell, "x.sh", None).1.unwrap();
    assert!(
        score
            .findings
            .iter()
            .any(|f| f.unit == "run" && f.rule == Rule::Complexity && f.value == 9)
    );
}

#[test]
fn independent_deep_functions_each_affect_the_file_score() {
    let mut score = Score::default();
    score.record(Rule::Nesting, 4, 1, "outer");
    let single = score.value();
    score.record(Rule::Nesting, 4, 20, "another");
    assert!(score.value() < single);
}

#[test]
fn duplicate_witnesses_survive_order_changes_and_exclude_overlapping_ranges() {
    // Each end shares 57 of 61 shingles with the middle, and 55 of 63 with the other end.
    let mut words = Vec::new();
    for changed in [0, 2, 4, 60] {
        for i in 0..60 {
            let prefix = if i < changed { "alternate" } else { "base" };
            words.push(format!("{prefix}{i}"));
        }
    }
    let source = words.join(" ");
    let tokens = crate::domain::code::lexer::tokens(&source, Language::Rust);
    let ranges = [0..60, 60..120, 120..180, 180..240, 181..240];
    for order in [[0, 1, 2, 3, 4], [1, 2, 0, 4, 3], [4, 3, 2, 0, 1]] {
        let bodies = order.map(|id| duplicates::Body {
            name: "unit",
            line: id + 1,
            lines: 12,
            tokens: ranges[id].clone(),
        });
        let mut score = Score::default();
        duplicates::record(&mut score, &tokens, &bodies);
        let marked: Vec<_> = score.findings.iter().map(|f| f.line).collect();
        let expected: Vec<_> = order.iter().filter(|&&id| id < 3).map(|&id| id + 1).collect();
        assert_eq!(marked, expected);
    }
}

#[test]
fn void_tag_spelling_preserves_parent_depth() {
    for tag in ["<br>", "<br/>", "<BR />"] {
        let html = format!("<main><section>{tag}<div><p><span>x</span></p></div></section></main>");
        let score = scored(Language::Html, &html, "page.html", None).1.unwrap();
        assert!(score.findings.iter().any(|f| f.rule == Rule::Nesting && f.value == 5));
        assert!(!score.findings.iter().any(|f| f.rule == Rule::Bumps));
    }
}

#[test]
fn decorator_arguments_do_not_change_method_binding() {
    for (decorator, static_method) in [("@staticmethod", true), ("@remember(staticmethod)", false)] {
        let code = format!("class Group:\n    {decorator}\n    def run(self,a,b,c,d): pass\n");
        let score = scored(Language::Python, &code, "methods.py", None).1.unwrap();
        assert_eq!(score.findings.iter().any(|f| f.rule == Rule::Arguments), static_method);
    }
}
