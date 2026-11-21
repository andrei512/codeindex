use super::*;
#[cfg(test)]
mod tests {
    use super::*;
    const LIB: &str = "mod inner; use crate::inner::{work as renamed, Thing}; fn outer(x: Thing) { let before = renamed; renamed(); let x = x; { let x = x; let z = x; } let z = x; }";
    const INNER: &str = "pub struct Thing; pub fn work() {}";
    fn fixture<R>(test: impl FnOnce(&[Unit], Names) -> R) -> R {
        let files = [
            ("src/lib.rs", LIB),
            ("src/inner.rs", INNER),
            ("other.rs", "pub fn work(){}"),
        ];
        let analyses: Vec<_> = files
            .iter()
            .map(|(_, s)| crate::domain::code::analyze(crate::domain::code::Language::Rust, s))
            .collect();
        let units: Vec<_> = files
            .iter()
            .zip(&analyses)
            .map(|(&(path, text), analysis)| Unit {
                path,
                text,
                analysis,
            })
            .collect();
        let names = Names::build(&units);
        test(&units, names)
    }
    fn at(u: &Unit, text: &str, delta: usize) -> usize {
        let pos = u.text.find(text).unwrap() + delta;
        u.analysis
            .calls
            .iter()
            .position(|c| c.span.start == pos)
            .unwrap()
    }
    #[test]
    fn grouped_imports_and_reference_call_roles() {
        fixture(|units, n| {
            let u = &units[0];
            let reference = at(u, "before = renamed", 9);
            let call = at(u, "renamed();", 0);
            assert!(!u.analysis.calls[reference].is_call);
            assert!(u.analysis.calls[call].is_call);
            let target = n.targets[&Site {
                file: 0,
                local: call,
            }][0];
            assert_eq!(target.file, 1);
            assert_eq!(n.incoming[&target].len(), 4);
            assert_eq!(
                n.targets[&Site {
                    file: 0,
                    local: reference
                }],
                [target]
            );
            let ty = at(u, "x: Thing", 3);
            let target = n.targets[&Site { file: 0, local: ty }][0];
            assert_eq!(target, Site { file: 1, local: 0 });
        })
    }
    #[test]
    fn shadowing_starts_after_initializer_and_stops_at_block_end() {
        fixture(|units, n| {
            let u = &units[0];
            let binds = |part: &str, delta| {
                let target = n.targets[&Site {
                    file: 0,
                    local: at(u, part, delta),
                }][0];
                assert_eq!(target.file, 0);
                u.analysis.declarations[target.local].span.start
            };
            assert_eq!(binds("let x = x; {", 8), LIB.find("x: Thing").unwrap());
            assert_eq!(binds("{ let x = x", 10), LIB.find("let x").unwrap() + 4);
            assert_eq!(
                binds("let z = x; } let", 8),
                LIB.find("{ let x").unwrap() + 6
            );
            assert_eq!(binds("} let z = x;", 10), LIB.find("let x").unwrap() + 4);
            assert!(!n.by_name.contains_key("x"));
        });
    }
    #[test]
    fn macros_strings_and_raw_identifiers() {
        let s = r###"fn r#type() {} fn f(){ let _=r##" quote " fn fake() {} "##; m!{fn generated(){}} r#type(); }"###;
        let a = crate::domain::code::analyze(crate::domain::code::Language::Rust, s);
        let u = Unit {
            path: "src/lib.rs",
            text: s,
            analysis: &a,
        };
        let n = Names::build(&[u]);
        assert!(!n.by_name.contains_key("fake"));
        assert!(!n.by_name.contains_key("generated"));
        let target = n.by_name["r#type"][0];
        assert_eq!(n.incoming[&target].len(), 1);
    }
}

