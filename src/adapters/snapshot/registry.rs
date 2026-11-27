//! Compatibility of *persisted* facts. Display and query-only changes do not
//! participate. All IDs are permanent. A revision may migrate only along an
//! explicitly declared edge, never just because its numeric value is smaller.
use crate::application::index::layout::{
    CONTENT, FILES, MD, N, PATHGRAM, RANK, REL, SOURCE, SYMBOLS,
};
use std::io;

const MAGIC: &[u8; 8] = b"CIXRULE1";
// Protocol 3 can publish grouped trigram and ranked directories.
const WRITER_PROTOCOL: u16 = 3;
const ALL: u8 = u8::MAX;
const fn bit(product: usize) -> u8 {
    1 << (product - 1)
}

// Extent type is (product tag, codec tag). The v2 piece directory changes
// the addressing of the payload, while preserving its logical byte stream.
pub(crate) const PRODUCT_TAGS: [u32; N] = [0, 0, 1, 2, 3, 4, 5, 6];
pub(crate) const CODEC_TAGS: [u32; N] = [32, 9, 32, 3, 32, 32, 32, 32];

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Migration {
    rebuild: u8,
}
impl Migration {
    pub(crate) fn current() -> Self {
        Self { rebuild: 0 }
    }
    pub(crate) fn reusable(&self) -> bool {
        self.rebuild == 0
    }
    pub(crate) fn requires_rebuild(&self, product: usize) -> bool {
        !(1..=N).contains(&product) || self.rebuild & bit(product) != 0
    }
}

struct Rule {
    id: u16,
    revision: u16,
    // Direct, tested source revisions. Same meaning => leave revision alone.
    from: &'static [u16],
    // Conservative transitive consumers for the native product layout.
    products: u8,
}
impl Rule {
    fn upgrade(&self, id: u16, revision: u16) -> io::Result<u8> {
        if id != self.id {
            return Err(incompatible("unknown required checkpoint contract"));
        }
        if revision == self.revision {
            return Ok(0);
        }
        if self.from.contains(&revision) {
            return Ok(self.products);
        }
        Err(incompatible(
            "checkpoint rule revision has no supported upgrade path",
        ))
    }
}

const fn rule(id: u16, products: u8) -> Rule {
    Rule {
        id,
        revision: 1,
        from: &[],
        products,
    }
}
const fn revised(id: u16, products: u8) -> Rule {
    Rule { revision: 2, from: &[1], ..rule(id, products) }
}

const CODE: u8 = bit(FILES) | bit(SYMBOLS) | bit(REL);
const RULES: &[Rule] = &[
    rule(1, ALL),                  // worktree membership, observations and eligibility
    rule(2, ALL & !bit(PATHGRAM)), // captured bytes, text/binary classification
    rule(3, bit(PATHGRAM) | bit(CONTENT)), // trigram keys
    rule(4, bit(RANK)),            // tokenization, positions and fields
    rule(5, bit(RANK)),            // ranked chunk boundaries
    revised(6, bit(MD) | bit(RANK)), // Markdown sections and anchors after raw HTML blocks
    Rule {
        revision: 10,
        from: &[1, 2, 3, 4, 5, 6, 7, 8, 9],
        ..rule(7, bit(FILES) | bit(MD) | bit(RANK))
    }, // Markdown link recognition and definition ordering
    // Binding through intermediate Rust glob imports changes derived graph edges.
    Rule { revision: 4, from: &[1, 2, 3], ..rule(8, bit(SYMBOLS) | bit(REL)) },
    rule(9, ALL),                  // file ID namespace and key ordering
    rule(10, bit(FILES) | bit(MD) | bit(RANK)), // source line boundaries
    // The parser evidence is packed into FILES. Each profile can evolve on its
    // own; products name the largest rebuild necessary when no finer evidence
    // proves a consumer unchanged. Rust, JS, Python, Starlark, shell, PowerShell,
    // C#, Swift, C, C++, Protobuf, CSS, HTML and SQL.
    Rule { revision: 15, from: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14], ..rule(32, CODE) }, // Rust tuple and array closure parameters.
    Rule { revision: 14, from: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13], ..rule(33, CODE) }, // JSX expression references.
    Rule { revision: 4, from: &[1, 2, 3], ..rule(34, CODE) },
    rule(35, CODE),
    Rule { revision: 10, from: &[1, 2, 3, 4, 5, 6, 7, 8, 9], ..rule(36, CODE) }, // Shell source operands and here-document quote removal.
    Rule { revision: 6, from: &[1, 2, 3, 4, 5], ..rule(37, CODE) }, // Leading BOM in PowerShell commands.
    Rule { revision: 9, from: &[1, 2, 3, 4, 5, 6, 7, 8], ..rule(38, CODE) }, // C# using and alias scope.
    Rule { revision: 4, from: &[1, 2, 3], ..rule(39, CODE) },
    // Older C-family evidence may predate SDK receipts, C23 branches, or compile option operand boundaries.
    Rule { revision: 23, from: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22], ..rule(40, CODE) },
    Rule { revision: 23, from: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22], ..rule(41, CODE) },
    revised(42, CODE),
    rule(43, CODE),
    revised(44, CODE),
    revised(45, CODE), // SQL dollar-quoted bodies
    Rule { revision: 4, from: &[1, 2, 3], ..rule(46, CODE) }, // Named PHP and Hack static method edges.
    Rule { revision: 14, from: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13], ..rule(47, bit(FILES)) }, // Rust macro arrows and foreign ABI parameter scoring.
    rule(48, CODE), // Go grouped declaration evidence.
    Rule { revision: 2, from: &[1], ..rule(49, CODE) }, // Objective-C declarator and tag namespace evidence.
    Rule { revision: 3, from: &[1, 2], ..rule(50, CODE) }, // Nested Mako expression references.
    rule(51, CODE), // HCL template interpolation references.
];
const _: () = assert!(N == 8 && SOURCE == 2);

fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid checkpoint contract registry",
    )
}
fn incompatible(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(test)]
pub(crate) fn write_contracts() -> Vec<u8> {
    let mut out = Vec::new();
    append_contracts(&mut out);
    out
}

pub(super) fn append_contracts(out: &mut Vec<u8>) {
    out.reserve(28 + RULES.len() * 4);
    out.extend(MAGIC);
    out.extend(WRITER_PROTOCOL.to_le_bytes());
    out.extend(env!("CX_BUILD_ID").as_bytes());
    out.extend((RULES.len() as u16).to_le_bytes());
    for rule in RULES {
        out.extend(rule.id.to_le_bytes());
        out.extend(rule.revision.to_le_bytes());
    }
}

pub(crate) fn check_contracts(bytes: &[u8]) -> io::Result<Migration> {
    plan(bytes, RULES)
}

/// Return the contract rows only after validating the writer envelope and row count.
fn contract_rows(bytes: &[u8]) -> io::Result<&[[u8; 4]]> {
    let (header, rows) = bytes.split_at_checked(28).ok_or_else(malformed)?;
    if &header[..8] != MAGIC
        || !matches!(
            u16::from_le_bytes([header[8], header[9]]),
            1 | 2 | WRITER_PROTOCOL
        )
    {
        return Err(incompatible("unsupported checkpoint writer protocol"));
    }
    if !header[10..26].iter().all(u8::is_ascii_hexdigit) {
        return Err(malformed());
    }
    let count = usize::from(u16::from_le_bytes([header[26], header[27]]));
    if rows.len() != count * 4 {
        return Err(malformed());
    }
    Ok(rows.as_chunks::<4>().0)
}

fn plan(bytes: &[u8], registry: &[Rule]) -> io::Result<Migration> {
    let rows = contract_rows(bytes)?;
    // Protocols 1–3 originally ended at rule 45. The following loop still
    // verifies every historical ID and revision before recognizing this edge.
    let missing = registry
        .get(rows.len()..)
        .ok_or_else(|| incompatible("unsupported checkpoint contract set"))?;
    // Only released registry prefixes ending at 45 through 50 can omit rules.
    let added = [46, 47, 48, 49, 50, 51];
    if !missing.iter().rev().map(|r| r.id).eq(added.into_iter().rev().take(missing.len())) {
        return Err(incompatible("unsupported checkpoint contract set"));
    }
    let mut rebuild = missing.iter().fold(0, |mask, rule| mask | rule.products);
    let mut previous = 0;
    for (raw, rule) in rows.iter().zip(registry) {
        let id = u16::from_le_bytes([raw[0], raw[1]]);
        let rev = u16::from_le_bytes([raw[2], raw[3]]);
        if id <= previous || rev == 0 {
            return Err(malformed());
        }
        rebuild |= rule.upgrade(id, rev)?;
        previous = id;
    }
    Ok(Migration { rebuild })
}

