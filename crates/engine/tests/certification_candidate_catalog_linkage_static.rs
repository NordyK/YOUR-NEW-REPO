use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const MANIFEST: &str = "../../contracts/test/fixtures/certification_candidates.local.json";
const CATALOG: &str = "../../../anvil/anvil_catalog.csv";

#[derive(Debug)]
struct CatalogRow {
    hops: u64,
    families: Vec<String>,
    tokens: Vec<String>,
    occurrences: u64,
    sources: Vec<String>,
    labels: Vec<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from("../../..")
}

fn read_required(path: impl AsRef<Path>, label: &str) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {label} at {}: {err}", path.display()))
}

fn split_plus(raw: &str) -> Vec<String> {
    raw.split('+')
        .filter(|part| !part.trim().is_empty())
        .map(str::to_string)
        .collect()
}

fn required_str<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("{field} must be a string"))
}

fn required_u64(value: &Value, field: &str) -> u64 {
    value[field]
        .as_u64()
        .unwrap_or_else(|| panic!("{field} must be a u64"))
}

fn required_string_array(value: &Value, field: &str) -> Vec<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be an array"))
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .unwrap_or_else(|| panic!("{field} entries must be strings"))
                .to_string()
        })
        .collect()
}

fn is_tx_hash(raw: &str) -> bool {
    raw.len() == 66 && raw.starts_with("0x") && raw[2..].chars().all(|ch| ch.is_ascii_hexdigit())
}

fn catalog_key(block: u64, tx_hash: &str) -> String {
    format!("{block}:{}", tx_hash.to_ascii_lowercase())
}

fn parse_catalog() -> BTreeMap<String, CatalogRow> {
    let mut rows = BTreeMap::new();
    for (idx, raw_line) in read_required(CATALOG, "anvil catalog").lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        if idx == 0 || line.trim().is_empty() {
            continue;
        }
        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            8,
            "catalog line {line_no} must have 8 comma-separated fields"
        );

        let block = fields[0]
            .parse::<u64>()
            .unwrap_or_else(|err| panic!("catalog line {line_no} invalid block: {err}"));
        let tx_hash = fields[1].to_ascii_lowercase();
        assert!(
            is_tx_hash(&tx_hash),
            "catalog line {line_no} invalid tx hash {tx_hash}"
        );
        let row = CatalogRow {
            hops: fields[2]
                .parse::<u64>()
                .unwrap_or_else(|err| panic!("catalog line {line_no} invalid hops: {err}")),
            families: split_plus(fields[3]),
            tokens: split_plus(fields[4]),
            occurrences: fields[5]
                .parse::<u64>()
                .unwrap_or_else(|err| panic!("catalog line {line_no} invalid occurrences: {err}")),
            sources: split_plus(fields[6]),
            labels: split_plus(fields[7]),
        };
        let old = rows.insert(catalog_key(block, &tx_hash), row);
        assert!(old.is_none(), "duplicate catalog key at line {line_no}");
    }
    rows
}

fn block_cache_path(block: u64) -> PathBuf {
    let direct = repo_root()
        .join("anvil/block_cache")
        .join(block.to_string());
    if direct.is_file() {
        return direct;
    }
    direct.join("storage.json")
}

fn artifact_path(raw: &str) -> PathBuf {
    repo_root().join(raw)
}