#[cfg(test)]
mod semantic_cases {
    use super::*;
    const METHODS: &str = r#"
struct A { score: u32 }
struct B { score: u8 }
impl A {
    fn make() -> Self { Self { score: 3 } }
    fn score(&self) -> u32 { self.score }
}
fn run() -> u32 {
    let one = A::make();
    let two: B = B { score: 4 };
    let _ = two.score;
    one.score()
}
"#;
    const SCOPES: &str = r#"
struct View<'a, T> { value: &'a T }
fn pick(fallback: u8, arg: Option<u8>) -> u8 {
    match arg {
        Some(fallback) if fallback > 0 => fallback,
        _ => fallback,
    }
}
fn check() { let mut total = 0; for (a,b) in [(1,2)] {total += a+b;} }
"#;
    fn exercise(files: &[(&str, &str)], inspect: impl FnOnce(&[Unit], &Names)) {
        let analyses: Vec<_> = files
            .iter()
            .map(|(path, s)| {
                if path.ends_with(".rs") {
                    crate::domain::code::analyze(crate::domain::code::Language::Rust, s)
                } else {
                    Analysis::default()
                }
            })
            .collect();
        let units: Vec<_> = files
            .iter()
            .zip(&analyses)
            .map(|(&(path, text), analysis)| Unit {
                path,
                text,
                analysis,
            })
            .collect();
        let names = Names::build(&units);
        inspect(&units, &names);
    }
    fn targets(
        units: &[Unit],
        n: &Names,
        file: usize,
        context: &str,
        offset: usize,
    ) -> Vec<(usize, usize, &'static str)> {
        let u = &units[file];
        let start = u.text.find(context).unwrap() + offset;
        let mut found = Vec::new();
        for (local, c) in u
            .analysis
            .calls
            .iter()
            .enumerate()
            .filter(|(_, c)| c.span.start == start)
        {
            for site in n.targets.get(&Site { file, local }).into_iter().flatten() {
                let d = &units[site.file].analysis.declarations[site.local];
                found.push((site.file, d.span.start, d.kind));
            }
            assert!(c.span.end > start);
        }
        found.sort_unstable();
        found.dedup();
        found
    }
    #[test]
    fn written_receiver_types_disambiguate_fields_methods_and_constructor_results() {
        exercise(&[("src/lib.rs", METHODS)], |u, n| {
            let field_a = METHODS.find("score: u32").unwrap();
            let field_b = METHODS.find("score: u8").unwrap();
            assert_eq!(
                targets(u, n, 0, "self.score", 5),
                [(0, field_a, "field_declaration")]
            );
            assert_eq!(
                targets(u, n, 0, "two.score", 4),
                [(0, field_b, "field_declaration")]
            );
            assert_eq!(
                targets(u, n, 0, "one.score", 4),
                [(0, METHODS.find("fn score").unwrap() + 3, "function_item")]
            );
        });
    }
    #[test]
    fn concrete_trait_calls_distinguish_overrides_from_abstract_dispatch() {
        let source = "trait Work { fn run(&self); } struct Job; impl Work for Job { fn run(&self) {} } fn by_value(x: Job) { x.run(); } fn generic<T: Work>(g: T) { g.run(); } fn erased(d: &dyn Work) { d.run(); } mod other { fn without_import(j: super::Job) { j.run(); } }";
        exercise(&[("src/lib.rs", source)], |u, n| {
            let signature = source.find("fn run").unwrap() + 3;
            let implementation = source.rfind("fn run").unwrap() + 3;
            assert_eq!(
                targets(u, n, 0, "x.run", 2),
                [(0, implementation, "function_item")]
            );
            for site in ["g.run", "d.run"] {
                assert_eq!(
                    targets(u, n, 0, site, 2),
                    [(0, signature, "function_signature_item")]
                );
            }
            assert!(targets(u, n, 0, "j.run", 2).is_empty());
        });
    }
    #[test]
    fn collection_closure_and_literal_values_keep_written_binding_types() {
        let source = "struct Entry { value: u32 } fn f(rows: &[Entry], amount: u32) { rows.iter().map(|row| row.value); let e = Entry { value: amount }; let _ = 0..e.value; }";
        exercise(&[("src/lib.rs", source)], |u, n| {
            let field = (0, source.find("value:").unwrap(), "field_declaration");
            assert_eq!(targets(u, n, 0, "row.value", 4), [field]);
            assert_eq!(targets(u, n, 0, "..e.value", 4), [field]);
            assert_eq!(
                targets(u, n, 0, "value: amount", 7),
                [(0, source.find("amount:").unwrap(), "parameter")]
            );
        });
    }
    #[test]
    fn generics_match_guards_and_for_patterns() {
        exercise(&[("src/lib.rs", SCOPES)], |u, n| {
            assert_eq!(
                targets(u, n, 0, "&'a T", 1),
                [(0, SCOPES.find("'a").unwrap(), "lifetime_parameter")]
            );
            assert_eq!(
                targets(u, n, 0, "&'a T", 4),
                [(0, SCOPES.find("T>").unwrap(), "generic_parameter")]
            );
            let arm = SCOPES.find("Some(fallback)").unwrap() + 5;
            assert_eq!(
                targets(u, n, 0, "if fallback", 3),
                [(0, arm, "local_binding")]
            );
            assert_eq!(
                targets(u, n, 0, "0 => fallback", 5),
                [(0, arm, "local_binding")]
            );
            assert_eq!(
                targets(u, n, 0, "_ => fallback", 5),
                [(0, SCOPES.find("fallback:").unwrap(), "parameter")]
            );
            assert_eq!(
                targets(u, n, 0, "+= a+b", 3),
                [(0, SCOPES.find("(a,b)").unwrap() + 1, "local_binding")]
            );
        });
    }
    #[test]
    fn cargo_path_alias_picks_its_declared_dependency() {
        let files = [
            (
                "consumer/Cargo.toml",
                "[package]\nname = \"consumer\"\n[dependencies]\nwire = { package = \"target-package\", path = \"../one\" }\n",
            ),
            ("one/Cargo.toml", "[package]\nname=\"target-package\"\n"),
            (
                "consumer/src/lib.rs",
                "use wire::Widget; fn test(_: Widget) {}",
            ),
            ("one/src/lib.rs", "pub struct Widget;"),
            ("unrelated/src/lib.rs", "pub struct Widget;"),
        ];
        exercise(&files, |u, n| {
            assert_eq!(targets(u, n, 2, "_: Widget", 3), [(3, 11, "struct_item")]);
            assert_eq!(targets(u, n, 2, "use wire", 4), [(3, 0, "source_file")]);
        });
    }
}

