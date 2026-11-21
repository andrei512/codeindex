use super::*;
use std::path::Path;
fn parse(path: &Path, text: &str) -> Parsed {
    Parsed::parse(path.to_str().unwrap(), text)
}
#[test]
fn builds_nested_sections_from_rendered_headings() {
    let text = "intro\r\n# *One* [label](#one-label) `x`\r\n## A\n## A-1\n## A\n\n```md\n# Fake\n[skip](no.md)\n```\nNext **topic**\n==========\nlast";
    let parsed = parse(Path::new("docs/readme.md"), text);
    let s = &parsed.sections;
    assert_eq!(
        s.iter().map(|s| s.anchor.as_str()).collect::<Vec<_>>(),
        ["", "one-label-x", "a", "a-1", "a-2", "next-topic"]
    );
    assert_eq!(s[0].end, text.len());
    assert_eq!(
        (s[1].title.as_str(), s[1].parent, s[1].line),
        ("One label x", Some(0), 2)
    );
    assert_eq!((s[2].parent, s[2].end), (Some(1), s[3].start));
    assert_eq!(s[1].end, s[5].start);
    assert_eq!(s[5].line, 11);
    assert_eq!(s[5].end, text.len());
    assert_eq!(parsed.links.len(), 1);
    assert_eq!(parsed.links[0].section, 1);
}

#[test]
fn resolves_references_and_decodes_once_within_repository() {
    let text = "[my ref][r]\n\n[r]: ../My%20Guide.md?mode=preview#caf%C3%A9\n\n# Café\n[self](#caf%C3%A9) [root](/a/b.md) [encoded](%2523.md) [n](../../escape) [e](https://x) [n](//host) [mail](mailto:x@y) <a@b.com> [bad](%FF.md)";
    let parsed = parse(Path::new("docs/index.md"), text);
    let links = parsed
        .links
        .iter()
        .map(|l| (l.target.as_str(), l.anchor.as_str(), l.section, l.line))
        .collect::<Vec<_>>();
    assert_eq!(
        links,
        [
            ("My Guide.md", "café", 0, 1),
            ("docs/index.md", "café", 1, 6),
            ("a/b.md", "", 1, 6),
            ("docs/%23.md", "", 1, 6)
        ]
    );
    assert_eq!(parsed.links[1].offset, text.find("[self]").unwrap());
    assert_eq!(parsed.sections[1].anchor, "café");
}

#[test]
fn respects_explicit_ids_and_punctuation() {
    let parsed = parse(
        Path::new("README.md"),
        "# Whatever {#ID}\n# **A/B** &amp; C_2\n# ID {#ID}\n",
    );
    assert_eq!(parsed.sections[1].anchor, "ID");
    assert_eq!(parsed.sections[2].title, "A/B & C_2");
    assert_eq!(parsed.sections[2].anchor, "ab--c_2");
    assert_eq!(parsed.sections[3].anchor, "ID-1");
}
#[test]
fn section_spans_keep_container_markers_and_use_lf_lines() {
    let text = "intro\rcontinued\n> # Quote\n> text\n\n- # List item\n";
    let parsed = parse(Path::new("a.md"), text);
    assert_eq!(parsed.sections[1].line, 2);
    assert_eq!(
        &text[parsed.sections[1].start..parsed.sections[1].end],
        "> # Quote\n> text\n\n"
    );
    assert_eq!(parsed.sections[2].line, 5);
    assert_eq!(&text[parsed.sections[2].start..], "- # List item\n");
}

#[test]
fn nested_sections_chunks_and_handles_survive_reopen() {
    let mut text = "preface\n> - # Top {#x}\n>   ## Child\n".to_owned();
    for _ in 0..60 {
        text.push_str("body\n");
    }
    text.push_str("# Sibling\n");
    let p = Parsed::parse("file#1.md", &text);
    assert_eq!(
        p.breadcrumbs(2)
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>(),
        ["file#1.md", "Top", "Child"]
    );
    let chunks = p.chunks(&text);
    assert_eq!(
        chunks
            .iter()
            .map(|c| (c.section, c.line))
            .collect::<Vec<_>>(),
        [(0, 1), (1, 2), (2, 3), (2, 53), (3, 64)]
    );
    assert_eq!(p.section_at(chunks[1].start), 1);
    let handle = format_handle("file#1.md", "A%23");
    assert_eq!(
        parse_handle(&handle),
        Some(("file#1.md".to_owned(), Some("A%23".to_owned())))
    );
    let mut bytes = Vec::new();
    p.write_to(&mut bytes).unwrap();
    assert_eq!(Parsed::read_from(bytes.as_slice()).unwrap(), p);
    assert!(Parsed::read_from(&bytes[..bytes.len() - 1]).is_err());
    bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(Parsed::read_from(bytes.as_slice()).is_err());
}

#[test]
fn anchors_use_the_frozen_unicode_16_general_categories() {
    // U+1E4EC is a Unicode 15 combining mark. U+1C89 is a Unicode 16 letter.
    let text = "# A\u{1e4ec}/\u{1c89}\n";
    let p = Parsed::parse("index.md", text);
    assert_eq!(p.sections[1].anchor, "a\u{1e4ec}\u{1c8a}");
}

#[test]
fn moving_cached_sections_requalifies_only_local_addresses() {
    let text = "# Setup\n[x](../guide.md#setup) [self](#setup) [root](/root.md) [external](https://example.org)";
    let before = Parsed::parse("start.md", text);
    assert_eq!(before.links.len(), 2);
    let mut wire = Vec::new();
    before.write_to(&mut wire).unwrap();
    let mut moved = Parsed::read_from(&wire[..]).unwrap();
    moved.relocate("docs/new.md");
    assert_eq!(moved.links.len(), 3);
    assert_eq!(moved, Parsed::parse("docs/new.md", text));
}
