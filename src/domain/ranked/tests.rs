use super::*;

#[test]
fn frozen_rank_and_excerpt_survive_reopen() {
    let mut index = RankedIndex::default();
    let bodies = [
        "foo bar pad pad",
        "foo gap bar pad",
        "foo bar foo bar",
        "bar pad pad pad",
    ];
    for (path, body) in ["a", "b", "c", "foo"].into_iter().zip(bodies) {
        index.add_file(path, body, &chunk_ranges(body, &[]));
    }
    let wire = encoded(&index);
    let restored = RankedIndex::read_from(wire.as_slice()).unwrap();
    for (query, mult) in [("foo-bar", 1.), ("foo-bar foo-bar", 2.)] {
        let (hits, overflow) = restored.search(query, |id| bodies[id]).unwrap();
        assert!(!overflow);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            (hits[0].file, hits[0].score, hits[0].excerpt.as_str()),
            (2, -1.375e-6 * mult, "[foo bar] [foo bar]")
        );
        assert_eq!(
            (hits[1].file, hits[1].score, hits[1].excerpt.as_str()),
            (0, -1e-6 * mult, "[foo bar] pad pad")
        );
    }
    assert!(RankedIndex::read_from(&wire[..wire.len() - 1]).is_err());
}

#[test]
fn unicode_parts_overlap_and_headings() {
    let body = "x\n".repeat(50) + "ÅngströmValue Café\u{301} __value\n" + &"x\n".repeat(20);
    let mut index = RankedIndex::default();
    let chunks = chunk_ranges(&body, &[]);
    assert_eq!(chunks.len(), 2);
    index.add_file("Å", &body, &chunks);
    for q in ["ångström", "VALUE", "cafe", "__value"] {
        let hits = index.search(q, |_| &body).unwrap().0;
        assert_eq!(hits.len(), 2, "{q}");
        if q == "cafe" {
            assert!(hits[0].excerpt.contains("[Café\u{301}]"));
        }
    }
    let chunks = chunk_ranges(&body, &[1, 52]);
    assert_eq!(
        chunks
            .iter()
            .map(|c| (c.first_line, c.last_line))
            .collect::<Vec<_>>(),
        [(1, 51), (52, 71)]
    );
}

#[test]
fn loads_only_winning_source_files_once() {
    let mut index = RankedIndex::default();
    let text = "needle\n".repeat(61);
    for i in 0..40 {
        index.add_file(&format!("p{i:02}"), &text, &chunk_ranges(&text, &[]));
    }
    let mut reads = std::collections::HashSet::new();
    let (hits, overflow) = index
        .search_with("needle", |f| {
            assert!(reads.insert(f));
            Ok::<_, String>(std::borrow::Cow::Borrowed(text.as_str()))
        })
        .unwrap();
    assert_eq!(hits.len(), 50);
    assert!(overflow);
}

#[test]
fn truncation_can_end_in_the_middle_of_a_folded_scalar() {
    let prefix = "a".repeat(32767);
    let body = format!("{prefix}あbbb");
    let mut index = RankedIndex::default();
    index.add_file("p", &body, &chunk_ranges(&body, &[]));
    let hits = index
        .search(&format!("{prefix}いccc"), |_| &body)
        .unwrap()
        .0;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].excerpt, format!("[{body}]"));
}

#[test]
fn frozen_sentence_window_and_literal_punctuation() {
    let text = format!(
        "{} end.  The needle is here: {}",
        "a ".repeat(19),
        "later ".repeat(23)
    );
    let mut index = RankedIndex::default();
    index.add_file("x", &text, &chunk_ranges(&text, &[]));
    let hits = index.search("needle", |_| &text).unwrap().0;
    assert_eq!(
        hits[0].excerpt,
        format!("…The [needle] is here: {}…", vec!["later"; 14].join(" "))
    );
}

