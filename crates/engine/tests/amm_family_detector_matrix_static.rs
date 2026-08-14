use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const MATRIX: &str = "../../../Codex/AMM_FAMILY_DETECTOR_CERTIFICATION_MATRIX_2026-06-15.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    positive: u64,
    loss: u64,
    other_revert: u64,
}

impl Counts {
    fn total(self) -> u64 {
        self.positive + self.loss + self.other_revert
    }

    fn inc(&mut self, key: &str) {
        match key {
            "positive" => self.positive += 1,
            "loss" => self.loss += 1,
            "other_revert" => self.other_revert += 1,
            other => panic!("unknown outcome {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BlockerClassCounts {
    foundry_positive: u64,
    state_point_sensitive_loss: u64,
    strict_metadata_missing: u64,
}

impl BlockerClassCounts {
    fn inc(&mut self, key: &str) {
        match key {
            "foundry_positive" => self.foundry_positive += 1,
            "state_point_sensitive_loss" => self.state_point_sensitive_loss += 1,
            "strict_metadata_missing" => self.strict_metadata_missing += 1,
            other => panic!("unknown blocker class {other}"),
        }
    }
}

#[derive(Debug)]
struct FamilyBlockerRow {
    registered_pool_count: u64,
    mantis_exchange_ids: Vec<u8>,
    detect_hop_hits: u64,
    strict_fork_verdict_reconstruction: bool,
    detector_corpus_scope: String,
    blocker: String,
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

fn opt_str_field(value: &Value, key: &str) -> Option<String> {
    match obj(value, key) {
        Value::Null => None,
        Value::String(raw) => Some(raw.clone()),
        other => panic!("field {key} must be string or null, got {other}"),
    }
}

fn bool_field(value: &Value, key: &str) -> bool {
    obj(value, key)
        .as_bool()
        .unwrap_or_else(|| panic!("missing bool field {key} in {value}"))
}

fn opt_bool_field(value: &Value, key: &str) -> Option<bool> {
    match obj(value, key) {
        Value::Null => None,
        Value::Bool(raw) => Some(*raw),
        other => panic!("field {key} must be bool or null, got {other}"),
    }
}

fn u64_field(value: &Value, key: &str) -> u64 {
    obj(value, key)
        .as_u64()
        .unwrap_or_else(|| panic!("missing u64 field {key} in {value}"))
}

fn u8_array(value: &Value, key: &str) -> Vec<u8> {
    obj(value, key)
        .as_array()
        .unwrap_or_else(|| panic!("field {key} must be array"))
        .iter()
        .map(|item| {
            item.as_u64()
                .unwrap_or_else(|| panic!("non-u64 item in {key}: {item}")) as u8
        })
        .collect()
}

fn matrix_rows(matrix: &Value) -> Vec<&Value> {
    obj(matrix, "rows")
        .as_array()
        .expect("rows must be array")
        .iter()
        .collect()
}

fn row_map<'a>(matrix: &'a Value) -> BTreeMap<String, &'a Value> {
    let mut out = BTreeMap::new();
    for row in matrix_rows(matrix) {
        let family = str_field(row, "registered_family");
        let old = out.insert(family.clone(), row);
        assert!(old.is_none(), "duplicate row for family {family}");
    }
    out
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

fn value_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    Some(rest.split_whitespace().next().unwrap_or(rest))
}

fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

fn parse_registered_families(path: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for raw_line in read(workspace_path(path)).lines() {
        let line = strip_ansi(raw_line);
        if !line.contains("registered pools by family") {
            continue;
        }
        let Some(family) = value_after(&line, "family=") else {
            continue;
        };
        let Some(count_raw) = value_after(&line, "count=") else {
            continue;
        };
        let count = count_raw.parse::<u64>().unwrap_or_else(|err| {
            panic!("invalid registered family count in line {line}: {err}");
        });
        out.insert(family.to_string(), count);
    }
    out
}

fn parse_exchange_ids(raw: &str, line_no: usize) -> Vec<u8> {
    raw.split('+')
        .filter(|value| !value.is_empty())
        .map(|value| {
            value.parse::<u8>().unwrap_or_else(|err| {
                panic!("line {line_no}: invalid Mantis exchange id {value}: {err}")
            })
        })
        .collect()
}

fn parse_bool(raw: &str, line_no: usize, label: &str) -> bool {
    match raw {
        "true" => true,
        "false" => false,
        other => panic!("line {line_no}: invalid {label} bool {other}"),
    }
}

fn parse_family_blockers(path: &str) -> BTreeMap<String, FamilyBlockerRow> {
    let rows = csv_rows(
        path,
        "family,registered_pool_count,mantis_exchange_ids,detect_hop_hits,strict_fork_verdict_reconstruction,detector_corpus_scope,blocker",
        7,
    );
    let mut out = BTreeMap::new();
    for (idx, row) in rows.iter().enumerate() {
        let line_no = idx + 2;
        let family = row[0].clone();
        let old = out.insert(
            family.clone(),
            FamilyBlockerRow {
                registered_pool_count: row[1].parse::<u64>().unwrap_or_else(|err| {
                    panic!("line {line_no}: invalid registered_pool_count: {err}")
                }),
                mantis_exchange_ids: parse_exchange_ids(&row[2], line_no),
                detect_hop_hits: row[3]
                    .parse::<u64>()
                    .unwrap_or_else(|err| panic!("line {line_no}: invalid detect_hop_hits: {err}")),
                strict_fork_verdict_reconstruction: parse_bool(
                    &row[4],
                    line_no,
                    "strict_fork_verdict_reconstruction",
                ),
                detector_corpus_scope: row[5].clone(),
                blocker: row[6].clone(),
            },
        );
        assert!(old.is_none(), "duplicate family blocker row for {family}");
    }
    out
}

fn strict_outcomes_by_exchange_id(path: &str) -> BTreeMap<u8, Counts> {
    let rows = csv_rows(
        path,
        "route_id,block_number,hop_count,exchange_ids,start_token,end_token,input_amount,gross_profit,outcome,realized_net_wei,reason,token_path,pool_path",
        13,
    );
    let mut out = BTreeMap::<u8, Counts>::new();
    for (idx, row) in rows.iter().enumerate() {
        let line_no = idx + 2;
        let outcome = row[8].as_str();
        for exchange_id in parse_exchange_ids(&row[3], line_no) {
            out.entry(exchange_id).or_default().inc(outcome);
        }
    }
    out
}

fn blocker_classes_by_family_name(path: &str) -> BTreeMap<String, BlockerClassCounts> {
    let rows = csv_rows(
        path,
        "route_id,detector_block_tag,hop_count,exchange_ids,family_path,start_is_weth,input_amount,gross_profit,foundry_outcome,foundry_realized_net_wei,first_revm_seq,first_revm_result,first_revm_balance_delta_wei,last_revm_seq,last_revm_result,last_revm_balance_delta_wei,last_revm_reject_reason,blocker_class",
        18,
    );
    let mut out = BTreeMap::<String, BlockerClassCounts>::new();
    for row in rows {
        let blocker_class = row[17].as_str();
        for family in row[4].split('>').filter(|family| !family.is_empty()) {
            out.entry(family.to_string())
                .or_default()
                .inc(blocker_class);
        }
    }
    out
}

fn counts_from_json(value: &Value) -> Counts {
    Counts {
        positive: u64_field(value, "positive"),
        loss: u64_field(value, "loss"),
        other_revert: u64_field(value, "other_revert"),
    }
}

fn blocker_counts_from_json(value: &Value) -> BlockerClassCounts {
    BlockerClassCounts {
        foundry_positive: u64_field(value, "foundry_positive"),
        state_point_sensitive_loss: u64_field(value, "state_point_sensitive_loss"),
        strict_metadata_missing: u64_field(value, "strict_metadata_missing"),
    }
}

fn exchange_id_name_map(matrix: &Value) -> BTreeMap<u8, String> {
    obj(matrix, "exchange_id_name_map")
        .as_object()
        .expect("exchange_id_name_map must be object")
        .iter()
        .map(|(id, name)| {
            (
                id.parse::<u8>()
                    .unwrap_or_else(|err| panic!("invalid exchange id key {id}: {err}")),
                name.as_str()
                    .unwrap_or_else(|| panic!("exchange id {id} name must be string"))
                    .to_string(),
            )
        })
        .collect()
}

fn catalog_family_counts(path: &str) -> BTreeMap<String, u64> {
    let matrix = read_json(workspace_path(path));
    obj(obj(&matrix, "catalog"), "family_counts")
        .as_object()
        .expect("catalog.family_counts must be object")
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_u64()
                    .unwrap_or_else(|| panic!("catalog count for {key} must be u64")),
            )
        })
        .collect()
}

