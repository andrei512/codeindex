use super::*;

fn symbol_names<'a>(a: &Analysis, src: &'a str) -> Vec<&'a str> {
    a.declarations
        .iter()
        .map(|d| &src[d.span.clone()])
        .collect()
}
fn edges<'a>(a: &Analysis, s: &'a str) -> Vec<(&'a str, Option<&'a str>, Option<&'a str>)> {
    a.calls
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let target = a
                .direct
                .iter()
                .find(|(use_id, _)| *use_id == i)
                .map(|(_, ids)| &s[a.declarations[ids[0]].span.clone()]);
            (
                &s[c.span.clone()],
                c.owner.map(|id| &s[a.declarations[id].span.clone()]),
                target,
            )
        })
        .collect()
}

#[test]
fn scoring_and_object_syntax_share_delimiter_recovery() {
    let fragments = ["(", ")", "[", "]", "{", "}", "name", "\"{\""];
    for a in fragments {
        for b in fragments {
            for c in fragments {
                let source = format!("{a} {b} {c}");
                let syntax = Syntax::new(Language::Go, &source);
                assert_eq!(syntax.pair, crate::domain::code::script::paired_for_score(&syntax.t), "{source}");
            }
        }
    }
}

#[test]
fn declarations_calls_owners_and_opaque_literals_in_all_seven_languages() {
    for (lang, src, target, caller) in [
        (
            Language::Go,
            "package sample\ntype Meter struct{}\nfunc café() {}\nfunc run() { café(); fmt.Println(`func bogus(){} fake()`); /* fake() */ }",
            "café",
            "run",
        ),
        (
            Language::Java,
            "final class Demo { private static void work() {} void run() { work(); foreign.work(); String text = \"\"\"\nfake() class Bogus {}\n\"\"\"; } }",
            "work",
            "run",
        ),
        (
            Language::Kotlin,
            "class Meter { fun work() {}\n fun run() = work()\n val text = \"\"\"fun bogus() fake()\"\"\" /* /* */ fun bogus() {} */ }",
            "work",
            "run",
        ),
        (
            Language::ObjectiveC,
            "static void work(void) {}\n@implementation Box\n- (void)run { work(); [self missing]; id s = @\"fake()\"; }\n@end",
            "work",
            "run",
        ),
        (
            Language::Dart,
            "void work() {}\nvoid run() { work(); print(r'''fake() class Bogus {}'''); /* /* */ fake() */ }",
            "work",
            "run",
        ),
        (
            Language::Php,
            "fake()<?php function work() {} function run() { WORK(); # fake()\n $s=<<<'END'\nfunction bogus() {fake();}\nEND;\n} ?>fake()",
            "work",
            "run",
        ),
        (
            Language::Hack,
            "<?hh function work(): void {} function run(): void { work(); /* fake() */ $s='function bogus() {}'; }",
            "work",
            "run",
        ),
    ] {
        let a = analyze(lang, src);
        let names = symbol_names(&a, src);
        let calls = edges(&a, src);
        assert!(
            names.contains(&target) && names.contains(&caller),
            "{lang:?}: {names:?}"
        );
        assert!(
            !names.contains(&"bogus") && !names.contains(&"Bogus"),
            "{lang:?}: {names:?}"
        );
        assert!(
            !calls.iter().any(|(name, _, _)| *name == "fake"),
            "{lang:?}: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|(_, owner, bound)| *owner == Some(caller) && *bound == Some(target)),
            "{lang:?}: {calls:?}"
        );
    }
}

#[test]
fn overloaded_shadowed_and_dynamic_calls_do_not_claim_an_implementation() {
    for (lang, s) in [
        (
            Language::Go,
            "package p\nfunc work() {}\nfunc run(work func()) { work(); remote.work(); }",
        ),
        (
            Language::Java,
            "class C { private void work() {} private void work(int x) {} void run() {work(); foreign.work();} }",
        ),
        (
            Language::Kotlin,
            "class C { open fun work() {} fun run() { work(); foreign.work(); } }",
        ),
        (
            Language::Dart,
            "void work() {} void run(Function work) { work(); foreign.work(); }",
        ),
        (
            Language::Php,
            "<?php class C { function work() {} function run() { $this->work(); $other->work(); } }",
        ),
        (
            Language::Hack,
            "<?hh class C { public function work(): void {} public function run(): void { $this->work(); } }",
        ),
        (
            Language::ObjectiveC,
            "@implementation C\n- (void)work {}\n- (void)run { [self work]; [other work]; }\n@end",
        ),
    ] {
        let a = analyze(lang, s);
        let e = edges(&a, s);
        assert!(
            e.iter()
                .any(|(name, owner, _)| *name == "work" && *owner == Some("run")),
            "{lang:?}: {e:?}"
        );
        assert!(
            !a.direct
                .iter()
                .any(|(use_id, targets)| a.calls[*use_id].is_call
                    && targets
                        .iter()
                        .any(|&id| &s[a.declarations[id].span.clone()] == "work")),
            "{lang:?}: {e:?}"
        );
    }
}

