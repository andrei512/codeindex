use super::*;
#[test]
fn lexical_shadowing_and_aliases_are_file_scoped() {
    for (lang, library, consumer, other, decl, body) in [
        (
            Language::Python,
            "pkg/a.py",
            "pkg/main.py",
            "pkg/other.py",
            "def greet(): pass\n",
            "from .a import greet as hello\ndef use():\n    item = hello\n    hello()\ndef shadow(hello):\n    hello()\n",
        ),
        (
            Language::Js,
            "pkg/a.ts",
            "pkg/main.ts",
            "pkg/other.ts",
            "export function greet() {}",
            "import {greet as hello} from './a'; function use(){ const item = hello; hello(); } function shadow(hello){ hello(); }",
        ),
    ] {
        let x = super::super::analyze(lang, decl);
        let y = super::super::analyze(lang, body);
        let z = super::super::analyze(lang, decl);
        let units = [
            Unit {
                path: library,
                text: decl,
                analysis: &x,
            },
            Unit {
                path: consumer,
                text: body,
                analysis: &y,
            },
            Unit {
                path: other,
                text: decl,
                analysis: &z,
            },
        ];
        let graph = Names::build(&units);
        let target = Site { file: 0, local: 0 };
        let incoming = &graph.incoming[&target];
        assert_eq!(
            incoming.iter().filter(|s| y.calls[s.local].is_call).count(),
            1
        );
        assert!(
            incoming
                .iter()
                .any(|s| !y.calls[s.local].is_call
                    && &body[y.calls[s.local].span.clone()] == "hello")
        );
        assert!(!graph.incoming.contains_key(&Site { file: 2, local: 0 }));
        let call = incoming.iter().find(|s| y.calls[s.local].is_call).unwrap();
        let owner = y.calls[call.local].owner.unwrap();
        assert_eq!(&body[y.declarations[owner].span.clone()], "use");
    }
}
#[test]
fn namespace_calls_follow_only_explicit_namespace_bindings() {
    for (lang, a_path, b_path, source, consumer) in [
        (
            Language::Js,
            "a.ts",
            "b.ts",
            "export function f() {}",
            "import * as m from './a'; m.f(); unknown.f()",
        ),
        (
            Language::Python,
            "a.py",
            "b.py",
            "def f(): pass",
            "import a as m\nm.f()\nunknown.f()",
        ),
        (
            Language::Python,
            "pkg/sub/module.py",
            "app.py",
            "def f(): pass",
            "import pkg.sub.module\npkg . sub . module . f()\npkg.sub.module.extra.f()\ndef shadow(pkg):\n    pkg.sub.module.f()",
        ),
    ] {
        let a = super::super::analyze(lang, source);
        let b = super::super::analyze(lang, consumer);
        let units = [
            Unit {
                path: a_path,
                text: source,
                analysis: &a,
            },
            Unit {
                path: b_path,
                text: consumer,
                analysis: &b,
            },
        ];
        let names = Names::build(&units);
        let hits = &names.incoming[&Site { file: 0, local: 0 }];
        assert_eq!(hits.len(), 1);
        assert_eq!(
            b.calls[hits[0].local].span.start,
            consumer.find("f()").unwrap()
        );
    }
}
#[test]
fn arrow_body_and_regexp_are_distinct_from_outer_scope() {
    let text = "const p = /function fake(x) { target() }/; function target() {} const launch = (arg) => { target(); arg(); };";
    let a = super::super::analyze(Language::Js, text);
    assert!(
        !a.declarations
            .iter()
            .any(|d| &text[d.span.clone()] == "fake")
    );
    let names = Names::build(&[Unit {
        path: "a.js",
        text,
        analysis: &a,
    }]);
    let target = names.by_name["target"][0];
    let incoming = &names.incoming[&target];
    assert_eq!(incoming.len(), 1);
    let owner = a.calls[incoming[0].local].owner.unwrap();
    assert_eq!(&text[a.declarations[owner].span.clone()], "launch");
}

