use super::*;
use std::path::Path;
fn parse(path: &Path, text: &str) -> Parsed {
    Parsed::parse(path.to_str().unwrap(), text)
}
#[test]
fn image_descriptions_isolate_links_from_the_surrounding_label() {
    for (label, targets) in [
        ("[outer ![alt [inner](i)](img)](o)", vec!["o", "i"]),
        ("[outer ![alt [inner](i)][missing]](o)", vec!["i"]),
        (
            "[outer [sibling](s) ![alt [inner](i)](img)](o)",
            vec!["s", "i"],
        ),
        ("[outer ![alt](img[fake](f))](o)", vec!["o"]),
    ] {
        let parsed = Parsed::parse("root.md", label);
        assert_eq!(
            parsed
                .links
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            targets,
            "{label}"
        );
    }
}

#[test]
fn streaming_heading_lines_match_navigation_at_block_boundaries() {
    let fragments = [
        "# atx", "> quoted", "continued", "- list", "    continuation", "---",
        "===", "[label]: url", "  'title'", "~~~", "<div>", "", "	indented",
    ];
    for first in fragments {
        for second in fragments {
            for third in fragments {
                let text = format!("{first}\r\n{second}\r\n{third}\r\n===\r\n# last");
                let navigation = Parsed::parse("doc.md", &text);
                let mut actual = Vec::new();
                visit_heading_lines(&text, |line| actual.push(line));
                assert_eq!(actual, navigation.sections[1..].iter().map(|s| s.line).collect::<Vec<_>>(), "{text}");
            }
        }
    }
}