#[test]
fn type_references_and_multicomponent_selectors_keep_source_spans() {
    let s =
        "class Parent {} final class Child extends Parent { private void f() { new Parent(); } }";
    let a = analyze(Language::Java, s);
    let e = edges(&a, s);
    assert!(e.contains(&("Parent", None, Some("Parent"))), "{e:?}");
    assert!(e.contains(&("Parent", Some("f"), Some("Parent"))), "{e:?}");
    let s = "@implementation Box\n- (void)send:(int)x to:(int)y { [self send:x to:y]; }\n@end";
    let a = analyze(Language::ObjectiveC, s);
    assert_eq!(symbol_names(&a, s), ["Box", "send"]);
    assert_eq!(edges(&a, s), [("send", Some("send"), None)]);
}

#[test]
fn grouped_go_value_specs_bind_each_name_and_skip_initializer_commas() {
    let src = "package p\nvar (\na, b = 1, 2\nc, d = b, a\n)\nconst (\nx, y = 3, 4\n)\nfunc total() int { return b + d + y }";
    let facts = analyze(Language::Go, src);
    assert_eq!(
        symbol_names(&facts, src),
        ["a", "b", "c", "d", "x", "y", "total"]
    );
    let markers = ["a, b", "b =", "c, d", "d =", "x, y", "y =", "total()"];
    let positions: Vec<_> = facts.declarations.iter().map(|d| d.span.start).collect();
    assert_eq!(positions, markers.map(|anchor| src.find(anchor).unwrap()));
}

#[test]
fn go_grouped_types_and_generic_receivers_are_named() {
    let s = "package p\ntype (\n Meter struct { Value int }\n Reader interface{}\n)\nfunc (m *Meter) Count() int { return 1 }\nfunc identity[T any](v T) T { return v }";
    let a = analyze(Language::Go, s);
    assert_eq!(
        symbol_names(&a, s),
        ["Meter", "Reader", "Count", "identity"]
    );
}

#[test]
fn go_receiver_lookup_accepts_later_types_and_keeps_distinct_method_owners() {
    let src = concat!(
        "package p\n",
        "func (a *Alpha) step() {}\n",
        "func (b Beta) step() {}\n",
        "func (a *Alpha) run() { a.step() }\n",
        "func (b Beta) run() { b.step() }\n",
        "type (\nAlpha struct{}\nBeta struct{}\n)\n",
    );
    let a = analyze(Language::Go, src);
    let targets: Vec<_> = a.direct.iter().filter_map(|(row, targets)| {
        let use_span = &a.calls[*row].span;
        if &src[use_span.clone()] != "step" { return None; }
        let [id] = targets.as_slice() else { panic!("ambiguous method") };
        Some(a.declarations[*id].span.start)
    }).collect();
    assert_eq!(targets, [src.find("step()").unwrap(), src.find("Beta) step").unwrap() + 6]);
}

#[test]
fn scoped_go_methods_and_php_namespaces_do_not_cross_bindings() {
    let s = "package p\ntype Counter struct{}\nfunc (c *Counter) next() {}\nfunc (c *Counter) run() {c.next(); other.next()}\nfunc (c *Counter) shadow() {c := other; c.next()}";
    let calls = edges(&analyze(Language::Go, s), s);
    assert!(
        calls.contains(&("next", Some("run"), Some("next"))),
        "{calls:?}"
    );
    assert!(calls.contains(&("next", Some("run"), None)));
    assert!(calls.contains(&("next", Some("shadow"), None)));
    let s = "<?php namespace A { function helper(){} function run(){ helper(); } } namespace B { function other(){ helper(); } }";
    let calls = edges(&analyze(Language::Php, s), s);
    assert!(
        calls.contains(&("helper", Some("run"), Some("helper"))),
        "{calls:?}"
    );
    assert!(calls.contains(&("helper", Some("other"), None)));
}

#[test]
fn generic_return_types_and_fields_are_distinguished_from_parameters() {
    let s = "class Box { private int size; private java.util.List<String> values(int limit, int count) { return null; } }";
    assert_eq!(
        symbol_names(&analyze(Language::Java, s), s),
        ["Box", "size", "values"]
    );
    let s = "<?php class Box { private int $size; public function items(int $limit, int $count): void {} }";
    assert_eq!(
        symbol_names(&analyze(Language::Php, s), s),
        ["Box", "$size", "items"]
    );
}

#[test]
fn nested_function_names_and_selected_receiver_fields_do_not_cross_bind() {
    let s = "fun work() {}\nfun run() { fun work() {}\nwork() }";
    let a = analyze(Language::Kotlin, s);
    assert!(edges(&a, s).contains(&("work", Some("run"), None)));
    let s = "package p\ntype T struct{}\nfunc (t T) work() {}\nfunc (t T) run() { other.t.work() }";
    let a = analyze(Language::Go, s);
    assert!(edges(&a, s).contains(&("work", Some("run"), None)));
}