#[cfg(test)]
#[test]
fn structural_symbol_contract_preserves_associated_navigation() {
    let code = "trait Api {type Error; type Iter: Iterator<Item=u8>; fn act();} struct S; impl Api for S {type Error=(); type Iter=std::vec::IntoIter<u8>;fn act(){}} fn union(){} const _: ()=();";
    let a = crate::domain::code::analyze(crate::domain::code::Language::Rust, code);
    let names = Names::build(&[Unit {
        path: "lib.rs",
        text: code,
        analysis: &a,
    }]);
    for name in ["_", "union", "Error", "Iter"] {
        assert_eq!(names.by_name[name].len(), 1, "{name}");
    }
    assert_eq!(
        a.declarations
            .iter()
            .filter(|d| d.kind == "associated_type")
            .count(),
        2
    );
}

#[cfg(test)]
#[test]
fn pointer_slice_tuple_and_unwrap_track_declared_payloads() {
    let code = "use std::sync::Arc; struct Row { k:u8 } fn pair()->(Row,u8){(Row{k:1},2)} fn fetch()->Option<Row>{None} fn inspect(a:Arc<Row>, xs:&[Row]){let _=a.k; for row in xs {let _=row.k;} let (thing,_)=pair();let _=thing.k; let value=fetch().unwrap();let _=value.k;}";
    let analysis = crate::domain::code::analyze(crate::domain::code::Language::Rust, code);
    let names = Names::build(&[Unit {
        path: "src/lib.rs",
        text: code,
        analysis: &analysis,
    }]);
    let target = code.find("k:u8").unwrap();
    for text in ["a.k", "row.k", "thing.k", "value.k"] {
        let pos = code.find(text).unwrap() + text.len() - 1;
        let local = analysis
            .calls
            .iter()
            .position(|c| c.span.start == pos)
            .unwrap();
        let sites = names.targets.get(&Site { file: 0, local }).expect(text);
        assert_eq!(sites.len(), 1);
        assert_eq!(
            analysis.declarations[sites[0].local].span.start, target,
            "{text}"
        );
    }
}

#[test]
fn explicit_module_path_wins_and_return_impl_trait_keeps_parameters() {
    use crate::domain::code::{Language, analyze};
    let src = r#"#[path="chosen.rs"] mod target; fn wrap(x: u8) -> impl Copy { x } fn call(){target::work();}"#;
    let texts = [src, "pub fn work() {}", "pub fn work() {}"];
    let aa: Vec<_> = texts.iter().map(|s| analyze(Language::Rust, s)).collect();
    let paths = ["src/lib.rs", "src/chosen.rs", "src/target.rs"];
    let units: Vec<_> = (0..3)
        .map(|i| Unit {
            path: paths[i],
            text: texts[i],
            analysis: &aa[i],
        })
        .collect();
    let n = Names::build(&units);
    let sites = |at| {
        let local = aa[0].calls.iter().position(|c| c.span.start == at).unwrap();
        &n.targets[&Site { file: 0, local }]
    };
    assert_eq!(sites(src.find("work();").unwrap())[0].file, 1);
    let uses = sites(src.find("{ x }").unwrap() + 2);
    assert_eq!(
        aa[0].declarations[uses[0].local].span.start,
        src.find("x:").unwrap()
    );
}