#[test]
fn certification_candidates_resolve_to_catalog_rows_and_parent_state() {
    if !Path::new(CATALOG).exists() {
        eprintln!("SKIP: artifact {CATALOG} absent (out-of-branch)");
        return;
    }
    let manifest: Value = serde_json::from_str(&read_required(MANIFEST, "candidate manifest"))
        .expect("candidate manifest must parse as JSON");
    assert_eq!(
        required_str(&manifest, "source"),
        "anvil/anvil_catalog.csv",
        "candidate manifest source must identify the catalog"
    );

    let catalog = parse_catalog();
    assert!(
        catalog.len() >= 1_200,
        "catalog is too small for manifest linkage checks: {}",
        catalog.len()
    );

    let candidates = manifest["candidates"]
        .as_array()
        .expect("manifest candidates must be an array");
    assert!(
        candidates.len() >= 38,
        "candidate manifest lost curated coverage: {}",
        candidates.len()
    );

    let mut linked_candidates = 0usize;
    let mut parent_cache_hits = 0usize;
    let mut seen_keys = BTreeSet::new();
    let mut hop_counts = BTreeMap::<u64, usize>::new();
    let mut family_set = BTreeSet::new();
    let mut token_set = BTreeSet::new();
    let mut discovery_only = 0usize;
    let mut positive_realized = 0usize;
    let mut positive_artifacts = BTreeSet::new();

    for candidate in candidates {
        let block = required_u64(candidate, "block_number");
        let tx_hash = required_str(candidate, "tx_hash").to_ascii_lowercase();
        assert!(
            is_tx_hash(&tx_hash),
            "candidate has invalid tx hash {tx_hash}"
        );
        assert_eq!(
            required_str(candidate, "source"),
            "anvil_catalog",
            "candidate {tx_hash} must resolve through anvil_catalog"
        );

        let key = catalog_key(block, &tx_hash);
        assert!(
            seen_keys.insert(key.clone()),
            "duplicate candidate catalog key {key}"
        );
        let catalog_row = catalog
            .get(&key)
            .unwrap_or_else(|| panic!("candidate {key} missing from current anvil catalog"));
        linked_candidates += 1;

        let hop_count = required_u64(candidate, "hop_count");
        assert_eq!(
            catalog_row.hops, hop_count,
            "candidate {key} hop_count drifted from catalog"
        );
        *hop_counts.entry(hop_count).or_insert(0) += 1;

        let families = required_string_array(candidate, "families");
        assert_eq!(
            catalog_row.families, families,
            "candidate {key} family path drifted from catalog"
        );
        for family in &families {
            family_set.insert(family.clone());
            assert!(
                catalog_row.labels.contains(family),
                "candidate {key} family {family} missing from catalog labels {:?}",
                catalog_row.labels
            );
        }

        let tokens = required_string_array(candidate, "token_path");
        assert_eq!(
            catalog_row.tokens, tokens,
            "candidate {key} token path drifted from catalog"
        );
        for token in &tokens {
            token_set.insert(token.clone());
        }

        assert!(
            catalog_row.occurrences > 0,
            "candidate {key} catalog occurrences must be nonzero"
        );
        assert!(
            catalog_row.sources.iter().any(|source| source == "deep"),
            "candidate {key} is no longer sourced from deep catalog rows"
        );

        let parent_cache = block_cache_path(block - 1);
        assert!(
            parent_cache.is_file(),
            "candidate {key} missing parent cache at {}",
            parent_cache.display()
        );
        assert!(
            fs::metadata(&parent_cache)
                .unwrap_or_else(|err| panic!("metadata {}: {err}", parent_cache.display()))
                .len()
                > 0,
            "candidate {key} parent cache is empty"
        );
        parent_cache_hits += 1;

        match (
            required_str(candidate, "route_reconstruction_status"),
            required_str(candidate, "profit_certification_status"),
        ) {
            ("discovered", "untested") => {
                discovery_only += 1;
                assert!(
                    candidate.get("certification_artifact").is_none(),
                    "discovery-only candidate {key} must not cite a profit artifact"
                );
                assert!(
                    candidate.get("realized_amount_raw").is_none(),
                    "discovery-only candidate {key} must not claim realized profit"
                );
            }
            ("reconstructed", "positive_realized") => {
                positive_realized += 1;
                let artifact = required_str(candidate, "certification_artifact");
                let path = artifact_path(artifact);
                assert!(
                    path.is_file(),
                    "positive candidate {key} artifact missing at {}",
                    path.display()
                );
                positive_artifacts.insert(artifact.to_string());
                let realized = required_str(candidate, "realized_amount_raw")
                    .parse::<u128>()
                    .unwrap_or_else(|err| {
                        panic!("positive candidate {key} invalid realized_amount_raw: {err}")
                    });
                assert!(
                    realized > 0,
                    "positive candidate {key} must have positive realized_amount_raw"
                );
            }
            (reconstruction, profit) => {
                panic!("candidate {key} unsupported status pair {reconstruction}/{profit}");
            }
        }
    }

    assert!(
        hop_counts.get(&2).copied().unwrap_or_default() >= 10,
        "candidate manifest lacks 2-hop catalog coverage: {hop_counts:?}"
    );
    assert!(
        hop_counts.get(&3).copied().unwrap_or_default() >= 10,
        "candidate manifest lacks 3-hop catalog coverage: {hop_counts:?}"
    );
    assert!(
        hop_counts.get(&4).copied().unwrap_or_default() >= 10,
        "candidate manifest lacks 4-hop catalog coverage: {hop_counts:?}"
    );
    assert!(
        family_set.len() >= 13,
        "candidate manifest lost family diversity: {family_set:?}"
    );
    assert!(
        token_set.len() >= 6,
        "candidate manifest lost token diversity: {token_set:?}"
    );
    assert_eq!(
        discovery_only + positive_realized,
        candidates.len(),
        "every candidate must be discovery-only or positive-realized"
    );
    assert_eq!(
        positive_realized, 1,
        "manifest should keep exact current positive replay count visible"
    );
    assert!(
        positive_artifacts
            .contains("engine/contracts/test/MantisAnvilCatalogPositiveReplay.Fork.t.sol"),
        "positive replay artifact link disappeared: {positive_artifacts:?}"
    );
    assert_eq!(
        linked_candidates,
        candidates.len(),
        "not every manifest candidate resolves to the current catalog; linked_candidates={linked_candidates}, parent_cache_hits={parent_cache_hits}, candidates={}, families={}, tokens={}, positives={positive_realized}",
        candidates.len(),
        family_set.len(),
        token_set.len()
    );
    assert_eq!(
        parent_cache_hits,
        candidates.len(),
        "not every manifest candidate has parent cached state; parent_cache_hits={parent_cache_hits}, candidates={}",
        candidates.len()
    );
}