fn foundry_family_pnl(path: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current_family: Option<String> = None;
    for raw_line in read(workspace_path(path)).lines() {
        let line = raw_line.trim();
        if let Some(family) = line.strip_prefix("family: ") {
            current_family = Some(family.to_string());
            continue;
        }
        let Some(delta_raw) = line.split(" delta (raw, +=profit): ").nth(1) else {
            continue;
        };
        if !line.starts_with("realized ") {
            continue;
        }
        let family = current_family
            .clone()
            .unwrap_or_else(|| panic!("realized delta line without preceding family: {line}"));
        let old = out.insert(family.clone(), delta_raw.to_string());
        assert!(old.is_none(), "duplicate Foundry PnL line for {family}");
    }
    out
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

fn expected_family_ids() -> BTreeMap<&'static str, Vec<u8>> {
    BTreeMap::from([
        ("algebra_v19", vec![9]),
        ("balancer_v2", vec![26, 27]),
        ("camelot_v2", vec![1]),
        ("curve_stableswap_ng", vec![18]),
        ("dodo_v2", vec![23]),
        ("pancake_v3", vec![6]),
        ("sushi_v3", vec![25]),
        ("trader_joe_lb", vec![15]),
        ("uniswap_v2", vec![24]),
        ("uniswap_v3", vec![0]),
        ("uniswap_v4", vec![20]),
    ])
}