#[test]
fn target_scopes_choose_modules_and_imports_without_hiding_written_symbols() {
    use crate::domain::code::{Language, analyze};
    let src = r#"
#[cfg(target_os="linux")] mod machine;
#[cfg(not(target_os="linux"))] mod machine { pub fn run() {} }
mod other { pub fn run() {} }
#[cfg(target_os="linux")] use machine::run as selected;
#[cfg(not(target_os="linux"))] use other::run as selected;
fn use_it(){selected();machine::run();}
"#;
    let ext = "pub fn helper(){} pub fn run(){helper();}";
    for (triple, linux) in [
        ("aarch64-apple-darwin", false),
        ("aarch64-unknown-linux-gnu", true),
    ] {
        let a = analyze(Language::Rust, src);
        let b = analyze(Language::Rust, ext);
        let empty = Analysis::default();
        let config = format!("[build]\ntarget=\"{triple}\"\n");
        let units = [
            Unit {
                path: "src/lib.rs",
                text: src,
                analysis: &a,
            },
            Unit {
                path: "src/machine.rs",
                text: ext,
                analysis: &b,
            },
            Unit {
                path: ".cargo/config.toml",
                text: &config,
                analysis: &empty,
            },
        ];
        let n = Names::build(&units);
        assert!(n.by_name["helper"].contains(&Site { file: 1, local: 0 }));
        let calls = b
            .calls
            .iter()
            .enumerate()
            .filter(|(i, _)| n.targets.contains_key(&Site { file: 1, local: *i }))
            .count();
        assert_eq!(calls, usize::from(linux));
        let index = a
            .calls
            .iter()
            .position(|c| c.span.start == src.rfind("selected()").unwrap())
            .unwrap();
        let target = n.targets[&Site {
            file: 0,
            local: index,
        }][0];
        if linux {
            assert_eq!(target.file, 1);
        } else {
            assert_eq!(
                a.declarations[target.local].span.start,
                src.find("mod other { pub fn run").unwrap() + "mod other { pub fn ".len()
            );
        }
    }
}

#[test]
fn public_labels_require_a_cargo_target_and_canonical_written_descriptors() {
    use crate::domain::code::{Language, analyze};
    let root = r#"[workspace]
members = ["apps/*"]
[workspace.package]
version = "1.4.2"
"#;
    let manifest = "[package]\nname=\"my-core\"\nversion.workspace=true\n";
    let lib = "pub mod model; pub fn run(local:u32){let _=local;}";
    let model = "pub trait Work {fn do_it(&self);} use Work as MyAlias; pub struct Item {pub value:u32} impl MyAlias for Item {fn do_it(&self){}} impl Item {pub fn new()->Self{Self{value:2}}} pub enum Choice{One{answer:u8}}";
    let files = [
        ("sub/Cargo.toml", root),
        ("sub/apps/core/Cargo.toml", manifest),
        ("sub/apps/core/src/lib.rs", lib),
        ("sub/apps/core/src/model.rs", model),
        ("sub/apps/core/scratch.rs", "fn unknown(){}"),
    ];
    let a: Vec<_> = files
        .iter()
        .map(|(p, s)| {
            if p.ends_with(".rs") {
                analyze(Language::Rust, s)
            } else {
                Analysis::default()
            }
        })
        .collect();
    let units: Vec<_> = files
        .iter()
        .zip(&a)
        .map(|(&(path, text), analysis)| Unit {
            path,
            text,
            analysis,
        })
        .collect();
    let n = Names::build(&units);
    let labels = &n.public_labels;
    let label = |file: usize, start: usize, kind: &str| -> Option<&str> {
        let local = a[file]
            .declarations
            .iter()
            .position(|d| d.span.start == start && d.kind == kind)?;
        labels.get(&Site { file, local }).map(String::as_str)
    };
    let prefix = "rust-analyzer sub rust-analyzer cargo my-core 1.4.2 ";
    assert_eq!(
        label(2, lib.find("run(").unwrap(), "function_item").unwrap(),
        format!("{prefix}run().")
    );
    assert_eq!(
        label(2, 0, "source_file").unwrap(),
        format!("{prefix}crate/")
    );
    assert_eq!(
        label(3, 0, "source_file").unwrap(),
        format!("{prefix}model/")
    );
    for (part, kind, expected) in [
        ("Work {", "trait_item", "Work#"),
        ("Item {", "struct_item", "Item#"),
        ("value:u32", "field_declaration", "Item#value."),
        ("new()", "function_item", "impl#[Item]new()."),
        ("answer:u8", "field_declaration", "Choice#One#answer."),
    ] {
        assert_eq!(
            label(3, model.find(part).unwrap(), kind).unwrap(),
            format!("{prefix}model/{expected}")
        );
    }
    assert_eq!(
        label(3, model.rfind("do_it").unwrap(), "function_item").unwrap(),
        format!("{prefix}model/impl#[Item][Work]do_it().")
    );
    assert_eq!(label(2, lib.find("local:").unwrap(), "parameter"), None);
    assert_eq!(label(4, 3, "function_item"), None);
}
