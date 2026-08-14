use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const MATRIX: &str = "../../../Codex/RELEASE_BLOCKER_MATRIX_2026-06-15.json";

const REQUIRED_STANDALONE_FIELDS: &[&str] = &[
    "exchange_ids",
    "mantis_calldata",
    "executor_calldata",
    "calldata",
    "changed_slots",
    "payee",
    "mantis_executor",
    "snapshot_block",
    "block_env",
    "block_timestamp",
    "gas_limit",
    "min_profit",
    "exchange_metadata",
];

#[derive(Debug)]
struct StrictSummary {
    rows: u64,
    unique_route_ids: u64,
    unique_route_block_pairs: u64,
    outcome_counts: BTreeMap<String, u64>,
    hop_counts: BTreeMap<String, u64>,
    exchange_id_hop_counts: BTreeMap<String, u64>,
    positive_net_sum: i128,
    loss_net_sum: i128,
    direct_all_route_net: i128,
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

fn map_u64(value: &Value, key: &str) -> BTreeMap<String, u64> {
    obj(value, key)
        .as_object()
        .unwrap_or_else(|| panic!("field {key} must be object in {value}"))
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_u64()
                    .unwrap_or_else(|| panic!("field {key} value must be u64")),
            )
        })
        .collect()
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    obj(value, key)
        .as_array()
        .unwrap_or_else(|| panic!("field {key} must be array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("non-string item in {key}"))
                .to_string()
        })
        .collect()
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

fn csv_rows(path: &str, expected_header: &str, expected_cols: usize) -> Vec<Vec<String>> {
    let raw = read(workspace_path(path));
    raw.lines()
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

fn inc(map: &mut BTreeMap<String, u64>, key: impl Into<String>) {
    *map.entry(key.into()).or_insert(0) += 1;
}

fn parse_u64(raw: &str, label: &str) -> u64 {
    raw.parse::<u64>()
        .unwrap_or_else(|err| panic!("invalid {label} {raw}: {err}"))
}

fn parse_i128(raw: &str, label: &str) -> i128 {
    raw.parse::<i128>()
        .unwrap_or_else(|err| panic!("invalid {label} {raw}: {err}"))
}

fn strict_summary(path: &str) -> StrictSummary {
    let rows = csv_rows(
        path,
        "route_id,block_number,hop_count,exchange_ids,start_token,end_token,input_amount,gross_profit,outcome,realized_net_wei,reason,token_path,pool_path",
        13,
    );
    let mut route_ids = BTreeSet::new();
    let mut route_block_pairs = BTreeSet::new();
    let mut outcome_counts = BTreeMap::new();
    let mut hop_counts = BTreeMap::new();
    let mut exchange_id_hop_counts = BTreeMap::new();
    let mut positive_net_sum = 0i128;
    let mut loss_net_sum = 0i128;

    for row in &rows {
        route_ids.insert(row[0].clone());
        route_block_pairs.insert(format!("{}:{}", row[0], row[1]));
        inc(&mut hop_counts, row[2].clone());
        for exchange_id in row[3].split('+') {
            inc(&mut exchange_id_hop_counts, exchange_id.to_string());
        }
        let outcome = row[8].clone();
        inc(&mut outcome_counts, outcome.clone());
        match outcome.as_str() {
            "positive" => positive_net_sum += parse_i128(&row[9], "positive net"),
            "loss" => loss_net_sum += parse_i128(&row[9], "loss net"),
            "other_revert" => assert!(row[9].is_empty(), "other_revert net should be empty"),
            other => panic!("unknown strict outcome {other}"),
        }
    }

    StrictSummary {
        rows: rows.len() as u64,
        unique_route_ids: route_ids.len() as u64,
        unique_route_block_pairs: route_block_pairs.len() as u64,
        outcome_counts,
        hop_counts,
        exchange_id_hop_counts,
        positive_net_sum,
        loss_net_sum,
        direct_all_route_net: positive_net_sum + loss_net_sum,
    }
}

fn result_counts(rows: &[Value]) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for row in rows {
        let result = row
            .get("revm_result")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        inc(&mut out, result.to_string());
    }
    out
}

fn jsonl(path: &str) -> Vec<Value> {
    read(workspace_path(path))
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{path} line {} invalid JSON: {err}", idx + 1))
        })
        .collect()
}

fn count_has_field(rows: &[Value], key: &str) -> u64 {
    rows.iter().filter(|row| row.get(key).is_some()).count() as u64
}