#[test]
fn named_export_and_configured_wildcard_follow_pinned_modules() {
    let specs = [
        (
            "pkg/tsconfig.json",
            r#"{"compilerOptions":{"paths":{"@app/*":["./src/*"]}}}"#,
        ),
        (
            "pkg/src/index.ts",
            "export { render as draw } from './impl';",
        ),
        (
            "pkg/src/impl.ts",
            "export function render(x:string):string; export function render(x:any){return x;}",
        ),
        (
            "pkg/page.ts",
            "import { draw } from '@app/index'; draw('hello');",
        ),
    ];
    let facts = specs.map(|(p, s)| {
        Language::for_path(p).map_or_else(Analysis::default, |l| super::super::analyze(l, s))
    });
    let units: Vec<_> = specs
        .iter()
        .zip(&facts)
        .map(|(&(path, text), analysis)| Unit {
            path,
            text,
            analysis,
        })
        .collect();
    let n = Names::build(&units);
    let call = units[3]
        .analysis
        .calls
        .iter()
        .position(|c| c.is_call)
        .unwrap();
    let targets = &n.targets[&Site {
        file: 3,
        local: call,
    }];
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|s| s.file == 2));
    assert_eq!(n.by_name["render"].len(), 1);
}
#[test]
fn jsx_template_text_and_component_props_have_distinct_bindings() {
    let text = "function Tag({title}: {title: string}) {return <span title={title}>title</span>;} const title = 'x'; const row = <Tag title={`${title}`} />;";
    let facts = super::super::analyze(Language::Js, text);
    let names = Names::build(&[Unit {
        path: "page.tsx",
        text,
        analysis: &facts,
    }]);
    let locals = &names.by_name["title"];
    assert_eq!(locals.len(), 1);
    let hits = &names.incoming[&locals[0]];
    assert_eq!(hits.len(), 1);
    assert_eq!(
        facts.calls[hits[0].local].span.start,
        text.rfind("title").unwrap()
    );
    let attr = text.find("<Tag title=").unwrap() + 5;
    let site = facts
        .calls
        .iter()
        .position(|c| c.span.start == attr)
        .unwrap();
    let d = &facts.declarations[names.targets[&Site {
        file: 0,
        local: site,
    }][0]
        .local];
    assert_eq!(d.span.start, text.find("title: string").unwrap());
}
#[test]
fn python_receivers_keyword_arguments_and_comprehension_scopes() {
    let text = "class Client:\n    def send(self, value):\n        self.name = value\n        return self.name\ndef keep(value): return value\nouter=1\nseq=[outer for outer in items]\nkeep(value=outer)\ndef nested():\n    def keep(value): return value\n    keep(value=1)\n";
    let a = super::super::analyze(Language::Python, text);
    let names = Names::build(&[Unit {
        path: "p.py",
        text,
        analysis: &a,
    }]);
    let definition = |offset| {
        let use_id = a.calls.iter().position(|u| u.span.start == offset).unwrap();
        a.declarations[names.targets[&Site {
            file: 0,
            local: use_id,
        }][0]
            .local]
            .span
            .start
    };
    assert_eq!(
        definition(text.rfind("name").unwrap()),
        text.find("name").unwrap()
    );
    assert_eq!(
        definition(text.find("[outer").unwrap() + 1),
        text.find("for outer").unwrap() + 4
    );
    assert_eq!(
        definition(text.rfind("outer").unwrap()),
        text.find("outer").unwrap()
    );
    assert_eq!(
        definition(text.find("value=outer").unwrap()),
        text.find("keep(value").unwrap() + 5
    );
    assert_eq!(
        definition(text.find("value=1").unwrap()),
        text.find("    def keep(value").unwrap() + "    def keep(".len()
    );
}

#[test]
fn expressions_are_named_only_within_their_body_and_new_uses_the_constructor() {
    let text = "class Task { constructor() {} } const build = function run() { run(); return new Task(); }; run();";
    let a = super::super::analyze(Language::Js, text);
    let mut saved = Vec::new();
    a.encode(&mut saved);
    let a = super::super::Analysis::decode(&mut saved.as_slice()).unwrap();
    let n = Names::build(&[Unit {
        path: "entry.ts",
        text,
        analysis: &a,
    }]);
    assert!(!n.by_name.contains_key("run"));
    let decl = |p| {
        let i = a.calls.iter().position(|u| u.span.start == p).unwrap();
        n.targets
            .get(&Site { file: 0, local: i })
            .map(|s| a.declarations[s[0].local].kind)
    };
    assert_eq!(
        decl(text.find("run();").unwrap()),
        Some("function_expression")
    );
    assert_eq!(decl(text.rfind("run();").unwrap()), None);
    assert_eq!(
        decl(text.find("new Task").unwrap() + 4),
        Some("method_definition")
    );
}