#[test]
fn matrix_sources_are_hash_pinned_and_current() {
    let matrix = read_json(MATRIX);
    assert_eq!(
        matrix["schema"].as_str(),
        Some("amm_family_detector_certification_matrix.v1")
    );
    assert_eq!(
        matrix["classification"].as_str(),
        Some("registered_amm_family_detector_profitability_not_fully_certified")
    );
    assert_eq!(
        matrix["summary"]["overall_status"].as_str(),
        Some("RED: every registered family row is either missing strict detector coverage/reconstruction or appears in a non-clean strict detector artifact")
    );

    let sources = obj(&matrix, "sources")
        .as_object()
        .expect("sources must be object");
    assert!(
        sources.len() >= 9,
        "matrix must pin every raw source it uses"
    );

    for (name, source) in sources {
        let path = workspace_path(&str_field(source, "path"));
        let metadata = fs::metadata(&path)
            .unwrap_or_else(|err| panic!("missing source {name} at {}: {err}", path.display()));
        assert_eq!(
            metadata.len(),
            u64_field(source, "bytes"),
            "byte size drift for source {name}"
        );
        assert_eq!(
            sha256_file(&path),
            str_field(source, "sha256"),
            "sha256 drift for source {name}"
        );
    }
}

#[test]
fn matrix_rows_match_registered_families_detector_artifacts_and_history() {
    let matrix = read_json(MATRIX);
    let source = |key: &str| str_field(obj(obj(&matrix, "sources"), key), "path");

    let registered = parse_registered_families(&source("gate9_registered_family_log"));
    let family_blockers = parse_family_blockers(&source("family_strict_coverage_blockers"));
    let outcomes = strict_outcomes_by_exchange_id(&source("strict_detector_verdict"));
    let blocker_classes =
        blocker_classes_by_family_name(&source("strict_detector_row_classification"));
    let catalog_counts = catalog_family_counts(&source("historical_corpus_matrix"));
    let family_pnl = foundry_family_pnl(&source("family_realized_pnl_foundry_log"));
    let exchange_names = exchange_id_name_map(&matrix);
    let expected_ids = expected_family_ids();
    let rows = row_map(&matrix);

    assert_eq!(
        rows.len(),
        registered.len(),
        "matrix must cover every registered family"
    );
    assert_eq!(
        rows.len() as u64,
        matrix["summary"]["registered_family_rows"]
    );

    for (family, row) in &rows {
        assert_eq!(
            Some(&u64_field(row, "registered_pool_count")),
            registered.get(family),
            "registered pool count mismatch for {family}"
        );
        assert_eq!(
            &u8_array(row, "mantis_exchange_ids"),
            expected_ids
                .get(family.as_str())
                .unwrap_or_else(|| panic!("missing expected id contract for {family}")),
            "Mantis exchange id mismatch for {family}"
        );

        let ids = u8_array(row, "mantis_exchange_ids");
        let actual_outcomes = ids.iter().fold(Counts::default(), |mut acc, id| {
            let by_id = outcomes.get(id).copied().unwrap_or_default();
            acc.positive += by_id.positive;
            acc.loss += by_id.loss;
            acc.other_revert += by_id.other_revert;
            acc
        });
        assert_eq!(
            counts_from_json(obj(row, "strict_detector_hop_outcomes")),
            actual_outcomes,
            "strict detector outcome mismatch for {family}"
        );
        assert_eq!(
            u64_field(row, "strict_detect_hop_hits"),
            actual_outcomes.total(),
            "strict detect hit total mismatch for {family}"
        );

        let actual_blocker_classes =
            ids.iter()
                .fold(BlockerClassCounts::default(), |mut acc, id| {
                    let family_name = exchange_names
                        .get(id)
                        .unwrap_or_else(|| panic!("missing exchange id name for {id}"));
                    let by_family = blocker_classes
                        .get(family_name)
                        .copied()
                        .unwrap_or_default();
                    acc.foundry_positive += by_family.foundry_positive;
                    acc.state_point_sensitive_loss += by_family.state_point_sensitive_loss;
                    acc.strict_metadata_missing += by_family.strict_metadata_missing;
                    acc
                });
        assert_eq!(
            blocker_counts_from_json(obj(row, "strict_detector_blocker_classes")),
            actual_blocker_classes,
            "strict detector blocker-class mismatch for {family}"
        );

        match family_blockers.get(family.as_str()) {
            Some(blocker) => {
                assert_eq!(
                    Some(blocker.blocker.clone()),
                    opt_str_field(row, "family_blocker_csv_row"),
                    "blocker reason mismatch for {family}"
                );
                assert_eq!(
                    blocker.registered_pool_count,
                    u64_field(row, "registered_pool_count"),
                    "blocker registered count mismatch for {family}"
                );
                assert_eq!(
                    blocker.mantis_exchange_ids, ids,
                    "blocker exchange id mismatch for {family}"
                );
                assert_eq!(
                    blocker.detect_hop_hits,
                    u64_field(row, "strict_detect_hop_hits"),
                    "blocker detect hit mismatch for {family}"
                );
                assert_eq!(
                    blocker.strict_fork_verdict_reconstruction,
                    bool_field(row, "strict_fork_verdict_reconstruction"),
                    "blocker strict reconstruction mismatch for {family}"
                );
                assert!(
                    matches!(
                        blocker.detector_corpus_scope.as_str(),
                        "registered_not_detected" | "detected_unsupported"
                    ),
                    "unexpected blocker scope for {family}: {}",
                    blocker.detector_corpus_scope
                );
            }
            None => assert!(
                opt_str_field(row, "family_blocker_csv_row").is_none(),
                "non-blocked family {family} must not record a blocker CSV row"
            ),
        }

        match opt_str_field(row, "historical_catalog_alias") {
            Some(alias) => assert_eq!(
                catalog_counts.get(&alias).copied().unwrap_or_default(),
                u64_field(row, "historical_catalog_alias_count"),
                "historical catalog alias count mismatch for {family}"
            ),
            None => assert_eq!(
                0,
                u64_field(row, "historical_catalog_alias_count"),
                "family {family} has no alias but non-zero alias count"
            ),
        }

        match opt_str_field(row, "historical_tx_realized_pnl_label") {
            Some(label) => {
                let raw_delta = family_pnl
                    .get(&label)
                    .unwrap_or_else(|| panic!("missing Foundry family PnL label {label}"));
                assert_eq!(
                    raw_delta,
                    &str_field(row, "historical_tx_best_delta_raw"),
                    "Foundry family PnL raw delta mismatch for {family}"
                );
                assert_eq!(
                    *raw_delta != "0",
                    opt_bool_field(row, "historical_tx_realized_positive")
                        .expect("historical_tx_realized_positive must be bool when label exists"),
                    "Foundry family PnL positivity mismatch for {family}"
                );
            }
            None => assert!(
                opt_bool_field(row, "historical_tx_realized_positive").is_none(),
                "family {family} has no PnL label but has realized-positive flag"
            ),
        }
    }
}

