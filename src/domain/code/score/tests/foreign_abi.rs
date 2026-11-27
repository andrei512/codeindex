use super::*;

#[test]
fn fsevents_callback_has_no_parameters_finding() {
    let source = include_str!("../../../../adapters/watcher/native.rs");
    let score = scored(Language::Rust, source, "native.rs", None).1.unwrap();
    assert!(
        !score
            .findings
            .iter()
            .any(|finding| finding.rule == Rule::Arguments)
    );
}

#[test]
fn foreign_abi_omits_only_bare_wildcards() {
    let source = r#"
        extern "C" fn callback(
            _: *const Result<u8, u16>,
            _named: i32, (x, _): (i32, i32), a: i32, b: i32, c: i32,
        ) {}
        extern "C" fn business(a:i32,b:i32,c:i32,d:i32,e:i32,f:i32) {}
        extern fn implicit(_:i32,_:i32,_:i32,_:i32,_:i32,_:i32) {}
        unsafe fn ordinary(_:i32,_:i32,_:i32,_:i32,_:i32,_:i32) {}
        extern "Rust" fn native(_:i32,_:i32,_:i32,_:i32,_:i32,_:i32) {}
        extern r"Rust" fn raw_native(_:i32,_:i32,_:i32,_:i32,_:i32,_:i32) {}
        extern "Ru\x73t" fn escaped_native(_:i32,_:i32,_:i32,_:i32,_:i32,_:i32) {}
    "#;
    let score = scored(Language::Rust, source, "abi.rs", None).1.unwrap();
    let findings: Vec<_> = score
        .findings
        .iter()
        .filter(|finding| finding.rule == Rule::Arguments)
        .map(|finding| (finding.unit.as_str(), finding.value))
        .collect();
    assert_eq!(
        findings,
        [
            ("callback", 5),
            ("business", 6),
            ("ordinary", 6),
            ("native", 6),
            ("raw_native", 6),
            ("escaped_native", 6),
        ]
    );
}

#[test]
fn foreign_abi_callback_body_still_receives_findings() {
    let body = "if condition() { work(); }\n".repeat(12);
    let source = format!("extern \"C\" fn callback(_: i32) {{\n{body}}}");
    let score = scored(Language::Rust, &source, "abi.rs", None).1.unwrap();
    assert!(
        score
            .findings
            .iter()
            .any(|finding| { finding.rule == Rule::Complexity && finding.unit == "callback" })
    );
}