#[test]
fn typed_chain_requires_each_receiver_link() {
    let text = "function make(){ return { speed: 1 }; } interface Drawn { made: ReturnType<typeof make>; } const all=new Set<Drawn>(); for(const item of all){ consume(item.made.speed); } unknown.made.speed;";
    let a = super::super::analyze(Language::Js, text);
    let n = Names::build(&[Unit {
        path: "a.ts",
        text,
        analysis: &a,
    }]);
    let at = |offset| {
        a.calls
            .iter()
            .enumerate()
            .find(|(_, u)| u.span.start == offset)
            .and_then(|(i, _)| n.targets.get(&Site { file: 0, local: i }))
            .map(|v| a.declarations[v[0].local].span.start)
    };
    assert_eq!(
        at(text.find("made.speed").unwrap() + 5),
        Some(text.find("speed:").unwrap())
    );
    assert_eq!(at(text.rfind("speed").unwrap()), None);
}

#[test]
fn catch_method_keeps_callback_declarations_and_catch_clause_binds_only_its_parameter() {
    let text = "run().catch(error => { const host = error; }); try { run(); } catch (reason) { const delay = reason; }";
    let a = super::super::analyze(Language::Js, text);
    let n = Names::build(&[Unit {
        path: "x.ts",
        text,
        analysis: &a,
    }]);
    assert_eq!(n.by_name["host"].len(), 1);
    assert_eq!(n.by_name["delay"].len(), 1);
    for word in ["error", "reason"] {
        let id = a
            .calls
            .iter()
            .position(|x| x.span.start == text.rfind(word).unwrap())
            .unwrap();
        assert_eq!(
            a.declarations[n.targets[&Site { file: 0, local: id }][0].local].kind,
            "parameter"
        );
    }
}

#[test]
fn python_reexports_dotted_modules_and_wildcards_obey_their_binding() {
    let specs = [
        ("pkg/impl.py", "def send(): pass\ndef hidden(): pass\n"),
        (
            "pkg/__init__.py",
            "from .impl import send as go\n__all__=['go']\n",
        ),
        (
            "consumer.py",
            "from pkg import *\nfrom pkg import impl as module\nimport pkg.impl\ngo()\nmodule.send()\npkg.impl.send()\nhidden()\n",
        ),
    ];
    let facts = specs.map(|(_, s)| super::super::analyze(Language::Python, s));
    let units: Vec<_> = specs
        .iter()
        .zip(&facts)
        .map(|(&(path, text), analysis)| Unit {
            path,
            text,
            analysis,
        })
        .collect();
    let names = Names::build(&units);
    let send = names.by_name["send"][0];
    assert_eq!(
        names.incoming[&send]
            .iter()
            .filter(|s| s.file == 2 && facts[2].calls[s.local].is_call)
            .count(),
        3
    );
    assert!(!names.incoming.contains_key(&names.by_name["hidden"][0]));
}

#[test]
fn typed_imports_resolve_declaration_files_and_default_identifier_reexports() {
    let files = [
        (
            "p/tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":"./src"}}"#,
        ),
        ("p/src/thing.d.ts", "export interface Thing {}"),
        (
            "p/src/real.ts",
            "function render() {} export default render;",
        ),
        (
            "p/main.ts",
            "import type {Thing} from 'thing'; import run from 'real'; run();",
        ),
    ];
    let facts = files.map(|(p, s)| {
        Language::for_path(p).map_or_else(Analysis::default, |l| super::super::analyze(l, s))
    });
    let units: Vec<_> = files
        .iter()
        .zip(&facts)
        .map(|(&(path, text), analysis)| Unit {
            path,
            text,
            analysis,
        })
        .collect();
    let n = Names::build(&units);
    assert!(
        n.incoming[&n.by_name["Thing"][0]]
            .iter()
            .any(|s| s.file == 3)
    );
    assert!(
        n.incoming[&n.by_name["render"][0]]
            .iter()
            .any(|s| s.file == 3 && facts[3].calls[s.local].is_call)
    );
}

#[test]
fn dynamic_or_shadowed_receiver_never_inherits_an_outer_parameter_type() {
    let s = "class A:\n    def run(self): pass\n    def outer(self):\n        def inner(self):\n            self.run()\n        self.run()\n";
    let a = super::super::analyze(Language::Python, s);
    let n = Names::build(&[Unit {
        path: "a.py",
        text: s,
        analysis: &a,
    }]);
    let calls = &n.incoming[&n.by_name["run"][0]];
    assert_eq!(calls.len(), 1);
    assert_eq!(a.calls[calls[0].local].span.start, s.rfind("run").unwrap());
}