// Produced by running the build.rs from the legacy native writer on its original sources.
// The old fingerprint includes TARGET. A matching hash from another target
// is insufficient evidence for native parser/eligibility behavior.
fn baseline_hash() -> Option<&'static [u8]> {
    Some(match env!("CX_TARGET") {
        "aarch64-apple-darwin" => b"a3bcdea982b89632",
        "x86_64-apple-darwin" => b"71252f029c7e135c",
        "x86_64-unknown-linux-gnu" => b"82fc9b29175d0986",
        "aarch64-unknown-linux-gnu" => b"8d8b9e525f9a4538",
        "x86_64-unknown-linux-musl" => b"463d20d63a2e4f2f",
        "aarch64-unknown-linux-musl" => b"eb0386544940b909",
        _ => return None,
    })
}

pub(crate) fn legacy_migration(fingerprint: &[u8]) -> Migration {
    if baseline_hash() != Some(fingerprint) {
        return Migration { rebuild: ALL };
    }
    // The frozen release produced revision 1 of these exact contracts. A new
    // producer or a changed rule makes its dependent products dirty, even if
    // the historical build fingerprint itself is allowlisted.
    let mut rebuild = 0;
    for rule in RULES {
        if rule.revision != 1 || !matches!(rule.id,1..=10|32..=45) {
            rebuild |= rule.products;
        }
    }
    Migration { rebuild }
}

