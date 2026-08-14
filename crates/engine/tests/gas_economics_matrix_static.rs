use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const MATRIX: &str = "../../../Codex/GAS_ECONOMICS_CERTIFICATION_MATRIX_2026-06-15.json";

#[derive(Debug, Default)]
struct DodoGasLog {
    route_gas: BTreeMap<String, u64>,
    max_measured: u64,
    detector_gas: u64,
    submit_floor_gas: u64,
    wire_gas_limit: u64,
}

#[derive(Debug, Default)]
struct DodoGasCsv {
    rows: u64,
    route_ids: BTreeSet<u64>,
    blank_gross_profit_rows: u64,
    blank_l2_base_fee_rows: u64,
    blank_l1_fee_rows: u64,
    blank_total_gas_rows: u64,
    blank_net_after_measured_gas_rows: u64,
    detector_underbudget_rows: u64,
}

#[derive(Debug, Default)]
struct Gate9Economics {
    rows: u64,
    result_counts: BTreeMap<String, u64>,
    non_null_counts: BTreeMap<String, u64>,
    success_rows_with_net_profit_wei: u64,
    clean_accept_rows: u64,
    clean_accept_rows_with_non_positive_net_profit_wei: u64,
    net_negative_after_gas_route_ids: BTreeSet<u64>,
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

fn string_array(value: &Value, key: &str) -> Vec<String> {
    obj(value, key)
        .as_array()
        .unwrap_or_else(|| panic!("field {key} must be array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("non-string item in {key}: {item}"))
                .to_string()
        })
        .collect()
}

fn u64_array(value: &Value, key: &str) -> BTreeSet<u64> {
    obj(value, key)
        .as_array()
        .unwrap_or_else(|| panic!("field {key} must be array"))
        .iter()
        .map(|item| {
            item.as_u64()
                .unwrap_or_else(|| panic!("non-u64 item in {key}: {item}"))
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

fn last_u64(line: &str) -> u64 {
    line.split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .next_back()
        .unwrap_or_else(|| panic!("line has no numeric suffix: {line}"))
        .parse::<u64>()
        .unwrap_or_else(|err| panic!("invalid numeric suffix in {line}: {err}"))
}

fn parse_dodo_gas_log(path: &str) -> DodoGasLog {
    let raw = read(workspace_path(path));
    let mut out = DodoGasLog::default();
    for line in raw.lines() {
        if let Some(route_start) = line.find("route ") {
            if line.contains(" gasUsed:") {
                let route_part = &line[route_start + "route ".len()..];
                let route = route_part
                    .split_whitespace()
                    .next()
                    .expect("route id after route");
                out.route_gas.insert(route.to_string(), last_u64(line));
            }
            continue;
        }
        if line.contains("MAX gasUsed across DODO cycles:") {
            out.max_measured = last_u64(line);
        } else if line.contains("engine DODO/PMM detect assumption") {
            out.detector_gas = last_u64(line);
        } else if line.contains("engine 3-hop submit-floor gas_units") {
            out.submit_floor_gas = last_u64(line);
        } else if line.contains("on-wire gas_limit 3-hop") {
            out.wire_gas_limit = last_u64(line);
        }
    }
    out
}

fn parse_u64(raw: &str, line_no: usize, label: &str) -> u64 {
    raw.parse::<u64>()
        .unwrap_or_else(|err| panic!("line {line_no}: invalid {label} {raw}: {err}"))
}

fn parse_i128(raw: &str, label: &str) -> i128 {
    raw.parse::<i128>()
        .unwrap_or_else(|err| panic!("invalid {label} {raw}: {err}"))
}

fn parse_dodo_gas_csv(path: &str) -> DodoGasCsv {
    let raw = read(workspace_path(path));
    let mut out = DodoGasCsv::default();
    for (idx, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            assert_eq!(
                line,
                "route_id,hop_count,input_amount,gross_profit_wei,measured_gas_used,l2_base_fee_wei,l1_fee_wei,total_gas_wei,net_after_measured_gas_wei,detector_gas_units,detector_gas_gap_units,submit_floor_gas_units,wire_gas_limit,status,blocker",
                "unexpected DODO gas CSV header"
            );
            continue;
        }
        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(fields.len(), 15, "line {line_no}: malformed DODO gas row");
        out.rows += 1;
        out.route_ids
            .insert(parse_u64(fields[0], line_no, "route_id"));
        let measured = parse_u64(fields[4], line_no, "measured_gas_used");
        let detector_gas = parse_u64(fields[9], line_no, "detector_gas_units");
        let detector_gap = parse_u64(fields[10], line_no, "detector_gas_gap_units");
        assert_eq!(
            detector_gap,
            measured.saturating_sub(detector_gas),
            "line {line_no}: detector gas gap mismatch"
        );
        if detector_gap > 0 {
            out.detector_underbudget_rows += 1;
        }
        if fields[3].is_empty() {
            out.blank_gross_profit_rows += 1;
        }
        if fields[5].is_empty() {
            out.blank_l2_base_fee_rows += 1;
        }
        if fields[6].is_empty() {
            out.blank_l1_fee_rows += 1;
        }
        if fields[7].is_empty() {
            out.blank_total_gas_rows += 1;
        }
        if fields[8].is_empty() {
            out.blank_net_after_measured_gas_rows += 1;
        }
    }
    out
}

fn parse_gate9_economics(path: &str, fields: &[&str]) -> Gate9Economics {
    let raw = read(workspace_path(path));
    let mut out = Gate9Economics::default();
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.rows += 1;
        let row = serde_json::from_str::<Value>(line)
            .unwrap_or_else(|err| panic!("candidate line {} invalid JSON: {err}", idx + 1));
        let result = row
            .get("revm_result")
            .and_then(Value::as_str)
            .unwrap_or("missing")
            .to_string();
        *out.result_counts.entry(result).or_insert(0) += 1;
        let reject_reason = row
            .get("reject_reason")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        let net_profit = row.get("net_profit_wei").and_then(Value::as_str);
        if row.get("revm_result").and_then(Value::as_str) == Some("success") {
            if net_profit.is_some() {
                out.success_rows_with_net_profit_wei += 1;
            }
            if reject_reason == "none" {
                out.clean_accept_rows += 1;
                if net_profit
                    .map(|net| parse_i128(net, "clean net_profit_wei") <= 0)
                    .unwrap_or(true)
                {
                    out.clean_accept_rows_with_non_positive_net_profit_wei += 1;
                }
            } else if reject_reason.starts_with("net_negative_after_gas") {
                out.net_negative_after_gas_route_ids.insert(
                    row.get("route_id")
                        .and_then(Value::as_u64)
                        .expect("net-negative row must carry route_id"),
                );
            }
        }
        for field in fields {
            if row.get(*field).is_some_and(|value| !value.is_null()) {
                *out.non_null_counts.entry((*field).to_string()).or_insert(0) += 1;
            } else {
                out.non_null_counts.entry((*field).to_string()).or_insert(0);
            }
        }
    }
    out
}

#[test]
fn gas_economics_sources_are_hash_pinned() {
    let matrix = read_json(MATRIX);
    assert_eq!(
        matrix["schema"].as_str(),
        Some("gas_economics_certification_matrix.v1")
    );
    assert_eq!(
        matrix["classification"].as_str(),
        Some("gas_guardrail_partially_green_net_economics_not_certified")
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
            "byte-size drift for source {name}"
        );
        assert_eq!(
            sha256_file(&path),
            str_field(source, "sha256"),
            "sha256 drift for source {name}"
        );
    }
}

#[test]
fn dodo_gas_guardrail_matches_current_foundry_log_and_csv() {
    let matrix = read_json(MATRIX);
    let source = |name: &str| str_field(obj(obj(&matrix, "sources"), name), "path");
    let log = parse_dodo_gas_log(&source("dodo_gas_foundry_log"));
    let csv = parse_dodo_gas_csv(&source("dodo_gas_net_csv"));
    let dodo = obj(&matrix, "dodo_measured_gas");

    assert_eq!(log.route_gas, {
        let mut expected = BTreeMap::new();
        for (route, value) in obj(dodo, "route_gas_used")
            .as_object()
            .expect("route_gas_used must be object")
        {
            expected.insert(
                route.clone(),
                value
                    .as_u64()
                    .unwrap_or_else(|| panic!("route gas value must be u64 for {route}")),
            );
        }
        expected
    });
    assert_eq!(log.max_measured, u64_field(dodo, "max_measured_gas_used"));
    assert_eq!(log.detector_gas, u64_field(dodo, "detector_gas_units"));
    assert_eq!(
        log.submit_floor_gas,
        u64_field(dodo, "submit_floor_gas_units")
    );
    assert_eq!(log.wire_gas_limit, u64_field(dodo, "wire_gas_limit"));
    assert_eq!(
        log.max_measured.saturating_sub(log.detector_gas),
        u64_field(dodo, "detector_gap_units_max")
    );
    assert!(
        log.detector_gas >= log.max_measured,
        "DODO detector gas must cover measured max"
    );
    assert!(
        log.submit_floor_gas >= log.max_measured,
        "DODO submit floor must cover measured max"
    );
    assert!(
        log.wire_gas_limit > log.max_measured,
        "DODO on-wire gas limit must exceed measured max"
    );

    let csv_matrix = obj(&matrix, "dodo_gas_net_csv");
    assert_eq!(csv.rows, u64_field(csv_matrix, "rows"));
    assert_eq!(csv.route_ids, u64_array(csv_matrix, "route_ids"));
    assert_eq!(
        csv.blank_gross_profit_rows,
        u64_field(csv_matrix, "blank_gross_profit_rows")
    );
    assert_eq!(
        csv.blank_l2_base_fee_rows,
        u64_field(csv_matrix, "blank_l2_base_fee_rows")
    );
    assert_eq!(
        csv.blank_l1_fee_rows,
        u64_field(csv_matrix, "blank_l1_fee_rows")
    );
    assert_eq!(
        csv.blank_total_gas_rows,
        u64_field(csv_matrix, "blank_total_gas_rows")
    );
    assert_eq!(
        csv.blank_net_after_measured_gas_rows,
        u64_field(csv_matrix, "blank_net_after_measured_gas_rows")
    );
    assert_eq!(
        0, csv.detector_underbudget_rows,
        "DODO CSV should not contain current detector gas underbudget rows"
    );
}

#[test]
fn gate9_candidate_economics_fields_are_persisted_but_exact_gas_is_not_certified() {
    let matrix = read_json(MATRIX);
    let source = |name: &str| str_field(obj(obj(&matrix, "sources"), name), "path");
    let gate9 = obj(&matrix, "gate9_candidate_economics");
    let fields = string_array(gate9, "candidate_field_coverage_fields");
    let field_refs = fields.iter().map(String::as_str).collect::<Vec<_>>();
    let actual = parse_gate9_economics(&source("gate9_binary_candidates"), &field_refs);

    assert_eq!(actual.rows, u64_field(gate9, "rows"));
    for (result, count) in &actual.result_counts {
        assert_eq!(
            *count,
            u64_field(obj(gate9, "revm_result_counts"), result),
            "Gate9 result count mismatch for {result}"
        );
    }
    for field in &fields {
        assert_eq!(
            actual
                .non_null_counts
                .get(field)
                .copied()
                .unwrap_or_default(),
            u64_field(obj(gate9, "non_null_field_counts"), field),
            "Gate9 non-null field count mismatch for {field}"
        );
    }

    assert_eq!(
        actual.success_rows_with_net_profit_wei,
        u64_field(gate9, "success_rows_with_net_profit_wei")
    );
    assert_eq!(
        actual.clean_accept_rows,
        u64_field(gate9, "clean_accept_rows")
    );
    assert_eq!(
        actual.clean_accept_rows_with_non_positive_net_profit_wei,
        u64_field(gate9, "clean_accept_rows_with_non_positive_net_profit_wei")
    );
    assert_eq!(
        0, actual.clean_accept_rows_with_non_positive_net_profit_wei,
        "clean accepted rows must have positive net profit"
    );
    assert_eq!(
        actual
            .net_negative_after_gas_route_ids
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![16803, 21919],
        "successful-but-rejected net-negative route ids must remain explicit"
    );
    assert_eq!(
        246,
        u64_field(obj(gate9, "non_null_field_counts"), "estimated_gas_wei")
    );
    assert_eq!(
        29,
        u64_field(obj(gate9, "non_null_field_counts"), "net_profit_wei")
    );
    assert_eq!(
        246,
        u64_field(obj(gate9, "non_null_field_counts"), "l1_fee_wei")
    );
    assert_eq!(
        246,
        u64_field(obj(gate9, "non_null_field_counts"), "calldata")
    );
}

#[test]
fn gas_documentation_and_engine_primitives_are_present_but_not_sufficient() {
    let matrix = read_json(MATRIX);
    let source = |name: &str| str_field(obj(obj(&matrix, "sources"), name), "path");
    let requirements = read_json(workspace_path(&source("gas_net_requirements")));
    let docs = obj(&requirements, "documentation_basis").to_string();
    for required in string_array(obj(&matrix, "documentation_basis"), "required_terms") {
        assert!(
            docs.contains(&required),
            "gas/net requirements missing documentation basis term {required}"
        );
    }

    let engine_gas = read(workspace_path(&source("engine_gas_source")));
    for function_name in string_array(obj(&matrix, "engine_gas_model"), "required_functions") {
        assert!(
            engine_gas.contains(&format!("fn {function_name}")),
            "engine gas source missing required function {function_name}"
        );
    }
    for field in string_array(obj(&matrix, "engine_gas_model"), "required_snapshot_fields") {
        assert!(
            engine_gas.contains(&format!("pub {field}:")),
            "engine gas source missing required snapshot field {field}"
        );
    }

    let pipeline = read(workspace_path(&source("engine_pipeline_source")));
    assert!(
        pipeline.contains("let gas_units: u64 = 960_000"),
        "DODO pipeline detector gas constant must remain at the measured safe value"
    );
    assert!(
        pipeline.contains("l1_data_fee_wei(gas_snap, calldata_bytes)"),
        "pipeline should include L1 data fee in DODO profit terms"
    );

    let release_requirements = obj(&matrix, "release_requirements")
        .as_array()
        .expect("release_requirements must be array");
    assert_eq!(release_requirements.len(), 5);
    let green = release_requirements
        .iter()
        .filter(|row| row["status"].as_str() == Some("GREEN"))
        .count();
    let yellow = release_requirements
        .iter()
        .filter(|row| row["status"].as_str() == Some("YELLOW"))
        .count();
    let red = release_requirements
        .iter()
        .filter(|row| row["status"].as_str() == Some("RED"))
        .count();
    assert_eq!(
        green, 2,
        "DODO gas guardrail and monitored B L1/L2 fee field persistence are green"
    );
    assert_eq!(
        yellow, 1,
        "candidate gas/net term persistence is partial because loss/revert rows do not carry net"
    );
    assert_eq!(
        red, 2,
        "exact gas replay and release-level gas/net certification remain red"
    );
    assert!(
        obj(&matrix, "summary")["overall_status"]
            .as_str()
            .expect("overall_status string")
            .starts_with("RED:"),
        "overall gas/economics matrix must remain red"
    );
}