#[test]
fn namespaced_reexport_preserves_the_member_module() {
    let specs = [
        ("lib.ts", "export function send() {}"),
        ("index.ts", "export * as client from './lib';"),
        ("use.ts", "import {client} from './index'; client.send();"),
    ];
    let facts = specs.map(|(_, s)| super::super::analyze(Language::Js, s));
    let units: Vec<_> = specs
        .iter()
        .zip(&facts)
        .map(|(&(path, text), analysis)| Unit {
            path,
            text,
            analysis,
        })
        .collect();
    let n = Names::build(&units);
    let hits = &n.incoming[&n.by_name["send"][0]];
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].file, 2);
}

#[test]
fn methods_skip_python_class_locals_and_nested_js_functions_reset_this() {
    let py = "x=0\nclass C:\n    x=1\n    def run(self):\n        return x\n";
    let js =
        "class C { value=0; run() { function inner(){ return this.value; } return this.value; } }";
    let python_reference = py.rfind('x').unwrap();
    let inner_value = js.find("this.value").unwrap() + 5;
    for (path, text, lang, reference, expected) in [
        ("a.py", py, Language::Python, python_reference, Some(0)),
        ("a.js", js, Language::Js, inner_value, None),
    ] {
        let a = super::super::analyze(lang, text);
        let n = Names::build(&[Unit {
            path,
            text,
            analysis: &a,
        }]);
        let target = |at| {
            a.calls
                .iter()
                .position(|c| c.span.start == at)
                .and_then(|local| n.targets.get(&Site { file: 0, local }))
                .map(|v| a.declarations[v[0].local].span.start)
        };
        assert_eq!(target(reference), expected);
    }
}

#[test]
fn exported_namespace_members_and_ambient_signatures_survive_imports() {
    let files = [
        (
            "lib.ts",
            "export namespace Api { export function send() {} function hidden() {} }",
        ),
        (
            "types.d.ts",
            "export declare function parse(s: string): number\nexport default function decode(): string",
        ),
        (
            "use.ts",
            "import {Api} from './lib'; import read, {parse} from './types'; Api.send(); Api.hidden(); parse(''); read();",
        ),
    ];
    let facts = files.map(|(_, text)| super::super::analyze(Language::Js, text));
    let units: Vec<_> = files
        .iter()
        .zip(&facts)
        .map(|(&(path, text), analysis)| Unit {
            path,
            text,
            analysis,
        })
        .collect();
    let graph = Names::build(&units);
    for name in ["send", "parse", "decode"] {
        let site = graph.by_name[name][0];
        assert!(
            graph.incoming[&site]
                .iter()
                .any(|call| call.file == 2 && facts[2].calls[call.local].is_call),
            "{name}"
        );
    }
    assert!(!graph.incoming.contains_key(&graph.by_name["hidden"][0]));
}

#[test]
fn super_call_requires_builtin_and_one_written_parent() {
    let source = "class Base:\n    def save(self): pass\nclass Child(Base):\n    def run(self, super):\n        super().save()\nclass Other: pass\nclass Multiple(Base, Other):\n    def run(self):\n        super().save()\n";
    let analysis = super::super::analyze(Language::Python, source);
    let graph = Names::build(&[Unit {
        path: "a.py",
        text: source,
        analysis: &analysis,
    }]);
    assert!(!graph.incoming.contains_key(&graph.by_name["save"][0]));
}

#[test]
fn only_a_single_object_return_supplies_an_inferred_shape() {
    let source = "function one() {return {x: 1};}
        function twice() {return {x: 1}; return {x: 2};}
        function before() {return 0; return {x: 1};}
        function after() {return {x: 1}; return 0;}
        function outer() {function inner() {return 0;} return {x: 1};}";
    let analysis = super::super::analyze(Language::Js, source);
    let evidence = analysis.script.as_ref().unwrap();
    let mut shaped: Vec<_> = evidence.result_shapes.keys()
        .map(|&id| &source[analysis.declarations[id].span.clone()])
        .collect();
    shaped.sort_unstable();
    assert_eq!(shaped, ["one", "outer"]);
}
