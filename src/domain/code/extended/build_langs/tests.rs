use super::*;
use crate::domain::code::{analyze, Language::*};

fn declarations<'a>(a: &Analysis, s: &'a str) -> Vec<(&'a str, &'static str)> {
    a.declarations
        .iter()
        .map(|d| (&s[d.span.clone()], d.kind))
        .collect()
}
fn edges<'a>(a: &Analysis, s: &'a str) -> Vec<(&'a str, &'a str, Option<&'a str>, bool)> {
    a.direct
        .iter()
        .map(|(i, targets)| {
            assert_eq!(targets.len(), 1);
            let c = &a.calls[*i];
            (
                &s[c.span.clone()],
                &s[a.declarations[targets[0]].span.clone()],
                c.owner.map(|o| &s[a.declarations[o].span.clone()]),
                c.is_call,
            )
        })
        .collect()
}

#[test]
fn docker_stages_resolve_only_prior_aliases_and_skip_heredocs() {
    let s = "FROM alpine AS Build\nRUN <<EOF\nFROM image AS fake\nEOF\nFROM build AS final\nCOPY --from=BUILD /a /b\nRUN --mount=type=bind,from=Build echo build\nCOPY --from=future /a /b\nFROM alpine AS future\n";
    let a = analyze(Dockerfile, s);
    assert_eq!(
        declarations(&a, s),
        [
            ("Build", "build_stage"),
            ("final", "build_stage"),
            ("future", "build_stage")
        ]
    );
    assert_eq!(
        edges(&a, s),
        [
            ("build", "Build", Some("final"), false),
            ("BUILD", "Build", Some("final"), false),
            ("Build", "Build", Some("final"), false)
        ]
    );
}
#[test]
fn make_targets_variables_and_recursive_recipe() {
    let s = "OUT := yes\nall: compile | prep\n\t@$(MAKE) compile\n\t@echo $(OUT) $$(OUT)\ncompile:\nprep:\n\t@echo not-a-rule: compile\ndefine literal\nfake: compile\nendef\n";
    let a = analyze(Makefile, s);
    assert_eq!(
        declarations(&a, s),
        [
            ("OUT", "variable_declarator"),
            ("all", "build_target"),
            ("compile", "build_target"),
            ("prep", "build_target")
        ]
    );
    assert_eq!(
        edges(&a, s),
        [
            ("compile", "compile", Some("all"), false),
            ("prep", "prep", Some("all"), false),
            ("compile", "compile", Some("all"), true),
            ("OUT", "OUT", Some("all"), false)
        ]
    );
}
#[test]
fn make_comment_offsets_follow_recipe_prefix_and_continuations() {
    let source = concat!(
        "  # first\n.RECIPEPREFIX := !\ntarget:\n!# recipe\n",
        "VALUE = one \\\n# continued operand\n",
        "define BODY\ndefine INNER\n# body\nendef\nendef\n#",
    );
    let mut offsets = Vec::new();
    super::make_comments(source, |offset| offsets.push(offset));
    assert_eq!(offsets, [source.find("# first").unwrap(), source.len() - 1]);
}

#[test]
fn batch_call_and_goto_have_different_meanings() {
    let s = "@echo off\r\n:Main\r\n@CALL :Work x\r\ngoto :work\r\nrem call :Work\r\necho call :Work\r\n::fake\r\n:Work\r\ngoto :eof\r\n";
    let a = analyze(Batchfile, s);
    assert_eq!(
        declarations(&a, s),
        [("Main", "batch_label"), ("Work", "batch_label")]
    );
    assert_eq!(
        edges(&a, s),
        [
            ("Work", "Work", Some("Main"), true),
            ("work", "Work", Some("Main"), false)
        ]
    );
}
#[test]
fn procfile_records_only_process_types() {
    let s = "web: worker --url=http://example.org\nworker: echo web\n# fake: ignored\n bad: command\nempty:\n";
    let a = analyze(Procfile, s);
    assert_eq!(
        declarations(&a, s),
        [("web", "process_type"), ("worker", "process_type")]
    );
    assert!(edges(&a, s).is_empty());
}
#[test]
fn cmake_command_boundaries_and_local_procedures() {
    let s = "add_library(core STATIC src.cc)\nadd_executable(app main.cc)\nfunction(Helper arg)\nmessage(STATUS \"${arg}\")\nendfunction()\nmacro(run)\nhELPer(\n  app\n)\nendmacro()\nrun()\ntarget_link_libraries(app PRIVATE core)\n#[=[ function(fake) ]=]\nmessage([=[fake() Helper()]=])\n";
    let a = analyze(CMake, s);
    assert_eq!(
        declarations(&a, s),
        [
            ("core", "build_target"),
            ("app", "build_target"),
            ("Helper", "function_definition"),
            ("arg", "parameter"),
            ("run", "function_definition")
        ]
    );
    let e = edges(&a, s);
    assert!(e.contains(&("arg", "arg", Some("Helper"), false)));
    assert!(e.contains(&("hELPer", "Helper", Some("run"), true)));
    assert!(e.contains(&("run", "run", None, true)));
    assert!(
        e.iter()
            .any(|&(u, t, _, c)| u == "core" && t == "core" && !c)
    );
    assert_eq!(e.len(), 5);
}
#[test]
fn hcl_terraform_addresses_scope_labels_and_ignore_literals() {
    let s = "variable \"region\" {}\nlocals { home = var.region }\nresource \"cloud_vm\" \"main\" {\n  zone = \"${local.home}\"\n  plain = \"var.region $${local.home}\"\n  script = <<-EOF\n  output \"fake\" {}\n  EOF\n}\noutput \"id\" { value = cloud_vm.main.id }\n";
    let a = analyze(Hcl, s);
    assert_eq!(
        declarations(&a, s),
        [
            ("region", "block_label"),
            ("home", "attribute"),
            ("main", "block_label"),
            ("zone", "attribute"),
            ("plain", "attribute"),
            ("script", "attribute"),
            ("id", "block_label"),
            ("value", "attribute")
        ]
    );
    assert_eq!(
        edges(&a, s),
        [
            ("region", "region", Some("home"), false),
            ("home", "home", Some("main"), false),
            ("main", "main", Some("id"), false)
        ]
    );
}
#[test]
fn hcl_data_addresses_preserve_the_target_across_comments_and_field_access() {
    let source = r#"data "platform" "server" {}
locals {
  spaced = data /* gap */ . platform . server.zone
  direct = "${data.platform.server.zone}"
  invalid = "${data.platform.server.bad!}"
}"#;
    assert_eq!(
        edges(&analyze(Hcl, source), source),
        [
            ("server", "server", Some("spaced"), false),
            ("server", "server", Some("direct"), false),
        ]
    );
}