#[test]
fn summary_and_release_blockers_are_recomputed_from_rows() {
    let matrix = read_json(MATRIX);
    let rows = matrix_rows(&matrix);
    let summary = obj(&matrix, "summary");

    let release_clean = rows
        .iter()
        .filter(|row| !bool_field(row, "release_blocker"))
        .count() as u64;
    let strict_positive = rows
        .iter()
        .filter(|row| u64_field(obj(row, "strict_detector_hop_outcomes"), "positive") > 0)
        .count() as u64;
    let strict_loss = rows
        .iter()
        .filter(|row| u64_field(obj(row, "strict_detector_hop_outcomes"), "loss") > 0)
        .count() as u64;
    let strict_metadata = rows
        .iter()
        .filter(|row| u64_field(obj(row, "strict_detector_hop_outcomes"), "other_revert") > 0)
        .count() as u64;
    let no_hits = rows
        .iter()
        .filter(|row| u64_field(row, "strict_detect_hop_hits") == 0)
        .count() as u64;
    let detected_but_missing_reconstruction = rows
        .iter()
        .filter(|row| {
            u64_field(row, "strict_detect_hop_hits") > 0
                && !bool_field(row, "strict_fork_verdict_reconstruction")
        })
        .count() as u64;
    let missing_reconstruction = rows
        .iter()
        .filter(|row| !bool_field(row, "strict_fork_verdict_reconstruction"))
        .count() as u64;
    let historical_alias = rows
        .iter()
        .filter(|row| opt_str_field(row, "historical_catalog_alias").is_some())
        .count() as u64;
    let positive_pnl_label = rows
        .iter()
        .filter(|row| opt_bool_field(row, "historical_tx_realized_positive") == Some(true))
        .count() as u64;
    let zero_pnl_label = rows
        .iter()
        .filter(|row| opt_bool_field(row, "historical_tx_realized_positive") == Some(false))
        .count() as u64;

    assert_eq!(release_clean, u64_field(summary, "release_clean_families"));
    assert_eq!(
        strict_positive,
        u64_field(summary, "families_with_strict_detector_positive_mentions")
    );
    assert_eq!(
        strict_loss,
        u64_field(summary, "families_with_strict_detector_loss_mentions")
    );
    assert_eq!(
        strict_metadata,
        u64_field(
            summary,
            "families_with_strict_detector_metadata_limited_mentions"
        )
    );
    assert_eq!(
        no_hits,
        u64_field(summary, "families_without_strict_detect_hits")
    );
    assert_eq!(
        detected_but_missing_reconstruction,
        u64_field(
            summary,
            "families_detected_but_strict_reconstruction_missing"
        )
    );
    assert_eq!(
        missing_reconstruction,
        u64_field(summary, "families_with_strict_reconstruction_missing")
    );
    assert_eq!(
        historical_alias,
        u64_field(summary, "families_with_historical_catalog_alias")
    );
    assert_eq!(
        positive_pnl_label,
        u64_field(
            summary,
            "families_with_historical_tx_positive_token_delta_label"
        )
    );
    assert_eq!(
        zero_pnl_label,
        u64_field(
            summary,
            "families_with_historical_tx_zero_token_delta_label"
        )
    );

    assert_eq!(
        0, release_clean,
        "current artifact must not certify any clean release family"
    );
    for row in rows {
        let family = str_field(row, "registered_family");
        let outcomes = counts_from_json(obj(row, "strict_detector_hop_outcomes"));
        let is_clean = u64_field(row, "strict_detect_hop_hits") > 0
            && outcomes.loss == 0
            && outcomes.other_revert == 0
            && bool_field(row, "strict_fork_verdict_reconstruction");
        assert!(
            !is_clean,
            "family {family} unexpectedly satisfies a clean release condition"
        );
        assert!(
            bool_field(row, "release_blocker"),
            "family {family} must remain a release blocker under current evidence"
        );
        assert!(
            str_field(row, "current_status").starts_with("RED:")
                || str_field(row, "current_status").starts_with("YELLOW:"),
            "family {family} must be red/yellow, not green"
        );
    }
}