// Exercise a recognized edge using the registered product dependencies without
// declaring a fictitious historical revision in a shipping binary.
#[cfg(test)]
pub(super) fn fixture_upgrade(id: u16) -> Migration {
    let mut next = RULES.iter().map(|r| Rule { ..*r }).collect::<Vec<_>>();
    let i = RULES.iter().position(|rule| rule.id == id).unwrap();
    let producer = &mut next[i];
    producer.from = std::slice::from_ref(&RULES[i].revision);
    producer.revision += 1;
    plan(&write_contracts(), &next).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_release_roundtrips_and_build_id_is_diagnostic() {
        let mut bytes = write_contracts();
        assert!(check_contracts(&bytes).unwrap().reusable());
        bytes[10..26].copy_from_slice(b"0000000000000000");
        assert!(check_contracts(&bytes).unwrap().reusable());
    }

    #[test]
    fn known_writer_protocols_share_existing_products() {
        for protocol in [1u16, 2, WRITER_PROTOCOL] {
            let mut bytes = write_contracts();
            bytes[8..10].copy_from_slice(&protocol.to_le_bytes());
            assert!(check_contracts(&bytes).unwrap().reusable());
        }
    }

    #[test]
    fn known_rule_upgrade_invalidates_only_its_declared_consumers() {
        let old = write_contracts();
        let mut rules = RULES.iter().map(|r| Rule { ..*r }).collect::<Vec<_>>();
        rules[5].revision = 3;
        rules[5].from = &[2];
        let change = plan(&old, &rules).unwrap();
        for product in 1..=N {
            assert_eq!(
                change.requires_rebuild(product),
                matches!(product, MD | RANK)
            );
        }
        let mut upgraded = old;
        upgraded[28 + 5 * 4 + 2..28 + 5 * 4 + 4].copy_from_slice(&3u16.to_le_bytes());
        assert_eq!(
            check_contracts(&upgraded).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        rules[5].from = &[];
        upgraded[28 + 5 * 4 + 2..28 + 5 * 4 + 4].copy_from_slice(&2u16.to_le_bytes());
        assert!(plan(&upgraded, &rules).is_err());
    }

    #[test]
    fn snapshots_before_the_new_parsers_require_new_evidence_and_bindings() {
        let mut old = write_contracts();
        let len = old.len();
        old.truncate(len - 8);
        old[26..28].copy_from_slice(&((RULES.len() - 2) as u16).to_le_bytes());
        assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
        old[28] = 0;
        assert!(check_contracts(&old).is_err());
    }

    #[test]
    fn old_go_and_compile_command_contracts_rebuild_code_only() {
        let stored = write_contracts();
        for missing in 1..=6 {
            let count = RULES.len() - missing;
            let mut old = stored[..28 + 4 * count].to_vec();
            old[26..28].copy_from_slice(&(count as u16).to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
        }
        for id in [40u16, 41] {
            let mut old = stored.clone();
            for revision in [5u16, 6, 7, 8, 9, 10, 11, 12] {
                let row = old[28..].chunks_exact_mut(4).find(|r| r[..2] == id.to_le_bytes()).unwrap();
                row[2..].copy_from_slice(&revision.to_le_bytes());
                assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
            }
        }
        assert!(check_contracts(&stored).unwrap().reusable());
    }

    #[test]
    fn old_jsx_and_c_expression_evidence_rebuilds_code_products() {
        for (id, previous) in [(33u16, 5u16), (37, 4), (40, 16), (41, 16), (32, 7), (33, 6), (40, 17), (41, 17), (50, 1), (32, 8), (36, 2), (37, 5), (40, 18), (41, 18), (33, 7), (32, 9), (36, 3), (40, 19), (41, 19), (38, 3), (33, 8), (34, 2), (36, 4), (49, 1), (40, 20), (41, 20), (32, 10), (33, 9), (34, 3), (36, 5), (38, 4), (32, 11), (33, 10), (36, 6), (50, 2), (38, 5), (40, 21), (41, 21), (32, 12), (33, 11), (36, 7), (38, 6), (32, 13), (33, 12), (36, 8), (38, 7), (40, 22), (41, 22), (32, 14), (33, 13), (36, 9), (38, 8)] {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4).find(|r| r[..2] == id.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&previous.to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
        }
    }

    #[test]
    fn previous_conformance_rules_invalidate_cached_parser_facts_and_edges() {
        let mut graph=write_contracts();
        let old_rule=graph[28..].chunks_exact_mut(4).find(|row|row[..2]==8u16.to_le_bytes()).unwrap();
        old_rule[2..].copy_from_slice(&3u16.to_le_bytes());
        assert_eq!(check_contracts(&graph).unwrap().rebuild,bit(SYMBOLS)|bit(REL));
        for id in [8u16, 32, 33, 34, 36, 37, 38, 39, 40, 41, 42, 44, 46, 47] {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4)
                .find(|row| row[..2] == id.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&1u16.to_le_bytes());
            let expected = match id { 8 => bit(SYMBOLS) | bit(REL), 47 => bit(FILES), _ => CODE };
            assert_eq!(check_contracts(&old).unwrap().rebuild, expected, "rule {id}");
        }
    }

    #[test]
    fn previous_script_and_rust_type_evidence_rebuild_code_products() {
        for (id, revision) in [(32u16, 3u16), (32, 4), (32, 5), (32, 6), (33, 3)] {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4).find(|row| row[..2] == id.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&revision.to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
        }
    }

    #[test]
    fn computed_bindings_and_direct_include_operands_refresh_code() {
        for (id, previous) in [(33u16, 4u16), (40, 13), (41, 13)] {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4).find(|row| row[..2] == id.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&previous.to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
        }
    }

    #[test]
    fn revised_swift_and_c_family_inputs_refresh_code() {
        for (id, previous) in [(39u16, 3u16), (40, 14), (41, 14), (40,15), (41,15)] {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4).find(|row| row[..2] == id.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&previous.to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
        }
    }

    #[test]
    fn prior_link_rules_rebuild_markdown_headers_and_ranked_boundaries() {
        for revision in 1u16..10 {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4).find(|row| row[..2] == 7u16.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&revision.to_le_bytes());
            assert_eq!(
                check_contracts(&old).unwrap().rebuild,
                bit(FILES) | bit(MD) | bit(RANK)
            );
        }
    }

    #[test]
    fn prior_score_revision_invalidates_file_measurements() {
        for revision in 1u16..12 {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4).find(|row| row[..2] == 47u16.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&revision.to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, bit(FILES));
        }
    }

    #[test]
    fn intermediate_repair_checkpoints_rebuild_changed_profile_evidence() {
        for id in [33u16, 37, 38, 39, 40, 41, 46] {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4)
                .find(|row| row[..2] == id.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&2u16.to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, CODE, "rule {id}");
        }
    }

    #[test]
    fn c_checkpoints_without_sdk_receipts_require_reanalysis() {
        for id in [40u16, 41] {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4)
                .find(|row| row[..2] == id.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&3u16.to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
        }
    }

    #[test]
    fn old_sql_evidence_cannot_retain_symbols_from_dollar_quoted_bodies() {
        let mut old = write_contracts();
        let row = old[28..].chunks_exact_mut(4).find(|row| row[..2] == 45u16.to_le_bytes()).unwrap();
        row[2..].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(check_contracts(&old).unwrap().rebuild, CODE);
    }

    #[test]
    fn unsupported_writer_and_malformed_bodies_are_distinct() {
        let mut bytes = write_contracts();
        bytes[8] = WRITER_PROTOCOL as u8 + 1;
        assert_eq!(
            check_contracts(&bytes).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        bytes[8] = 1;
        bytes.pop();
        assert_eq!(
            check_contracts(&bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn historical_import_has_a_target_specific_allowlist() {
        if let Some(hash) = baseline_hash() {
            assert_eq!(legacy_migration(hash).rebuild, CODE | bit(MD) | bit(RANK));
        }
        for unknown in [b"".as_slice(), b"0123456789abcdef"] {
            let plan = legacy_migration(unknown);
            assert!((1..=N).all(|p| plan.requires_rebuild(p)));
        }
    }
}