fn count_non_null_field(rows: &[Value], key: &str) -> u64 {
    rows.iter()
        .filter(|row| row.get(key).is_some_and(|value| !value.is_null()))
        .count() as u64
}

fn last_u64(line: &str) -> u64 {
    line.split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .next_back()
        .unwrap_or_else(|| panic!("line has no numeric suffix: {line}"))
        .parse::<u64>()
        .unwrap_or_else(|err| panic!("invalid numeric suffix in {line}: {err}"))
}

#[test]
fn release_blocker_sources_are_hash_pinned() {
    let matrix = read_json(MATRIX);
    assert_eq!(matrix["schema"].as_str(), Some("release_blocker_matrix.v1"));
    assert_eq!(
        matrix["classification"].as_str(),
        Some("release_money_path_red_current")
    );
    let sources = obj(&matrix, "sources")
        .as_object()
        .expect("sources must be object");
    assert_eq!(sources.len(), 11, "unexpected source count");
    for (name, source) in sources {
        let path = workspace_path(&str_field(source, "path"));
        let metadata = fs::metadata(&path)
            .unwrap_or_else(|err| panic!("metadata for source {name} {}: {err}", path.display()));
        assert_eq!(
            metadata.len(),
            u64_field(source, "bytes"),
            "source byte size drifted for {name}"
        );
        assert_eq!(
            sha256_file(&path),
            str_field(source, "sha256"),
            "source hash drifted for {name}"
        );
    }
}

#[test]
fn strict_detector_and_row_classification_match_current_red_release_matrix() {
    let matrix = read_json(MATRIX);
    let source = obj(obj(&matrix, "sources"), "strict_detector_verdict");
    let summary = strict_summary(&str_field(source, "path"));
    let strict = obj(&matrix, "strict_detector");

    assert_eq!(summary.rows, u64_field(strict, "rows"));
    assert_eq!(
        summary.unique_route_ids,
        u64_field(strict, "unique_route_ids")
    );
    assert_eq!(
        summary.unique_route_block_pairs,
        u64_field(strict, "unique_route_block_pairs")
    );
    assert_eq!(summary.outcome_counts, map_u64(strict, "outcome_counts"));
    assert_eq!(summary.hop_counts, map_u64(strict, "hop_counts"));
    assert_eq!(
        summary.exchange_id_hop_counts,
        map_u64(strict, "exchange_id_hop_counts")
    );
    assert_eq!(
        summary.positive_net_sum,
        i128_string_field(strict, "positive_net_sum_wei")
    );
    assert_eq!(
        summary.loss_net_sum,
        i128_string_field(strict, "loss_net_sum_wei")
    );
    assert_eq!(
        summary.direct_all_route_net,
        i128_string_field(strict, "direct_all_route_net_wei")
    );
    assert_eq!(
        summary
            .outcome_counts
            .get("loss")
            .copied()
            .unwrap_or_default(),
        11,
        "release gate must remain red while direct loss rows exist"
    );
    assert_eq!(
        summary
            .outcome_counts
            .get("other_revert")
            .copied()
            .unwrap_or_default(),
        6,
        "release gate must remain red while metadata-limited rows exist"
    );

    let classification_source = obj(
        obj(&matrix, "sources"),
        "strict_detector_row_classification",
    );
    let rows = csv_rows(
        &str_field(classification_source, "path"),
        "route_id,detector_block_tag,hop_count,exchange_ids,family_path,start_is_weth,input_amount,gross_profit,foundry_outcome,foundry_realized_net_wei,first_revm_seq,first_revm_result,first_revm_balance_delta_wei,last_revm_seq,last_revm_result,last_revm_balance_delta_wei,last_revm_reject_reason,blocker_class",
        18,
    );
    let mut blocker_counts = BTreeMap::new();
    let mut outcome_counts = BTreeMap::new();
    let mut first_revm_result_counts = BTreeMap::new();
    let mut last_revm_result_counts = BTreeMap::new();
    for row in &rows {
        inc(&mut blocker_counts, row[17].clone());
        inc(&mut outcome_counts, row[8].clone());
        inc(&mut first_revm_result_counts, row[11].clone());
        inc(&mut last_revm_result_counts, row[14].clone());
    }
    let row_classification = obj(&matrix, "row_classification");
    assert_eq!(rows.len() as u64, u64_field(row_classification, "rows"));
    assert_eq!(
        blocker_counts,
        map_u64(row_classification, "blocker_counts")
    );
    assert_eq!(
        outcome_counts,
        map_u64(row_classification, "foundry_outcome_counts")
    );
    assert_eq!(
        first_revm_result_counts,
        map_u64(row_classification, "first_revm_result_counts")
    );
    assert_eq!(
        last_revm_result_counts,
        map_u64(row_classification, "last_revm_result_counts")
    );
}