#[test]
fn exchange_ids_family_strings_and_kernel_modules_are_backed_by_current_source() {
    let matrix = read_json(MATRIX);
    let source = |key: &str| {
        read(workspace_path(&str_field(
            obj(obj(&matrix, "sources"), key),
            "path",
        )))
    };
    let pool_rs = source("pool_kind_exchange_id_mapping_source");
    let startup_rs = source("family_string_mapping_source");
    let amm_lib_rs = source("amm_kernel_module_source");

    for snippet in [
        "PoolKind::UniswapV3 => 0",
        "PoolKind::CamelotV2 => 1",
        "PoolKind::PancakeV3 => 6",
        "PoolKind::AlgebraV19 => 9",
        "PoolKind::TraderJoeLb => 15",
        "PoolKind::CurveStableSwapNg => 18",
        "PoolKind::UniswapV4 => 20",
        "PoolKind::DodoV2 => 23",
        "PoolKind::UniswapV2 => 24",
        "PoolKind::SushiV3 => 25",
        "PoolKind::BalancerStable => 26",
        "PoolKind::BalancerWeighted => 27",
    ] {
        assert!(
            pool_rs.contains(snippet),
            "pool.rs missing exchange-id snippet {snippet}"
        );
    }

    for snippet in [
        "\"uniswap_v3\" => Some(PoolKind::UniswapV3)",
        "\"sushi_v3\" => Some(PoolKind::SushiV3)",
        "\"pancake_v3\" => Some(PoolKind::PancakeV3)",
        "\"algebra_v19\" => Some(PoolKind::AlgebraV19)",
        "\"dodo_v2\" | \"dodo_dvm\" | \"dodo_dpp\" | \"dodo_dsp\" => Some(PoolKind::DodoV2)",
        "\"curve_stableswap_ng\" => Some(PoolKind::CurveStableSwapNg)",
        "\"uniswap_v4\" => Some(PoolKind::UniswapV4)",
        "\"trader_joe_lb\" => Some(PoolKind::TraderJoeLb)",
        "\"camelot_v2\" => Some(PoolKind::CamelotV2)",
        "\"uniswap_v2\" => Some(PoolKind::UniswapV2)",
        "\"balancer_v2\" => Some(PoolKind::BalancerStable)",
    ] {
        assert!(
            startup_rs.contains(snippet),
            "startup.rs missing family mapping snippet {snippet}"
        );
    }

    for snippet in [
        "pub mod balancer_stable;",
        "pub mod balancer_weighted;",
        "pub mod curve;",
        "pub mod dodo_v2;",
        "pub mod trader_joe_lb;",
        "pub mod uniswap_v2;",
        "pub mod uniswap_v3;",
    ] {
        assert!(
            amm_lib_rs.contains(snippet),
            "engine-amm lib missing kernel module snippet {snippet}"
        );
    }
}
