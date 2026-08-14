use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const MANIFEST: &str = "../../contracts/test/fixtures/certification_candidates.local.json";

fn read_manifest() -> Value {
    let raw = fs::read_to_string(Path::new(MANIFEST))
        .unwrap_or_else(|err| panic!("failed to read certification candidate manifest: {err}"));
    serde_json::from_str(&raw).expect("certification candidate manifest must be valid json")
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

fn string_array(value: &Value, field: &str) -> Vec<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be an array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{field} entries must be strings"))
                .to_string()
        })
        .collect()
}

fn assert_hex_tx_hash(tx_hash: &str) {
    assert!(
        tx_hash.len() == 66
            && tx_hash.starts_with("0x")
            && tx_hash[2..].chars().all(|ch| ch.is_ascii_hexdigit()),
        "invalid tx hash {tx_hash}"
    );
}

#[test]
fn local_certification_candidate_manifest_is_structured_for_replay_promotion() {
    let manifest = read_manifest();
    assert_eq!(
        required_str(&manifest, "source"),
        "anvil/anvil_catalog.csv",
        "manifest must identify the local catalog source"
    );
    assert_eq!(
        required_str(&manifest, "candidate_status"),
        "mixed_discovery_and_exact_prefix_replay",
        "manifest must distinguish discovery rows from exact-prefix proof rows"
    );
    assert_eq!(
        required_str(&manifest, "same_venue_status"),
        "open_catalog_has_no_single_family_hop_rows",
        "same-venue proof must stay open until segment-level discovery finds a candidate"
    );

    let minimums = manifest["minimum_required_candidates"]
        .as_object()
        .expect("minimum_required_candidates must be an object");
    let required_minimums = BTreeMap::from([
        ("two_hop", 10_u64),
        ("three_hop", 10),
        ("four_hop", 10),
        ("cross_venue", 10),
        ("same_venue_from_catalog", 0),
    ]);
    for (field, expected) in required_minimums {
        assert_eq!(
            minimums
                .get(field)
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("minimum_required_candidates.{field} must be a u64")),
            expected,
            "unexpected minimum for {field}"
        );
    }

    let allowed_reconstruction_statuses = BTreeSet::from([
        "discovered",
        "reconstructed",
        "rejected_missing_pool_data",
        "rejected_non_cycle",
        "rejected_unsupported_family",
    ]);
    let allowed_profit_statuses = BTreeSet::from([
        "untested",
        "positive_realized",
        "loss_revert",
        "mechanical_revert",
        "raw_tx_positive_only",
    ]);

    let candidates = manifest["candidates"]
        .as_array()
        .expect("candidates must be an array");
    assert!(
        candidates.len() >= 30,
        "manifest needs at least 30 local candidates, got {}",
        candidates.len()
    );

    let mut hop_counts = BTreeMap::<u64, usize>::new();
    let mut source_counts = BTreeMap::<String, usize>::new();
    let mut all_families = BTreeSet::<String>::new();
    let mut cross_venue_count = 0usize;
    let mut same_venue_count = 0usize;
    let mut untested_discovery_count = 0usize;
    let mut positive_realized_count = 0usize;
    let mut positive_realized_3hop_count = 0usize;
    let mut seen_txs = BTreeSet::<String>::new();

    for candidate in candidates {
        let block_number = required_u64(candidate, "block_number");
        assert!(block_number > 0, "block_number must be nonzero");

        let tx_hash = required_str(candidate, "tx_hash").to_ascii_lowercase();
        assert_hex_tx_hash(&tx_hash);
        assert!(
            seen_txs.insert(tx_hash.clone()),
            "duplicate candidate tx hash {tx_hash}"
        );

        let hop_count = required_u64(candidate, "hop_count");
        assert!(
            (2..=4).contains(&hop_count),
            "candidate hop_count must be 2, 3, or 4, got {hop_count}"
        );
        *hop_counts.entry(hop_count).or_insert(0) += 1;

        let families = string_array(candidate, "families");
        assert!(
            !families.is_empty(),
            "candidate {tx_hash} must include at least one family"
        );
        if families.len() == 1 {
            same_venue_count += 1;
        } else {
            cross_venue_count += 1;
        }
        for family in families {
            assert!(!family.trim().is_empty(), "family name cannot be empty");
            all_families.insert(family);
        }

        let token_path = string_array(candidate, "token_path");
        assert!(
            !token_path.is_empty(),
            "candidate {tx_hash} must include token_path"
        );

        let source = required_str(candidate, "source");
        *source_counts.entry(source.to_string()).or_insert(0) += 1;
        assert_eq!(source, "anvil_catalog", "unexpected source {source}");

        let reconstruction_status = required_str(candidate, "route_reconstruction_status");
        assert!(
            allowed_reconstruction_statuses.contains(reconstruction_status),
            "unsupported route_reconstruction_status {reconstruction_status}"
        );

        let profit_status = required_str(candidate, "profit_certification_status");
        assert!(
            allowed_profit_statuses.contains(profit_status),
            "unsupported profit_certification_status {profit_status}"
        );
        if reconstruction_status == "discovered" && profit_status == "untested" {
            untested_discovery_count += 1;
        } else if reconstruction_status == "reconstructed" && profit_status == "positive_realized" {
            positive_realized_count += 1;
            if hop_count == 3 {
                positive_realized_3hop_count += 1;
            }
            assert!(
                candidate["certification_artifact"].as_str().is_some(),
                "positive candidate {tx_hash} must cite certification_artifact"
            );
            assert!(
                candidate["realized_amount_raw"]
                    .as_str()
                    .and_then(|amount| amount.parse::<u128>().ok())
                    .unwrap_or_default()
                    > 0,
                "positive candidate {tx_hash} must include positive realized_amount_raw"
            );
        } else {
            panic!(
                "candidate {tx_hash} has unsupported status combination {reconstruction_status}/{profit_status}"
            );
        }
    }

    assert!(
        hop_counts.get(&2).copied().unwrap_or_default() >= 10,
        "manifest lacks 2-hop seeds: {hop_counts:?}"
    );
    assert!(
        hop_counts.get(&3).copied().unwrap_or_default() >= 10,
        "manifest lacks 3-hop seeds: {hop_counts:?}"
    );
    assert!(
        hop_counts.get(&4).copied().unwrap_or_default() >= 10,
        "manifest lacks 4-hop seeds: {hop_counts:?}"
    );
    assert!(
        cross_venue_count >= 10,
        "manifest lacks cross-venue seeds: {cross_venue_count}"
    );
    assert_eq!(
        same_venue_count, 0,
        "same-venue candidates require segment-level proof before entering this catalog fixture"
    );
    assert_eq!(
        source_counts
            .get("anvil_catalog")
            .copied()
            .unwrap_or_default(),
        candidates.len(),
        "all candidates in this fixture must come from the local anvil catalog"
    );
    assert_eq!(
        untested_discovery_count + positive_realized_count,
        candidates.len(),
        "all candidates must be either discovery-only or exact-prefix positive proof"
    );
    assert!(
        positive_realized_3hop_count >= 1,
        "manifest must include the proven positive 3-hop exact-prefix candidate"
    );

    for required_family in [
        "uniswap",
        "pancakeswap",
        "camelot",
        "sushiswap",
        "fluid",
        "dodo",
        "balancer",
        "trader_joe",
        "maverick",
        "ramses",
        "solidly",
        "kyberswap",
        "gammaswap",
    ] {
        assert!(
            all_families.contains(required_family),
            "manifest lacks local seed for family {required_family}; families={all_families:?}"
        );
    }
}