#[test]
fn metadata_recovery_is_positive_but_candidate_metadata_persistence_is_still_missing() {
    let matrix = read_json(MATRIX);
    let metadata = obj(&matrix, "strict_metadata");

    let requirement_source = obj(obj(&matrix, "sources"), "strict_metadata_requirements");
    let requirements = csv_rows(
        &str_field(requirement_source, "path"),
        "route_id,input_amount,hop_count,exchange_ids,family_path,foundry_outcome,first_revm_result,last_revm_result,stable_revm_success,required_missing_metadata",
        10,
    );
    let mut stable_counts = BTreeMap::new();
    let mut required_counts = BTreeMap::new();
    for row in &requirements {
        inc(&mut stable_counts, row[8].clone());
        inc(&mut required_counts, row[9].clone());
    }
    assert_eq!(
        requirements.len() as u64,
        u64_field(metadata, "requirement_rows")
    );
    assert_eq!(
        stable_counts,
        map_u64(metadata, "stable_revm_success_counts")
    );
    assert_eq!(
        required_counts,
        map_u64(metadata, "required_metadata_counts")
    );

    let recovery_source = obj(obj(&matrix, "sources"), "strict_metadata_recovery");
    let recovery_rows = csv_rows(
        &str_field(recovery_source, "path"),
        "pool_address,family,cache_head_block,metadata_value,used_by_strict_routes,certification_meaning",
        6,
    );
    let mut recovery_family_rows = BTreeMap::new();
    let mut unique_routes = BTreeSet::new();
    for row in &recovery_rows {
        inc(&mut recovery_family_rows, row[1].clone());
        for route in row[4].split(';').filter(|route| !route.is_empty()) {
            unique_routes.insert(route.to_string());
        }
    }
    assert_eq!(
        recovery_rows.len() as u64,
        u64_field(metadata, "discovery_recovery_rows")
    );
    assert_eq!(
        unique_routes.len() as u64,
        u64_field(metadata, "discovery_recovery_unique_routes")
    );
    assert_eq!(
        recovery_family_rows,
        map_u64(metadata, "recovery_family_rows")
    );

    let log_source = obj(obj(&matrix, "sources"), "strict_metadata_foundry_log");
    let log = read(workspace_path(&str_field(log_source, "path")));
    let mut tested = None;
    let mut positive = None;
    let mut loss = None;
    let mut other = None;
    let mut sum_net = None;
    for line in log.lines() {
        if line.contains("metadata rows tested:") {
            tested = Some(last_u64(line));
        } else if line.contains("metadata rows positive:") {
            positive = Some(last_u64(line));
        } else if line.contains("metadata rows loss:") {
            loss = Some(last_u64(line));
        } else if line.contains("metadata rows other:") {
            other = Some(last_u64(line));
        } else if line.contains("metadata rows sum net:") {
            sum_net = Some(last_u64(line) as i128);
        }
    }
    assert_eq!(
        tested.expect("metadata tested count"),
        u64_field(metadata, "foundry_recovered_rows_tested")
    );
    assert_eq!(
        positive.expect("metadata positive count"),
        u64_field(metadata, "foundry_recovered_positive")
    );
    assert_eq!(
        loss.expect("metadata loss count"),
        u64_field(metadata, "foundry_recovered_loss")
    );
    assert_eq!(
        other.expect("metadata other count"),
        u64_field(metadata, "foundry_recovered_other")
    );
    assert_eq!(
        sum_net.expect("metadata net sum"),
        i128_string_field(metadata, "foundry_recovered_sum_net_wei")
    );
}