#[test]
fn score_units_match_navigation_with_forward_references_and_container_headings() {
    let text = "# [First][ref] {#custom}\r\n\r\n> second [link](docs.md)\r\n> line\r\n> ====\r\n\r\n[ref]:\r\n  /guide.md\r\n\r\n# [ref]\n";
    let navigation = Parsed::parse("guide.md", text);
    let headings = Parsed::headings("guide.md", text);
    assert_eq!(headings.sections, navigation.sections);
    assert!(headings.links.is_empty() && headings.destinations.is_empty());
    let units = score_sections("guide.md", text);
    assert_eq!(units.len(), 4);
    for (unit, section) in units.iter().zip(&navigation.sections) {
        assert_eq!((&unit.title, unit.start, unit.line), (&section.title, section.start, section.line));
    }
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
fn first_reference_definition_wins_across_paragraphs() {
    for first in ["[id]: first.md", "[id]:\n  first.md"] {
        for second in ["[ID]: second.md", "[ID]:\n  second.md"] {
            let text = format!("[before][id]\n\n{first}\n\n{second}\n\n# [after][ID]");
            let doc = Parsed::parse("guide.md", &text);
            assert_eq!(
                doc.links
                    .iter()
                    .map(|link| link.target.as_str())
                    .collect::<Vec<_>>(),
                ["first.md", "first.md"],
                "{text}"
            );
            assert_eq!(doc.sections[1].title, "after");
        }
    }
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
    let mut unread = wire.as_slice();
    let (headings, path) = read_headings(&mut unread).unwrap();
    assert_eq!(headings, before.sections);
    assert_eq!(path, "start.md");
    assert_eq!(&unread[..8], &3u64.to_le_bytes()); // Raw destinations include the escaping link.
    let (headings, hits) = read_selected_links(&wire[..], [1].into_iter()).unwrap();
    assert_eq!(headings, before.sections);
    assert_eq!(hits, before.links[1..]);
    assert!(read_selected_links(&wire[..], [2].into_iter()).is_err());
    let mut moved = Parsed::read_from(&wire[..]).unwrap();
    moved.relocate("docs/new.md");
    assert_eq!(moved.links.len(), 3);
    assert_eq!(moved, Parsed::parse("docs/new.md", text));
}

#[test]
fn heading_and_table_backlinks_point_to_the_original_label_bytes() {
    let text = "# [**Café**](dest.md)\r\n\r\n| Label |\r\n| --- |\r\n| a\\|b [second](dest.md) |\r\n";
    let parsed = Parsed::parse("guide.md", text);
    let (headings, hits) = Parsed::backlinks("guide.md", text, ("dest.md", None), 50);
    assert_eq!(headings, Parsed::headings("guide.md", text).sections);
    assert_eq!(headings[1].title, "Café");
    assert_eq!(hits, parsed.links);
    let locations: Vec<_> = hits.iter().map(|hit| (hit.line, hit.offset, hit.section)).collect();
    assert_eq!(locations, [
        (1, text.find("[**Café**]").unwrap(), 1),
        (5, text.find("[second]").unwrap(), 1),
    ]);
}

#[test]
fn malformed_autolinks_keep_inner_backlinks_visible() {
    let text = "# <bad_@[inner](dest.md)>\n\n<bad_@ bad [nested](dest.md)>\n";
    let (sections, hits) = Parsed::backlinks("guide.md", text, ("dest.md", None), 10);
    assert_eq!(sections[1].title, "<bad_@inner>");
    assert_eq!(hits.iter().map(|hit| hit.offset).collect::<Vec<_>>(), [
        text.find("[inner]").unwrap(), text.find("[nested]").unwrap(),
    ]);
}

#[test]
fn backlink_limit_counts_matches_and_preserves_later_heading_anchors() {
    let text = "# One\n[a](other.md) [b](target.md) [c](target.md)\n\n# [Later][id]\n\n[id]: target.md\n";
    let expected = Parsed::parse("guide.md", text);
    let matching: Vec<_> = expected.links.iter().filter(|link| link.target == "target.md").collect();
    assert_eq!(matching.len(), 3);
    for limit in 0..=4 {
        let (sections, hits) = Parsed::backlinks("guide.md", text, ("target.md", None), limit);
        assert_eq!(sections, expected.sections);
        assert_eq!(sections.last().unwrap().anchor, "later");
        assert!(hits.iter().eq(matching.iter().copied().take(limit)));
    }
}

#[test]
fn reference_labels_follow_escape_parity_and_count_unicode_characters() {
    let text = |key: &str| format!("# Chapter [read][{key}]\n\n[{key}]: guide.md\n");
    for key in [r"x\]:section".to_owned(), "雪".repeat(999)] {
        let page = Parsed::parse("index.md", &text(&key));
        assert_eq!(page.links.len(), 1);
        assert_eq!(page.links[0].target, "guide.md");
        assert_eq!(page.sections.last().unwrap().title, "Chapter read");
    }
    for key in [
        r"x\\]:section".to_owned(),
        "a".repeat(1000),
        "雪".repeat(1000),
    ] {
        assert!(Parsed::parse("index.md", &text(&key)).links.is_empty());
    }
}

#[test]
fn reference_titles_consume_only_the_rows_they_own() {
    let text = format!(
        r#"# [read][id]

[id]: target.md
"{} first
[hidden](wrong.md) last"

[invalid](wrong.md (unescaped(nested))
[ok](right.md (escaped\(open))
"#,
        "t".repeat(5000)
    );
    let parsed = Parsed::parse("guide.md", &text);
    assert_eq!(parsed.sections[1].title, "read");
    let links: Vec<_> = parsed
        .links
        .iter()
        .map(|l| (l.target.as_str(), l.offset))
        .collect();
    assert_eq!(
        links,
        [
            ("target.md", text.find("[read]").unwrap()),
            ("right.md", text.find("[ok]").unwrap())
        ]
    );
}

#[test]
fn reference_titles_and_fences_end_at_their_list_item() {
    let text = "[key]: target.md\n- \"[visible](first.md)\"\n- ```\n- [next](second.md)\n- ```\n\n[read][key]";
    let parsed = Parsed::parse("doc.md", text);
    let links: Vec<_> = parsed
        .links
        .iter()
        .map(|l| (l.target.as_str(), l.offset))
        .collect();
    assert_eq!(
        links,
        [
            ("first.md", text.find("[visible]").unwrap()),
            ("second.md", text.find("[next]").unwrap()),
            ("target.md", text.find("[read]").unwrap())
        ]
    );
}

#[test]
fn blank_row_outside_a_quoted_list_closes_its_indentation() {
    let text =
        "[read][key]\n\n> - [key]: target.md\n    \n    \"[code](hidden.md)\"\n\n[after](after.md)";
    let parsed = Parsed::parse("doc.md", text);
    assert_eq!(
        parsed
            .links
            .iter()
            .map(|l| l.target.as_str())
            .collect::<Vec<_>>(),
        ["target.md", "after.md"]
    );
}

#[test]
fn emphasis_uses_visible_delimiters_and_preserves_link_offsets() {
    let text = "# *orphan [label](file*name.md)\n\n## x***y**z*\n";
    let parsed = Parsed::parse("doc.md", text);
    assert_eq!(parsed.sections[1].title, "*orphan label");
    assert_eq!(parsed.sections[2].title, "xyz");
    assert_eq!(parsed.links[0].offset, text.find("[label]").unwrap());
    assert_eq!(parsed.links[0].target, "file*name.md");
}

#[test]
fn emphasis_inside_a_link_label_cannot_consume_an_outer_opener() {
    let text = "# *outside [end*](doc.md)\n\n## _word \\_ and `_`\n";
    let parsed = Parsed::parse("doc.md", text);
    assert_eq!(parsed.sections[1].title, "*outside end*");
    assert_eq!(parsed.sections[2].title, "_word _ and _");
}

#[test]
fn link_separators_keep_non_ascii_spaces_in_the_address() {
    let text = "[bad](<wrong.md>\u{a0}) [ok](<right.md>\t\"title\"\t) [literal](x\u{2003}y.md)";
    let parsed = Parsed::parse("doc.md", text);
    let links: Vec<_> = parsed
        .links
        .iter()
        .map(|l| (l.target.as_str(), l.offset))
        .collect();
    assert_eq!(
        links,
        [
            ("right.md", text.find("[ok]").unwrap()),
            ("x\u{2003}y.md", text.find("[literal]").unwrap())
        ]
    );
}

#[test]
fn reference_labels_distinguish_non_ascii_spaces() {
    let text = "[ascii][two words] [unicode][two\u{2003}words] [tab][two\twords]\n\n[two words]: first.md\n[two\u{2003}words]: second.md\n";
    let parsed = Parsed::parse("doc.md", text);
    let links: Vec<_> = parsed
        .links
        .iter()
        .map(|l| (l.target.as_str(), l.offset))
        .collect();
    assert_eq!(
        links,
        [
            ("first.md", 0),
            ("second.md", text.find("[unicode]").unwrap()),
            ("first.md", text.find("[tab]").unwrap())
        ]
    );
}

#[test]
fn unicode_line_separators_are_ordinary_emphasis_neighbors() {
    let text = "# a*\u{2028}b* and a_\u{a0}b_";
    let parsed = Parsed::parse("doc.md", text);
    assert_eq!(parsed.sections[1].title, "a\u{2028}b and a_\u{a0}b_");
}

#[test]
fn reference_suffixes_use_identifier_delimiters_inside_visible_markup() {
    for (suffix, identifier) in [
        ("a `]` rest", "a `"),
        ("a <i x=\"]\"> rest", "a <i x=\""),
        ("a <!-- ] -->", "a <!-- "),
        ("\u{a0}", "\u{a0}"),
    ] {
        let text = format!("λ [outer [inner][{suffix}]](wrong.md)\n\n[{identifier}]: target.md");
        let parsed = Parsed::parse("index.md", &text);
        let links: Vec<_> = parsed
            .links
            .iter()
            .map(|link| (link.target.as_str(), link.offset))
            .collect();
        assert_eq!(
            links,
            [("target.md", text.find("[inner]").unwrap())],
            "{text}"
        );
    }
}