#[test]
fn php_header_selects_hack_and_keeps_original_offsets() {
    for bom in ["", "\u{feff}"] {
        let src =
            format!("{bom}<?hh\nnewtype Score = int; function value(): Score {{ return 1; }}");
        assert_eq!(
            symbol_names(&analyze(Language::Php, &src), &src),
            ["Score", "value"]
        );
    }
    let ordinary = "<?hhx newtype Spurious = int;";
    assert!(symbol_names(&analyze(Language::Php, ordinary), ordinary).is_empty());
    let markup = "function spurious() {}";
    assert!(symbol_names(&analyze(Language::Php, markup), markup).is_empty());
    let headerless = "function valid(): void {}\n// <?hh function spurious(): void {}";
    assert_eq!(
        symbol_names(&analyze(Language::Hack, headerless), headerless),
        ["valid"]
    );
}

#[test]
fn anonymous_callback_parameter_shadows_only_inside_its_body() {
    for (lang, src) in [
        (
            Language::Go,
            "package p\nfunc work() {}\nfunc run() { cb := func(work func()) { work() }; _ = cb; work() }",
        ),
        (
            Language::Kotlin,
            "fun work() {}\nfun run() { list.forEach { work -> work() }; work() }",
        ),
        (
            Language::Dart,
            "void work() {} void run() { final f = (work) { work(); }; work(); }",
        ),
    ] {
        let analysis = analyze(lang, src);
        let bound: Vec<_> = edges(&analysis, src)
            .into_iter()
            .filter(|(name, _, _)| *name == "work")
            .map(|(_, _, target)| target)
            .collect();
        assert_eq!(bound, [None, Some("work")], "{lang:?}");
    }
}

#[test]
fn explicit_super_ignores_inapplicable_parent_overload() {
    let src = "class Root { void work() {} } class Parent extends Root { void work(int x) {} } class Child extends Parent { void run() { super.work(); } }";
    assert!(edges(&analyze(Language::Java, src), src).contains(&("work", Some("run"), None)));
}

#[test]
fn objc_parameter_named_like_a_direct_attribute_remains_dynamic() {
    let src =
        "@implementation C\n- (void)work:(int)objc_direct {}\n- (void)run { [self work:2]; }\n@end";
    assert!(edges(&analyze(Language::ObjectiveC, src), src).contains(&("work", Some("run"), None)));
}

#[test]
fn php_reopened_namespace_uses_the_same_function_table() {
    let src = "<?php namespace A\\B { function work(){} } namespace Other { function work(){} } namespace a\\b { function run(){WORK();} }";
    let a = analyze(Language::Php, src);
    let (_, targets) = a
        .direct
        .iter()
        .find(|(call, _)| &src[a.calls[*call].span.clone()] == "WORK")
        .unwrap();
    assert_eq!(
        a.declarations[targets[0]].span.start,
        src.find("work").unwrap()
    );
}

#[test]
fn incomplete_php_declaration_keeps_the_type_at_a_shared_name_token() {
    let source = "<?php const class T = 0;";
    let facts = analyze(Language::Php, source);
    let kinds: Vec<_> = facts.declarations.iter()
        .filter(|d| &source[d.span.clone()] == "T")
        .map(|d| d.kind)
        .collect();
    assert_eq!(kinds, ["class_declaration"]);
}

#[test]
fn qualified_static_calls_require_an_unambiguous_local_class_and_public_implementation() {
    for (lang, header) in [(Language::Hack, "<?hh"), (Language::Php, "<?php")] {
        let src = format!("{header}
namespace A; class Box {{ public static function same() {{}} }}
            namespace B; class Box {{ public static function same() {{}} private static function hidden() {{}} public final function instance() {{}} }}
            function run() {{ Box::same(); Box::hidden(); Box::instance(); Other\\Box::same(); $box::same(); }}");
        let a = analyze(lang, &src);
        let resolved: Vec<_> = a.direct.iter().map(|(call, targets)| {
            (a.calls[*call].span.start, a.declarations[targets[0]].span.start)
        }).collect();
        assert_eq!(resolved, [(src.find("Box::same()").unwrap() + 5, src.rfind("function same").unwrap() + 9)]);
    }
    let private = "<?php class Box { private static function secret() {} public function inside() { Box::secret(); } } Box::secret();";
    let a = analyze(Language::Php, private);
    assert_eq!(edges(&a, private), [("secret", Some("inside"), Some("secret")), ("secret", None, None)]);
    let src = "<?hh class Box { public static function same(): void {} } function run<reify Box>(): void { Box::same(); }";
    assert!(analyze(Language::Hack, src).direct.is_empty());
}
