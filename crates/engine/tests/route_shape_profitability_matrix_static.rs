use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const MATRIX: &str = "../../../Codex/ROUTE_SHAPE_PROFITABILITY_MATRIX_2026-06-15.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    rows: u64,
    positive: u64,
    loss: u64,
    other_revert: u64,
    positive_net: i128,
    loss_net: i128,
    all_net: i128,
}

impl Counts {
    fn inc(&mut self, outcome: &str, realized_net: &str) {
        self.rows += 1;
        match outcome {
            "positive" => {
                self.positive += 1;
                let net = parse_i128(realized_net, "positive realized_net_wei");
                self.positive_net += net;
                self.all_net += net;
            }
            "loss" => {
                self.loss += 1;
                let net = parse_i128(realized_net, "loss realized_net_wei");
                self.loss_net += net;
                self.all_net += net;
            }
            "other_revert" => {
                self.other_revert += 1;
                assert!(
                    realized_net.is_empty(),
                    "other_revert rows must not carry realized net"
                );
            }
            other => panic!("unknown strict detector outcome {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BlockerCounts {
    foundry_positive: u64,
    state_point_sensitive_loss: u64,
    strict_metadata_missing: u64,
}

impl BlockerCounts {
    fn inc(&mut self, blocker_class: &str) {
        match blocker_class {
            "foundry_positive" => self.foundry_positive += 1,
            "state_point_sensitive_loss" => self.state_point_sensitive_loss += 1,
            "strict_metadata_missing" => self.strict_metadata_missing += 1,
            other => panic!("unknown blocker class {other}"),
        }
    }
}

fn workspace_path(path: &str) -> PathBuf {
    Path::new("../../..").join(path)
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn read_json(path: impl AsRef<Path>) -> Value {
    let raw = read(path);
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("invalid JSON: {err}"))
}

fn obj<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing object key {key} in {value}"))
}

fn str_field(value: &Value, key: &str) -> String {
    obj(value, key)
        .as_str()
        .unwrap_or_else(|| panic!("missing string field {key} in {value}"))
        .to_string()
}

fn u64_field(value: &Value, key: &str) -> u64 {
    obj(value, key)
        .as_u64()
        .unwrap_or_else(|| panic!("missing u64 field {key} in {value}"))
}

fn i128_string_field(value: &Value, key: &str) -> i128 {
    str_field(value, key)
        .parse::<i128>()
        .unwrap_or_else(|err| panic!("invalid i128 string field {key}: {err}"))
}

fn parse_i128(raw: &str, label: &str) -> i128 {
    raw.parse::<i128>()
        .unwrap_or_else(|err| panic!("invalid {label} {raw}: {err}"))
}

fn csv_rows(path: &str, expected_header: &str, expected_cols: usize) -> Vec<Vec<String>> {
    read(workspace_path(path))
        .lines()
        .enumerate()
        .filter_map(|(idx, raw_line)| {
            let line = raw_line.trim_end_matches('\r');
            if line.trim().is_empty() {
                return None;
            }
            if idx == 0 {
                assert_eq!(line, expected_header, "unexpected CSV header for {path}");
                return None;
            }
            if line.starts_with('#') {
                return None;
            }
            let fields = line.split(',').map(str::to_string).collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                expected_cols,
                "line {} in {path} has wrong CSV field count",
                idx + 1
            );
            Some(fields)
        })
        .collect()
}

fn split_plus(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    raw.split('+')
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn venue_shape(exchange_ids: &str) -> &'static str {
    let unique = split_plus(exchange_ids)
        .into_iter()
        .collect::<BTreeSet<_>>();
    if unique.len() <= 1 {
        "same_exchange"
    } else {
        "cross_exchange"
    }
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|err| panic!("failed to run sha256sum for {}: {err}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("sha256sum stdout must be utf8")
        .split_whitespace()
        .next()
        .expect("sha256sum output missing hash")
        .to_string()
}

fn counts_from_json(value: &Value) -> Counts {
    Counts {
        rows: u64_field(value, "rows"),
        positive: u64_field(value, "positive"),
        loss: u64_field(value, "loss"),
        other_revert: u64_field(value, "other_revert"),
        positive_net: value
            .get("positive_net_wei")
            .map(|_| i128_string_field(value, "positive_net_wei"))
            .unwrap_or_default(),
        loss_net: value
            .get("loss_net_wei")
            .map(|_| i128_string_field(value, "loss_net_wei"))
            .unwrap_or_default(),
        all_net: i128_string_field(value, "all_route_net_wei"),
    }
}

fn blocker_counts_from_json(value: &Value) -> BlockerCounts {
    BlockerCounts {
        foundry_positive: u64_field(value, "foundry_positive"),
        state_point_sensitive_loss: u64_field(value, "state_point_sensitive_loss"),
        strict_metadata_missing: u64_field(value, "strict_metadata_missing"),
    }
}

fn strict_detector_counts(
    path: &str,
) -> (Counts, BTreeMap<String, Counts>, BTreeMap<String, Counts>) {
    let rows = csv_rows(
        path,
        "route_id,block_number,hop_count,exchange_ids,start_token,end_token,input_amount,gross_profit,outcome,realized_net_wei,reason,token_path,pool_path",
        13,
    );
    let mut total = Counts::default();
    let mut by_hop = BTreeMap::<String, Counts>::new();
    let mut by_shape = BTreeMap::<String, Counts>::new();
    for row in rows {
        total.inc(&row[8], &row[9]);
        by_hop
            .entry(row[2].clone())
            .or_default()
            .inc(&row[8], &row[9]);
        by_shape
            .entry(venue_shape(&row[3]).to_string())
            .or_default()
            .inc(&row[8], &row[9]);
    }
    (total, by_hop, by_shape)
}

fn strict_blocker_counts(
    path: &str,
) -> (
    BTreeMap<String, BlockerCounts>,
    BTreeMap<String, BlockerCounts>,
) {
    let rows = csv_rows(
        path,
        "route_id,detector_block_tag,hop_count,exchange_ids,family_path,start_is_weth,input_amount,gross_profit,foundry_outcome,foundry_realized_net_wei,first_revm_seq,first_revm_result,first_revm_balance_delta_wei,last_revm_seq,last_revm_result,last_revm_balance_delta_wei,last_revm_reject_reason,blocker_class",
        18,
    );
    let mut by_hop = BTreeMap::<String, BlockerCounts>::new();
    let mut by_shape = BTreeMap::<String, BlockerCounts>::new();
    for row in rows {
        by_hop.entry(row[2].clone()).or_default().inc(&row[17]);
        by_shape
            .entry(venue_shape(&row[3]).to_string())
            .or_default()
            .inc(&row[17]);
    }
    (by_hop, by_shape)
}

#[derive(Debug, Default)]
struct CatalogCounts {
    hop_counts: BTreeMap<String, u64>,
    two_to_four_rows: u64,
    cross_family_rows: u64,
    same_family_rows: u64,
}

fn catalog_counts(path: &str) -> CatalogCounts {
    let rows = csv_rows(
        path,
        "block,tx,hops,fams,tokens,occurrences,sources,labels",
        8,
    );
    let mut out = CatalogCounts::default();
    for row in rows {
        let hop_count = row[2].clone();
        *out.hop_counts.entry(hop_count.clone()).or_insert(0) += 1;
        let hop_count = hop_count
            .parse::<u64>()
            .unwrap_or_else(|err| panic!("invalid catalog hop count: {err}"));
        if !(2..=4).contains(&hop_count) {
            continue;
        }
        out.two_to_four_rows += 1;
        let unique_families = split_plus(&row[3]).into_iter().collect::<BTreeSet<_>>();
        match unique_families.len() {
            0 => {}
            1 => out.same_family_rows += 1,
            _ => out.cross_family_rows += 1,
        }
    }
    out
}

#[derive(Debug, Default)]
struct TaxonomyCounts {
    rows: u64,
    cross_dex_rows: u64,
    cross_dex_replay_rows: u64,
    cross_dex_placeholder_rows: u64,
    same_dex_rows: u64,
    same_dex_replay_rows: u64,
    same_dex_placeholder_rows: u64,
    subtype_counts: BTreeMap<String, u64>,
}

fn taxonomy_counts(path: &str) -> TaxonomyCounts {
    let rows = csv_rows(
        path,
        "block,tx,arb_type,subtype,dodo_position,families,cycle_token,max_usd,note",
        9,
    );
    let mut out = TaxonomyCounts::default();
    for row in rows {
        out.rows += 1;
        *out.subtype_counts.entry(row[3].clone()).or_insert(0) += 1;
        let replay_ready = row[1] != "0x";
        match row[2].as_str() {
            "cross_dex" => {
                out.cross_dex_rows += 1;
                if replay_ready {
                    out.cross_dex_replay_rows += 1;
                } else {
                    out.cross_dex_placeholder_rows += 1;
                }
            }
            "same_dex" => {
                out.same_dex_rows += 1;
                if replay_ready {
                    out.same_dex_replay_rows += 1;
                } else {
                    out.same_dex_placeholder_rows += 1;
                }
            }
            other => panic!("unknown taxonomy arb_type {other}"),
        }
    }
    out
}

#[test]
fn route_shape_sources_are_hash_pinned() {
    let matrix = read_json(MATRIX);
    assert_eq!(
        matrix["schema"].as_str(),
        Some("route_shape_profitability_matrix.v1")
    );
    assert_eq!(
        matrix["classification"].as_str(),
        Some("route_shape_profitability_not_release_clean")
    );
    for (name, source) in obj(&matrix, "sources")
        .as_object()
        .expect("sources must be object")
    {
        let path = workspace_path(&str_field(source, "path"));
        let metadata = fs::metadata(&path)
            .unwrap_or_else(|err| panic!("missing source {name} at {}: {err}", path.display()));
        assert_eq!(
            metadata.len(),
            u64_field(source, "bytes"),
            "source byte count drift for {name}"
        );
        assert_eq!(
            sha256_file(&path),
            str_field(source, "sha256"),
            "source hash drift for {name}"
        );
    }
}

#[test]
fn strict_detector_route_shape_counts_match_current_artifacts() {
    let matrix = read_json(MATRIX);
    let source = |name: &str| str_field(obj(obj(&matrix, "sources"), name), "path");
    let (total, by_hop, by_shape) = strict_detector_counts(&source("strict_detector_verdict"));
    let (blockers_by_hop, blockers_by_shape) =
        strict_blocker_counts(&source("strict_detector_row_classification"));

    let strict = obj(&matrix, "strict_detector");
    assert_eq!(total.rows, u64_field(strict, "total_rows"));
    assert_eq!(
        total.positive,
        u64_field(obj(strict, "outcome_counts"), "positive")
    );
    assert_eq!(total.loss, u64_field(obj(strict, "outcome_counts"), "loss"));
    assert_eq!(
        total.other_revert,
        u64_field(obj(strict, "outcome_counts"), "other_revert")
    );
    assert_eq!(
        total.all_net,
        i128_string_field(strict, "direct_all_route_net_wei")
    );

    for hop in ["2", "3", "4"] {
        let actual = *by_hop
            .get(hop)
            .unwrap_or_else(|| panic!("missing strict detector hop {hop}"));
        assert_eq!(
            counts_from_json(obj(obj(strict, "by_hop"), hop)),
            actual,
            "strict detector count mismatch for hop {hop}"
        );
        assert_eq!(
            blocker_counts_from_json(obj(obj(obj(strict, "by_hop"), hop), "blocker_classes")),
            *blockers_by_hop
                .get(hop)
                .unwrap_or_else(|| panic!("missing blocker counts for hop {hop}")),
            "strict blocker-class mismatch for hop {hop}"
        );
    }

    for shape in ["same_exchange", "cross_exchange"] {
        let actual = *by_shape
            .get(shape)
            .unwrap_or_else(|| panic!("missing strict detector shape {shape}"));
        assert_eq!(
            counts_from_json(obj(obj(strict, "by_venue_shape"), shape)),
            actual,
            "strict detector count mismatch for shape {shape}"
        );
        assert_eq!(
            blocker_counts_from_json(obj(
                obj(obj(strict, "by_venue_shape"), shape),
                "blocker_classes"
            )),
            *blockers_by_shape
                .get(shape)
                .unwrap_or_else(|| panic!("missing blocker counts for shape {shape}")),
            "strict blocker-class mismatch for shape {shape}"
        );
    }
}

#[test]
fn historical_route_shape_counts_match_current_sources() {
    let matrix = read_json(MATRIX);
    let source = |name: &str| str_field(obj(obj(&matrix, "sources"), name), "path");
    let catalog = catalog_counts(&source("anvil_catalog"));
    let taxonomy = taxonomy_counts(&source("dune_arb_taxonomy"));

    let historical = obj(&matrix, "historical_corpus");
    for (hop, count) in &catalog.hop_counts {
        assert_eq!(
            *count,
            u64_field(obj(historical, "catalog_hop_counts"), hop),
            "catalog hop-count mismatch for hop {hop}"
        );
    }
    assert_eq!(
        catalog.two_to_four_rows,
        u64_field(historical, "catalog_2_to_4_rows")
    );
    assert_eq!(
        catalog.cross_family_rows,
        u64_field(historical, "catalog_cross_family_rows")
    );
    assert_eq!(
        catalog.same_family_rows,
        u64_field(historical, "catalog_same_family_rows")
    );

    let tax = obj(historical, "taxonomy");
    assert_eq!(taxonomy.rows, u64_field(tax, "rows"));
    assert_eq!(taxonomy.cross_dex_rows, u64_field(tax, "cross_dex_rows"));
    assert_eq!(
        taxonomy.cross_dex_replay_rows,
        u64_field(tax, "cross_dex_replay_rows")
    );
    assert_eq!(
        taxonomy.cross_dex_placeholder_rows,
        u64_field(tax, "cross_dex_placeholder_rows")
    );
    assert_eq!(taxonomy.same_dex_rows, u64_field(tax, "same_dex_rows"));
    assert_eq!(
        taxonomy.same_dex_replay_rows,
        u64_field(tax, "same_dex_replay_rows")
    );
    assert_eq!(
        taxonomy.same_dex_placeholder_rows,
        u64_field(tax, "same_dex_placeholder_rows")
    );
    for (subtype, count) in taxonomy.subtype_counts {
        assert_eq!(
            count,
            u64_field(obj(tax, "subtype_counts"), &subtype),
            "taxonomy subtype count mismatch for {subtype}"
        );
    }
}

#[test]
fn route_shape_release_status_remains_red_or_yellow_not_green() {
    let matrix = read_json(MATRIX);
    let summary = obj(&matrix, "summary");
    assert_eq!(
        0,
        u64_field(summary, "strict_release_clean_route_shapes"),
        "no route shape is release-clean under current evidence"
    );
    assert_eq!(
        0,
        u64_field(summary, "strict_clean_hop_depths"),
        "2/3/4-hop positives cannot be treated as clean while loss/metadata rows remain"
    );
    assert_eq!(
        0,
        u64_field(summary, "strict_same_exchange_positive_rows"),
        "same-exchange detector proof must stay red until a positive strict same-exchange row exists"
    );
    assert_eq!(
        0,
        u64_field(summary, "taxonomy_same_dex_replay_rows"),
        "same-dex taxonomy placeholders are not replay-ready evidence"
    );

    let requirements = obj(&matrix, "release_requirements")
        .as_array()
        .expect("release_requirements must be array");
    assert_eq!(requirements.len(), 5);
    let red = requirements
        .iter()
        .filter(|row| row["status"].as_str() == Some("RED"))
        .count();
    let yellow = requirements
        .iter()
        .filter(|row| row["status"].as_str() == Some("YELLOW"))
        .count();
    assert_eq!(red, 1, "same-venue/normal route proof should be red");
    assert_eq!(
        yellow, 4,
        "remaining route-shape proof is partial, not green"
    );
    assert!(
        obj(summary, "overall_status")
            .as_str()
            .expect("overall_status must be string")
            .starts_with("RED:"),
        "overall route-shape status must remain red"
    );
}