#[test]
fn cross_index_markdown_release_witness() {
    let guide = "# Guide\n## Setup\n🙂 needle\n### Child\nagain\n";
    let refs = "[Setup](guide.md#setup)\n";
    let mut index = RankedIndex::default();
    index.add_file("guide.md", guide, &chunk_ranges(guide, &[1, 2, 4]));
    index.add_file("refs.md", refs, &chunk_ranges(refs, &[]));
    let (hits, overflow) = index.search("needle", |id| [guide, refs][id]).unwrap();
    assert!(!overflow);
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!((hit.file, hit.line, hit.last_line), (0, 2, 3));
    assert_eq!(hit.excerpt, "## Setup\n🙂 [needle]\n");
    assert_eq!(hit.score.to_bits(), (-0.8104588229790644f64).to_bits());
}

struct CountedExtent<'a> {
    wire: &'a [u8],
    reads: &'a mut usize,
}

impl crate::domain::content_lookup::ExtentRead for CountedExtent<'_> {
    fn length(&self) -> u64 {
        self.wire.len() as u64
    }

    fn read(&mut self, offset: u64, out: &mut [u8]) -> std::io::Result<()> {
        let start = offset as usize;
        out.copy_from_slice(&self.wire[start..start + out.len()]);
        *self.reads += out.len();
        Ok(())
    }
}

#[test]
fn selective_reads_preserve_score_bits_and_skip_cold_spans() {
    let mut index = RankedIndex::default();
    let paths = ["z", "a", "noise"];
    let bodies = [
        "x\n".repeat(80) + "foo bar needle. Unicode Café 🦀.",
        "foo-x foo bar needle Café".into(),
        "unrelated filler ".repeat(65_536),
    ];
    for (p, body) in paths.iter().zip(&bodies) {
        index.add_file(p, body, &chunk_ranges(body, &[]));
    }
    let wire = encoded(&index);
    for q in [
        "needle",
        "foo-bar foo-bar café",
        "x",
        "needle nope 🦀 bar foo café foo-bar x a",
        "unknown",
    ] {
        let mut reads = 0;
        let mut source_ranges = vec![];
        let args = RankedIndex::search_extent(
            CountedExtent {
                wire: &wire,
                reads: &mut reads,
            },
            ExtentCatalog {
                count: paths.len(),
                path: |f| paths[f],
                source: |f: usize, r: Range<u64>| {
                    assert_ne!(f, 2);
                    source_ranges.push((f, r.clone()));
                    Ok(bodies[f][r.start as usize..r.end as usize].into())
                },
            },
            q,
        )
        .unwrap();
        let expected = index.search(q, |f| &bodies[f]).unwrap();
        assert_eq!(expected.1, args.1);
        let project = |h: &RankedHit| {
            (
                h.file,
                h.line,
                h.last_line,
                h.score.to_bits(),
                h.excerpt.clone(),
            )
        };
        assert_eq!(
            args.0.iter().map(project).collect::<Vec<_>>(),
            expected.0.iter().map(project).collect::<Vec<_>>()
        );
        assert!(reads < wire.len() / 10, "{q}: read {reads}/{}", wire.len());
        if q == "unknown" {
            assert!(source_ranges.is_empty());
        }
    }
}

#[test]
fn refresh_matches_fresh_wire_with_shifted_rows_and_document_frequencies() {
    for seed in 0..16 {
        compare_refreshed_documents(seed);
    }
}

fn compare_refreshed_documents(seed: usize) {
    fn body(i: usize, salt: usize) -> String {
        ("\n".repeat(i % 3) + &format!("SharedKey group{} value-{} café shared\n", i % 4, salt))
            .repeat(match i % 5 {
                0 => 62,
                1 => 0,
                _ => 1,
            })
    }
    let add = |rank: &mut RankedIndex, id: usize, salt| {
        let text = body(id, salt);
        rank.add_file(&format!("p{id:02}"), &text, &chunk_ranges(&text, &[]));
    };
    // Drop, insert, change and retain documents at all positions, including
    // tokenless files and overlapping multi-chunk documents.
    let mut old = RankedIndex::default();
    let prior_ids: Vec<_> = (0..22).filter(|i| (i + seed) % 5 != 1).collect();
    for &i in &prior_ids {
        add(&mut old, i, i);
    }
    let ids: Vec<_> = (0..22).filter(|i| (i + seed) % 7 != 1).collect();
    let salt = |i| if (i + seed) % 5 == 2 { 104 } else { i };
    let reuse: Vec<_> = ids
        .iter()
        .map(|&i| {
            (salt(i) == i)
                .then(|| prior_ids.binary_search(&i).ok())
                .flatten()
        })
        .collect();
    let mut generated = Vec::new();
    let updated = RankedIndex::refresh(old, &reuse, |r, new| {
        generated.push(new);
        add(r, ids[new], salt(ids[new]));
    })
    .unwrap();
    assert_eq!(
        generated,
        (0..ids.len())
            .filter(|&i| reuse[i].is_none())
            .collect::<Vec<_>>()
    );
    let mut fresh = RankedIndex::default();
    for &i in &ids {
        add(&mut fresh, i, salt(i));
    }
    assert_eq!(encoded(&updated), encoded(&fresh), "seed {seed}");
}