#[test]
fn nix_recursive_let_static_selections_params_and_nonrecursive_attrs() {
    let s = "let base = 1; fn = x: x; cfg = { base = 2; value = base; }; other = rec { a = 1; b = a; }; in fn cfg.value";
    let a = analyze(Nix, s);
    let e = edges(&a, s);
    assert!(e.contains(&("x", "x", Some("fn"), false)));
    assert!(e.contains(&("fn", "fn", None, true)));
    assert!(e.contains(&("value", "value", None, false)));
    assert!(e.contains(&("a", "a", Some("b"), false)));
    let site = s.find("= base; }").unwrap() + 2;
    let (i, targets) = a
        .direct
        .iter()
        .find(|(i, _)| a.calls[*i].span.start == site)
        .unwrap();
    assert_eq!(
        a.declarations[targets[0]].span.start,
        s.find("base = 1").unwrap()
    );
    assert!(!a.calls[*i].is_call);
    let s = "let f = x: x; in [ f f \"f\" '' let fake = f; '' ]";
    let a = analyze(Nix, s);
    assert!(a.calls.iter().all(|c| !c.is_call));
    assert!(!declarations(&a, s).iter().any(|d| d.0 == "fake"));
}

#[test]
fn continuations_opaque_input_and_shadowing_do_not_create_false_links() {
    let s = "target: one \\\n  two\none:\ntwo:\n";
    let a = analyze(Makefile, s);
    assert_eq!(
        edges(&a, s),
        [
            ("one", "one", Some("target"), false),
            ("two", "two", Some("target"), false)
        ]
    );
    let s = "FROM alpine AS root\nRUN echo '<<EOF'\nFROM root AS final\n";
    let a = analyze(Dockerfile, s);
    assert_eq!(declarations(&a, s).len(), 2);
    assert_eq!(edges(&a, s), [("root", "root", Some("final"), false)]);
    let s = "let f = x: { output = x; }; path = 1; in [ ./path path/sub <path> f.output ]";
    let a = analyze(Nix, s);
    let e = edges(&a, s);
    assert!(!e.iter().any(|(u, _, _, _)| *u == "path" || *u == "output"));
    let s = "let x=1; in let inherit (external) x; in x";
    assert!(edges(&analyze(Nix, s), s).is_empty());
    let s = "set(undefined \"${undefined}\")\nadd_library(a STATIC a.c)\nadd_executable(b main.c)\ntarget_link_libraries(b PRIVATE a)\n";
    let a = analyze(CMake, s);
    assert_eq!(
        edges(&a, s),
        [("b", "b", None, false), ("a", "a", Some("b"), false)]
    );
}

#[test]
fn docker_numbered_stages_and_strict_heredoc_delimiters() {
    let s = "FROM scratch AS build\nRUN <<EOF\n  EOF\nFROM scratch AS not_a_stage\nEOF\nFROM scratch AS app\nCOPY --from=0 /a /b\nCOPY --from=app /a /b\n";
    let a = analyze(Dockerfile, s);
    assert_eq!(
        declarations(&a, s),
        [("build", "build_stage"), ("app", "build_stage")]
    );
    assert_eq!(edges(&a, s), [("0", "build", Some("app"), false)]);
}

#[test]
fn batch_conditional_commands_resolve_gradle_labels_only_in_command_position() {
    let s = ":start\r\nif defined JAVA_HOME goto findJava\r\nif %ERRORLEVEL% equ 0 goto execute\r\nif exist \"%Program Files%\\java.exe\" goto execute\r\nif /I not \"a b\"==\"c d\" call :execute arg\r\nif exist \"goto execute\" echo goto execute\r\nif \"goto execute\" equ \"goto execute\" echo call :execute\r\n:findJava\r\n:execute\r\n";
    let a = analyze(Batchfile, s);
    assert_eq!(
        edges(&a, s),
        [
            ("findJava", "findJava", Some("start"), false),
            ("execute", "execute", Some("start"), false),
            ("execute", "execute", Some("start"), false),
            ("execute", "execute", Some("start"), true)
        ]
    );
}