#[test]
fn state_sensitive_losses_and_gate9_schema_are_release_blockers() {
    let matrix = read_json(MATRIX);
    let state_source = obj(obj(&matrix, "sources"), "state_point_sensitive_losses");
    let rows = csv_rows(
        &str_field(state_source, "path"),
        "route_id,detector_block_tag,hop_count,exchange_ids,family_path,start_is_weth,input_amount,gross_profit,foundry_loss_wei,occurrences,success_occurrences,loss_occurrences,first_seq,first_balance_delta_wei,last_seq,last_loss_shortfall_wei,last_gas_used,seq_results,token_path,pool_path",
        20,
    );
    let mut total_occurrences = 0u64;
    let mut success_occurrences = 0u64;
    let mut loss_occurrences = 0u64;
    let mut max_last_gas_used = 0u64;
    let mut hop_counts = BTreeMap::new();
    let mut exchange_id_hop_counts = BTreeMap::new();
    for row in &rows {
        inc(&mut hop_counts, row[2].clone());
        for exchange_id in row[3].split('+') {
            inc(&mut exchange_id_hop_counts, exchange_id.to_string());
        }
        total_occurrences += parse_u64(&row[9], "occurrences");
        success_occurrences += parse_u64(&row[10], "success_occurrences");
        loss_occurrences += parse_u64(&row[11], "loss_occurrences");
        max_last_gas_used = max_last_gas_used.max(parse_u64(&row[16], "last_gas_used"));
    }
    let state = obj(&matrix, "state_point_sensitive_losses");
    assert_eq!(rows.len() as u64, u64_field(state, "rows"));
    assert_eq!(total_occurrences, u64_field(state, "total_occurrences"));
    assert_eq!(success_occurrences, u64_field(state, "success_occurrences"));
    assert_eq!(loss_occurrences, u64_field(state, "loss_occurrences"));
    assert_eq!(hop_counts, map_u64(state, "hop_counts"));
    assert_eq!(
        exchange_id_hop_counts,
        map_u64(state, "exchange_id_hop_counts")
    );
    assert_eq!(max_last_gas_used, u64_field(state, "max_last_gas_used"));

    let exact_source = obj(obj(&matrix, "sources"), "state_point_exact_replay_status");
    let exact_rows = csv_rows(
        &str_field(exact_source, "path"),
        "route_id,detector_block_tag,input_amount,first_seq_status,last_seq_status,foundry_exact_state_status,revm_status_match,blocker",
        8,
    );
    let missing_exact_inputs = exact_rows
        .iter()
        .filter(|row| row[5] == "missing_standalone_replay_inputs")
        .count() as u64;
    assert_eq!(
        exact_rows.len() as u64,
        u64_field(state, "exact_replay_status_rows")
    );
    assert_eq!(
        missing_exact_inputs,
        u64_field(state, "exact_replay_missing_standalone_inputs")
    );

    let gate9_source = obj(obj(&matrix, "sources"), "gate9_binary_candidates");
    let candidates = jsonl(&str_field(gate9_source, "path"));
    let schema = obj(&matrix, "gate9_candidate_schema");
    assert_eq!(candidates.len() as u64, u64_field(schema, "rows"));
    assert_eq!(
        result_counts(&candidates),
        map_u64(schema, "revm_result_counts")
    );
    let required_fields = string_array(schema, "required_standalone_fields");
    assert_eq!(
        required_fields,
        REQUIRED_STANDALONE_FIELDS
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
    );
    let rows_with_all_required = candidates
        .iter()
        .filter(|row| {
            REQUIRED_STANDALONE_FIELDS
                .iter()
                .all(|field| row.get(*field).is_some())
        })
        .count() as u64;
    assert_eq!(
        rows_with_all_required,
        u64_field(schema, "rows_with_all_required_standalone_fields")
    );
    let mut field_presence = BTreeMap::new();
    for field in REQUIRED_STANDALONE_FIELDS {
        field_presence.insert((*field).to_string(), count_has_field(&candidates, field));
    }
    assert_eq!(
        field_presence,
        map_u64(schema, "required_field_presence_counts")
    );
    assert_eq!(
        count_non_null_field(&candidates, "balance_delta_wei"),
        u64_field(schema, "rows_with_balance_delta")
    );
    assert_eq!(
        count_non_null_field(&candidates, "estimated_gas_wei"),
        u64_field(schema, "rows_with_estimated_gas")
    );
    assert_eq!(
        count_non_null_field(&candidates, "net_profit_wei"),
        u64_field(schema, "rows_with_net_profit")
    );
}