fn apply_patch(bytes: &[u8], edits: &[patch::Edit<'_>]) -> Vec<u8> {
    let plan = patch::patch(
        bytes.len() as u64,
        |r| Ok(bytes[r.start as usize..r.end as usize].to_vec()),
        edits,
    )
    .unwrap()
    .unwrap();
    let mut actual = vec![];
    for piece in plan.pieces {
        match piece {
            patch::Piece::Bytes(b) => actual.extend(b),
            patch::Piece::Borrow(r) => {
                actual.extend_from_slice(&bytes[r.start as usize..r.end as usize])
            }
        }
    }
    actual
}

fn encoded(index: &RankedIndex) -> Vec<u8> {
    let mut bytes = Vec::new();
    index.write_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn grouped_rank_patch_preserves_exact_wire_after_repeated_edits() {
    let names = ["left.md", "middle.md", "empty", "right.md", "z.md"];
    let initial: Vec<String> = [
        "common only-left\n",
        "common pair pair\n",
        "",
        "x COMMON shared\n",
        "unique only-right\n",
    ]
    .into_iter()
    .map(|s| s.repeat(76))
    .collect();
    let chunks = |s: &str| {
        chunks_from_line_ends(
            &s.split_inclusive('\n')
                .scan(0, |end, line| {
                    *end += line.len();
                    Some(*end)
                })
                .collect::<Vec<_>>(),
            &[],
        )
    };
    let encode = |texts: &[String]| {
        let mut rank = RankedIndex::default();
        for (name, body) in names.iter().zip(texts) {
            rank.add_file(name, body, &chunks(body));
        }
        encoded(&rank)
    };
    let mut texts = initial;
    let mut bytes = encode(&texts);
    for changes in [
        vec![(0, "☃ ONE\n"), (1, ""), (3, "shared one onE\n")],
        vec![(0, ""), (2, "new unique only-right"), (4, "")],
        vec![(1, "common unique shared shared"), (4, "aabb zz")],
        vec![(2, "novel\n"), (3, ""), (4, "")],
    ] {
        let mut next = texts.clone();
        for &(id, body) in &changes {
            next[id] = body.repeat(75);
        }
        let edits = changes
            .iter()
            .map(|&(id, _)| patch::Edit {
                file: id,
                path: names[id],
                old: &texts[id],
                new: &next[id],
                chunks: chunks(&next[id]),
            })
            .collect::<Vec<_>>();
        let actual = apply_patch(&bytes, &edits);
        assert_eq!(actual, encode(&next));
        texts = next;
        bytes = actual;
    }
}

fn assert_token_streams_match(text: &str) {
    let mut words=vec![];
    let mut parts=vec![];
    super::token::tokenize(text, |k,s| words.push((k.to_vec(),s)));
    super::token::identifier_parts(text, |base,p| super::token::tokenize(p, |k,s| parts.push((k.to_vec(),base+s.start..base+s.end))));
    let mut got_words=vec![];
    let mut got_parts=vec![];
    super::token::ranked_words(text, |part,k,s| if part {got_parts.push((k.to_vec(),s))} else {got_words.push((k.to_vec(),s))});
    assert_eq!(words,got_words);
    assert_eq!(parts,got_parts);
}

#[test]
fn fused_rank_words_and_packed_groups_match_reference() {
    let mut rank = RankedIndex::default();
    let mut bulk = super::BulkRank::default();
    let mut large = String::new();
    let alphabet = [' ', 'a', 'B', 'C', 'd', '4', '_', '.', '\n', '\0', 'Ł', 'ñ', 'Σ', '\u{301}', '🙂', '\u{ffff}', '\u{fffe}', '中'];
    let mut seed = 13u32;
    for _ in 0..20000 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        large.push(alphabet[((seed >> 8) as usize) % alphabet.len()]);
    }
    let long_key = format!("abc{}_{}{}", "Z".repeat(40000), "α".repeat(30000), "XYZ");
    for (i,text) in ["", "_hHTTPServer abcNew\n\n___TTT_", &large, &long_key].into_iter().enumerate() {
        assert_token_streams_match(text);
        let path=format!("src/HTTP_File_{i}.rs");
        let chunks=chunk_ranges(text,&[]);
        rank.add_file(&path,text,&chunks);
        bulk.add_file(&path,text,&chunks);
    }
    let mut a=Vec::new(); let mut b=Vec::new();
    rank.write_to(&mut a).unwrap(); bulk.write_to(&mut b).unwrap();
    assert_eq!(a,b);
}

#[test]
fn joined_packed_groups_keep_global_file_order_and_rank_bytes() {
    let mut rank = RankedIndex::default();
    let mut pieces = Vec::new();
    for batch in 0..6 {
        let mut bulk = super::BulkRank::at(batch * 44);
        for file in batch * 44..(batch+1)*44 {
            let path = format!("src/f_{file}.txt");
            let body = if file % 7 == 0 { String::new() } else {
                format!("id_{file} foo_bar FooBar with 🧩token_42\nunrelated line{}\n", file%4).repeat(file%83)
            };
            let chunks = chunk_ranges(&body,&[]);
            rank.add_file(&path,&body,&chunks);
            bulk.add_file(&path,&body,&chunks);
        }
        pieces.push(bulk.into_shard());
    }
    let merged = super::ShardSet::new(pieces).unwrap();
    let mut expected = vec![];let mut got = vec![];
    rank.write_to(&mut expected).unwrap();
    merged.write_to(&mut got).unwrap();
    assert_eq!(expected.len(),got.len());
    assert!(expected==got);
}

#[test]
fn legacy_grouped_directory_validates_file_bases_separately_from_offsets() {
    let mut bytes = b"CIRANK03".to_vec();
    bytes.extend([0, 0, 0]); // Empty file, row and term tables.
    for value in [9u64, 11] {
        bytes.extend(value.to_le_bytes());
    }
    let base_at = bytes.len();
    bytes.extend(0u64.to_le_bytes());
    bytes.extend_from_slice(b"RANKDIR3");
    for value in [0u64, 0, 11, 10, 0, 0] {
        bytes.extend(value.to_le_bytes());
    }
    assert!(RankedIndex::read_from(bytes.as_slice()).is_ok());
    bytes[base_at] = 1;
    assert!(RankedIndex::read_from(bytes.as_slice()).is_err());
}

#[test]
fn ranked_row_block_rejects_order_totals_and_trailing_data() {
    let mut dir = cursor::Directory {
        format: storage::Format::Tree,
        files: 2,
        terms: 0,
        offset: 42,
        rows_end: 40,
        tokens: 2,
        rows: 2,
    };
    let mut rows = vec![2, 0, 1, 0, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 1, 0, 0, 1, 0];
    let mut offsets = Vec::new();
    dir.row_block(&rows)
        .unwrap()
        .visit(|at, start, _| offsets.push((at, start)))
        .unwrap();
    assert_eq!(offsets, [(22, 0), (31, 1)]);
    rows.swap(1, 10);
    assert!(dir.row_block(&rows).unwrap().visit(|_, _, _| {}).is_err());
    rows.swap(1, 10);
    rows.push(0);
    assert!(dir.row_block(&rows).unwrap().visit(|_, _, _| {}).is_err());
    rows.pop();
    dir.tokens = 3;
    assert!(dir.row_block(&rows).unwrap().visit(|_, _, _| {}).is_err());
}
