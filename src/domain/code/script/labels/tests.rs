use super::*;
use crate::domain::code::{Analysis, analyze};

fn identities(files: &[(&str, &str)]) -> Vec<String> {
    let parsed: Vec<_> = files
        .iter()
        .map(|(p, s)| Language::for_path(p).map_or_else(Analysis::default, |lang| analyze(lang, s)))
        .collect();
    let units: Vec<_> = files
        .iter()
        .zip(&parsed)
        .map(|(&(path, text), analysis)| Unit {
            path,
            text,
            analysis,
        })
        .collect();
    let mut labels = HashMap::new();
    public_labels(&units, &mut labels);
    let mut result: Vec<_> = labels.into_values().collect();
    result.sort();
    result
}

#[test]
fn package_and_signature_owners() {
    let source = "export function read(key: number, { value }: Options) { const local = 1; }";
    let out = identities(&[
        (
            "p/package.json",
            r#"{"name":"old","extra":[null,1.2e-2,{"name":"nested"}],"name":"@pkg/lib","version":"1"}"#,
        ),
        ("p/tsconfig.json", "{}"),
        ("p/src/lib.ts", source),
    ]);
    assert_eq!(
        out,
        [
            "scip-typescript  scip-typescript npm @pkg/lib 1 src/`lib.ts`/read().",
            "scip-typescript  scip-typescript npm @pkg/lib 1 src/`lib.ts`/read().(key)"
        ]
    );
    assert!(
        identities(&[
            ("p/package.json", r#"{"name":"invalid",}"#),
            ("p/lib.ts", source)
        ])
        .is_empty()
    );
}

#[test]
fn differing_project_roots_withhold_global_identity() {
    assert!(
        identities(&[
            ("p/tsconfig.json", "{}"),
            ("p/sub/m.ts", "export const x = 1;")
        ])
        .is_empty()
    );
    assert_eq!(
        identities(&[("p/sub/m.ts", "export const x = 1;")]),
        ["scip-typescript  scip-typescript npm . . `m.ts`/x."]
    );
}

#[test]
fn python_global_class_and_method_parameters() {
    let out = identities(&[(
        "lib/net/read.py",
        "class Stream:\n    def read(self, count, _):\n        temporary = count\n",
    )]);
    let base = "scip-python  scip-python python workspace 0 `lib.net.read`/Stream#";
    let expected: Vec<_> = ["", "read().", "read().(count)", "read().(self)"]
        .iter()
        .map(|s| format!("{base}{s}"))
        .collect();
    assert_eq!(out, expected);
}