#[test]
fn gas_family_and_loss_acceptance_blockers_match_current_artifacts() {
    let matrix = read_json(MATRIX);

    let gas_source = obj(obj(&matrix, "sources"), "dodo_gas_foundry_log");
    let gas_log = read(workspace_path(&str_field(gas_source, "path")));
    let mut measured_route_gas = BTreeMap::new();
    let mut max_measured = None;
    let mut detector_assumption = None;
    let mut submit_floor = None;
    let mut on_wire = None;
    for line in gas_log.lines() {
        let Some(route_start) = line.find("route ") else {
            if line.contains("MAX gasUsed across DODO cycles:") {
                max_measured = Some(last_u64(line));
            } else if line.contains("engine DODO/PMM detect assumption") {
                detector_assumption = Some(last_u64(line));
            } else if line.contains("engine 3-hop submit-floor gas_units") {
                submit_floor = Some(last_u64(line));
            } else if line.contains("on-wire gas_limit 3-hop") {
                on_wire = Some(last_u64(line));
            }
            continue;
        };
        if !line.contains(" gasUsed:") {
            continue;
        }
        let route_part = &line[route_start + "route ".len()..];
        let route = route_part
            .split_whitespace()
            .next()
            .expect("route id after route");
        measured_route_gas.insert(route.to_string(), last_u64(line));
    }
    let gas = obj(&matrix, "dodo_gas");
    assert_eq!(measured_route_gas, map_u64(gas, "measured_route_gas_used"));
    assert_eq!(
        max_measured.expect("max gas"),
        u64_field(gas, "max_measured_gas_used")
    );
    assert_eq!(
        detector_assumption.expect("detector gas"),
        u64_field(gas, "detector_assumption_gas_units")
    );
    assert_eq!(
        submit_floor.expect("submit floor gas"),
        u64_field(gas, "submit_floor_gas_units")
    );
    assert_eq!(
        on_wire.expect("on-wire gas"),
        u64_field(gas, "on_wire_gas_limit")
    );
    assert_eq!(
        max_measured
            .unwrap()
            .saturating_sub(detector_assumption.unwrap()),
        u64_field(gas, "detector_under_budget_by_gas_units")
    );
    assert!(
        detector_assumption.unwrap() >= max_measured.unwrap(),
        "DODO detector gas must cover the measured fork worst case"
    );
    assert!(
        submit_floor.unwrap() >= max_measured.unwrap(),
        "DODO submit floor must cover the measured fork worst case"
    );
    assert!(
        on_wire.unwrap() > max_measured.unwrap(),
        "DODO on-wire gas limit must exceed the measured fork worst case"
    );
    assert!(
        str_field(gas, "release_status").contains("measured DODO sample gas guardrail"),
        "DODO gas status should distinguish sample gas coverage from full gas/net certification"
    );

    let family_source = obj(obj(&matrix, "sources"), "family_strict_coverage_blockers");
    let family_rows = csv_rows(
        &str_field(family_source, "path"),
        "family,registered_pool_count,mantis_exchange_ids,detect_hop_hits,strict_fork_verdict_reconstruction,detector_corpus_scope,blocker",
        7,
    );
    let mut missing_reconstruction_rows = 0u64;
    let mut no_detect_hit_rows = 0u64;
    let mut scope_counts = BTreeMap::new();
    let mut blocker_counts = BTreeMap::new();
    for row in &family_rows {
        if row[4] == "false" {
            missing_reconstruction_rows += 1;
        }
        if row[3] == "0" {
            no_detect_hit_rows += 1;
        }
        inc(&mut scope_counts, row[5].clone());
        inc(&mut blocker_counts, row[6].clone());
    }
    let family = obj(&matrix, "family_strict_coverage");
    assert_eq!(family_rows.len() as u64, u64_field(family, "blocker_rows"));
    assert_eq!(
        missing_reconstruction_rows,
        u64_field(family, "missing_strict_reconstruction_rows")
    );
    assert_eq!(no_detect_hit_rows, u64_field(family, "no_detect_hit_rows"));
    assert_eq!(scope_counts, map_u64(family, "scope_counts"));
    assert_eq!(blocker_counts, map_u64(family, "blocker_counts"));

    let loss_source = obj(obj(&matrix, "sources"), "loss_acceptance_classification");
    let loss_rows = csv_rows(
        &str_field(loss_source, "path"),
        "file,classification,profit_gate",
        3,
    );
    let mut classification_counts = BTreeMap::new();
    let mut profit_gate_counts = BTreeMap::new();
    for row in &loss_rows {
        inc(&mut classification_counts, row[1].clone());
        inc(&mut profit_gate_counts, row[2].clone());
    }
    let loss = obj(&matrix, "loss_acceptance_classification");
    assert_eq!(loss_rows.len() as u64, u64_field(loss, "rows"));
    assert_eq!(
        classification_counts,
        map_u64(loss, "classification_counts")
    );
    assert_eq!(profit_gate_counts, map_u64(loss, "profit_gate_counts"));
    assert_eq!(
        profit_gate_counts.get("no").copied().unwrap_or_default(),
        15,
        "loss/mechanical tests must remain classified outside profit proof"
    );
}
