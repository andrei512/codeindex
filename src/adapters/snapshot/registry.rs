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

// Revision 2 checkpoints predate final callable, module-export and
// translation-unit dependency evidence. Either earlier writer needs reanalysis.
const fn repaired_profile(id: u16) -> Rule {
    Rule { revision: 3, from: &[1, 2], ..rule(id, CODE) }
}

const CODE: u8 = bit(FILES) | bit(SYMBOLS) | bit(REL);
const RULES: &[Rule] = &[
    rule(1, ALL),                  // worktree membership, observations and eligibility
    rule(2, ALL & !bit(PATHGRAM)), // captured bytes, text/binary classification
    rule(3, bit(PATHGRAM) | bit(CONTENT)), // trigram keys
    rule(4, bit(RANK)),            // tokenization, positions and fields
    rule(5, bit(RANK)),            // ranked chunk boundaries
    rule(6, bit(MD) | bit(RANK)),  // Markdown sections and heading anchors
    rule(7, bit(MD)),              // relative link extraction and qualification
    revised(8, bit(SYMBOLS) | bit(REL)), // project and binding rules
    rule(9, ALL),                  // file ID namespace and key ordering
    rule(10, bit(FILES) | bit(MD) | bit(RANK)), // source line boundaries
    // The parser evidence is packed into FILES. Each profile can evolve on its
    // own; products name the largest rebuild necessary when no finer evidence
    // proves a consumer unchanged. Rust, JS, Python, Starlark, shell, PowerShell,
    // C#, Swift, C, C++, Protobuf, CSS, HTML and SQL.
    rule(32, CODE),
    revised(33, CODE),
    revised(34, CODE),
    rule(35, CODE),
    rule(36, CODE),
    Rule { revision: 4, from: &[1, 2, 3], ..rule(37, CODE) }, // Leading BOM in PowerShell commands.
    repaired_profile(38),
    repaired_profile(39),
    // Earlier C evidence lacks external-header receipts. Rebuild before decoding.
    Rule { revision: 4, from: &[1, 2, 3], ..rule(40, CODE) },
    Rule { revision: 4, from: &[1, 2, 3], ..rule(41, CODE) },
    rule(42, CODE),
    rule(43, CODE),
    revised(44, CODE),
    revised(45, CODE), // SQL dollar-quoted bodies
    repaired_profile(46), // Additional native language profiles and their evidence.
    Rule { revision: 6, from: &[1, 2, 3, 4, 5], ..rule(47, bit(FILES)) }, // File-local score semantics.
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

pub(crate) fn write_contracts() -> Vec<u8> {
    let mut out = Vec::with_capacity(28 + RULES.len() * 4);
    out.extend(MAGIC);
    out.extend(WRITER_PROTOCOL.to_le_bytes());
    out.extend(env!("CX_BUILD_ID").as_bytes());
    out.extend((RULES.len() as u16).to_le_bytes());
    for rule in RULES {
        out.extend(rule.id.to_le_bytes());
        out.extend(rule.revision.to_le_bytes());
    }
    out
}

pub(crate) fn check_contracts(bytes: &[u8]) -> io::Result<Migration> {
    plan(bytes, RULES)
}

fn plan(bytes: &[u8], registry: &[Rule]) -> io::Result<Migration> {
    let header = bytes.get(..28).ok_or_else(malformed)?;
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
    if bytes.len() - 28 != count * 4 {
        return Err(malformed());
    }
    // Protocols 1–3 originally ended at rule 45. The following loop still
    // verifies every historical ID and revision before recognizing this edge.
    let missing = registry
        .get(count..)
        .ok_or_else(|| incompatible("unsupported checkpoint contract set"))?;
    if !matches!(
        missing,
        [] | [Rule { id: 47, .. }] | [Rule { id: 46, .. }, Rule { id: 47, .. }]
    ) {
        return Err(incompatible("unsupported checkpoint contract set"));
    }
    let mut rebuild = missing.iter().fold(0, |mask, rule| mask | rule.products);
    let mut previous = 0;
    for (raw, rule) in bytes[28..].chunks_exact(4).zip(registry) {
        let id = u16::from_le_bytes([raw[0], raw[1]]);
        let rev = u16::from_le_bytes([raw[2], raw[3]]);
        if id <= previous || rev == 0 {
            return Err(malformed());
        }
        if id != rule.id {
            return Err(incompatible("unknown required checkpoint contract"));
        }
        previous = id;
        if rev != rule.revision {
            if !rule.from.contains(&rev) {
                return Err(incompatible(
                    "checkpoint rule revision has no supported upgrade path",
                ));
            }
            rebuild |= rule.products;
        }
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
    let producer = next.iter_mut().find(|r| r.id == id).unwrap();
    producer.from = match producer.revision {
        1 => &[1],
        2 => &[2],
        _ => panic!("undeclared test revision"),
    };
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
        rules[5].revision = 2;
        rules[5].from = &[1];
        let change = plan(&old, &rules).unwrap();
        for product in 1..=N {
            assert_eq!(
                change.requires_rebuild(product),
                matches!(product, MD | RANK)
            );
        }
        let mut upgraded = old;
        upgraded[28 + 5 * 4 + 2..28 + 5 * 4 + 4].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            check_contracts(&upgraded).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        rules[5].from = &[];
        upgraded[28 + 5 * 4 + 2..28 + 5 * 4 + 4].copy_from_slice(&1u16.to_le_bytes());
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
    fn previous_conformance_rules_invalidate_cached_parser_facts_and_edges() {
        for id in [8u16, 33, 34, 37, 38, 39, 40, 41, 44, 46, 47] {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4)
                .find(|row| row[..2] == id.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&1u16.to_le_bytes());
            let expected = match id { 8 => bit(SYMBOLS) | bit(REL), 47 => bit(FILES), _ => CODE };
            assert_eq!(check_contracts(&old).unwrap().rebuild, expected, "rule {id}");
        }
    }

    #[test]
    fn prior_score_revision_invalidates_file_measurements() {
        for revision in 1u16..5 {
            let mut old = write_contracts();
            let row = old[28..].chunks_exact_mut(4).find(|row| row[..2] == 47u16.to_le_bytes()).unwrap();
            row[2..].copy_from_slice(&revision.to_le_bytes());
            assert_eq!(check_contracts(&old).unwrap().rebuild, bit(FILES));
        }
    }

    #[test]
    fn intermediate_repair_checkpoints_rebuild_changed_profile_evidence() {
        for id in [37u16, 38, 39, 40, 41, 46] {
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
            assert_eq!(legacy_migration(hash).rebuild, CODE);
        }
        for unknown in [b"".as_slice(), b"0123456789abcdef"] {
            let plan = legacy_migration(unknown);
            assert!((1..=N).all(|p| plan.requires_rebuild(p)));
        }
    }
}
