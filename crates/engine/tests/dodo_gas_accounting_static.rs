use std::{collections::BTreeSet, fs, path::Path};

use serde_json::Value;

const PIPELINE_RS: &str = "src/pipeline.rs";
const POOL_RS: &str = "../engine-state/src/pool.rs";
const GAS_ORACLE_TEST: &str = "../../contracts/test/MantisGasOracle.Fork.t.sol";
const GAS_ORACLE_LOG: &str = "../../../codex_tmp/foundry/gas_oracle_2026-06-15.log";
const ENGINE_GAS_LIB: &str = "../engine-gas/src/lib.rs";
const GATE9_B_CANDIDATES: &str =
    "../../../codex_tmp/gate9_revm_preflight/gate9/B_binary_candidates_revm.jsonl";
const DODO_GAS_NET_CSV: &str = "../../../Codex/DODO_PMM_GAS_NET_CANDIDATES_2026-06-14.csv";
const GAS_NET_REQUIREMENTS: &str =
    "../../../Codex/GAS_NET_CERTIFICATION_REQUIREMENTS_2026-06-14.json";

const MEASURED_DODO_GAS_USED: [u64; 5] = [711_810, 767_097, 723_622, 733_820, 933_529];
const MEASURED_DODO_ROUTE_IDS: [u64; 5] = [2_665, 2_967, 2_664, 2_668, 2_972];
const DODO_DETECTOR_GAS_UNITS: u64 = 960_000;
const DODO_SUBMIT_3HOP_FLOOR_GAS_UNITS: u64 = 960_000;
const DODO_SUBMIT_3HOP_WIRE_GAS_LIMIT: u64 = 1_300_000;
const DODO_GAS_ORACLE_AMOUNT_IN: &str = "20000000000000000";

fn read_required(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("read {path}: {err}"))
}

fn first_u64_after_marker(source: &str, marker: &str, needle: &str) -> u64 {
    let marker_pos = source
        .find(marker)
        .unwrap_or_else(|| panic!("missing marker {marker}"));
    source[marker_pos..]
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing needle {needle} after {marker}"))
        .split(needle)
        .nth(1)
        .unwrap_or_else(|| panic!("missing rhs for {needle}"))
        .trim()
        .trim_end_matches(';')
        .replace('_', "")
        .parse::<u64>()
        .unwrap_or_else(|err| panic!("parse gas constant after {needle}: {err}"))
}

fn json_string_or_null(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .unwrap_or_else(|| panic!("candidate missing field {key}: {row}"))
        .as_str()
        .map(str::to_string)
}

fn parse_u64_field(value: &str, label: &str, line_no: usize) -> u64 {
    value
        .parse::<u64>()
        .unwrap_or_else(|err| panic!("line {line_no}: invalid {label} {value}: {err}"))
}

fn read_json(path: &str) -> Value {
    let raw = read_required(path);
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("{path} invalid JSON: {err}"))
}

fn json_string_set(value: &Value, field: &str) -> BTreeSet<String> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be array"))
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .unwrap_or_else(|| panic!("{field} entries must be strings"))
                .to_string()
        })
        .collect()
}

fn json_u64_set(value: &Value, field: &str) -> BTreeSet<u64> {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be array"))
        .iter()
        .map(|entry| {
            entry
                .as_u64()
                .unwrap_or_else(|| panic!("{field} entries must be numbers"))
        })
        .collect()
}

#[derive(Debug, Default)]
struct Gate9EconomicsCounts {
    rows: usize,
    success_rows: usize,
    loss_rows: usize,
    gross_profit_null_rows: usize,
    estimated_gas_null_rows: usize,
    net_profit_null_rows: usize,
}

fn gate9_economics_counts() -> Gate9EconomicsCounts {
    let raw = read_required(GATE9_B_CANDIDATES);
    let mut counts = Gate9EconomicsCounts::default();

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        counts.rows += 1;
        let row = serde_json::from_str::<Value>(line)
            .unwrap_or_else(|err| panic!("candidate line {} invalid JSON: {err}", idx + 1));
        match row["revm_result"]
            .as_str()
            .unwrap_or_else(|| panic!("candidate line {} missing revm_result", idx + 1))
        {
            "success" => counts.success_rows += 1,
            "loss" => counts.loss_rows += 1,
            _ => {}
        }
        if row["gross_profit_wei"].is_null() {
            counts.gross_profit_null_rows += 1;
        }
        if row["estimated_gas_wei"].is_null() {
            counts.estimated_gas_null_rows += 1;
        }
        if row["net_profit_wei"].is_null() {
            counts.net_profit_null_rows += 1;
        }
    }

    counts
}

#[derive(Debug, Default)]
struct DodoGasCsvSummary {
    rows: usize,
    route_ids: BTreeSet<u64>,
    measured_gas: BTreeSet<u64>,
    undercount_route_ids: BTreeSet<u64>,
    statuses: BTreeSet<String>,
    blank_gross_profit_rows: usize,
    blank_l2_base_fee_rows: usize,
    blank_l1_fee_rows: usize,
    blank_total_gas_rows: usize,
    blank_net_rows: usize,
}

fn dodo_gas_csv_summary() -> DodoGasCsvSummary {
    let raw = read_required(DODO_GAS_NET_CSV);
    let mut summary = DodoGasCsvSummary::default();

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() || idx == 0 {
            continue;
        }
        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(fields.len(), 15, "line {line_no}: malformed DODO gas row");

        let route_id = parse_u64_field(fields[0], "route_id", line_no);
        let measured_gas_used = parse_u64_field(fields[4], "measured_gas_used", line_no);
        let detector_gap = parse_u64_field(fields[10], "detector_gas_gap_units", line_no);

        summary.rows += 1;
        summary.route_ids.insert(route_id);
        summary.measured_gas.insert(measured_gas_used);
        if detector_gap > 0 {
            summary.undercount_route_ids.insert(route_id);
        }
        summary.statuses.insert(fields[13].to_string());
        if fields[3].is_empty() {
            summary.blank_gross_profit_rows += 1;
        }
        if fields[5].is_empty() {
            summary.blank_l2_base_fee_rows += 1;
        }
        if fields[6].is_empty() {
            summary.blank_l1_fee_rows += 1;
        }
        if fields[7].is_empty() {
            summary.blank_total_gas_rows += 1;
        }
        if fields[8].is_empty() {
            summary.blank_net_rows += 1;
        }
    }

    summary
}

#[test]
fn gas_net_certification_requirements_match_current_artifacts_and_arbitrum_basis() {
    let requirements = read_json(GAS_NET_REQUIREMENTS);
    let docs = &requirements["documentation_basis"];
    for required in [
        "https://docs.arbitrum.io/how-arbitrum-works/deep-dives/gas-and-fees",
        "child-chain basefee",
        "parent-chain calldata charge",
        "eth_estimateGas",
        "NodeInterface.gasEstimateComponents()",
        "NodeInterface.gasEstimateL1Component()",
        "gasUsedForL1",
        "Brotli-zero",
    ] {
        assert!(
            docs.to_string().contains(required),
            "gas/net requirements missing Arbitrum documentation basis fragment: {required}"
        );
    }

    let required_fields = json_string_set(
        &requirements["green_candidate_requirements"],
        "must_have_non_null_fields",
    );
    for field in [
        "gross_profit_wei",
        "estimated_gas_wei",
        "net_profit_wei",
        "measured_execution_gas_used",
        "l2_gas_price_wei",
        "l1_fee_wei",
        "total_gas_wei",
        "raw_calldata_bytes_or_l1_data_units",
        "gas_limit",
        "calldata",
    ] {
        assert!(
            required_fields.contains(field),
            "gas/net requirements missing required candidate field {field}"
        );
    }

    let counts = gate9_economics_counts();
    let gate9 = &requirements["current_gate9_evidence"];
    assert_eq!(gate9["candidate_rows"].as_u64(), Some(counts.rows as u64));
    assert_eq!(
        gate9["success_rows"].as_u64(),
        Some(counts.success_rows as u64)
    );
    assert_eq!(gate9["loss_rows"].as_u64(), Some(counts.loss_rows as u64));
    assert_eq!(
        gate9["gross_profit_null_rows"].as_u64(),
        Some(counts.gross_profit_null_rows as u64)
    );
    assert_eq!(
        gate9["estimated_gas_wei_null_rows"].as_u64(),
        Some(counts.estimated_gas_null_rows as u64)
    );
    assert_eq!(
        gate9["net_profit_wei_null_rows"].as_u64(),
        Some(counts.net_profit_null_rows as u64)
    );
    assert_eq!(gate9["status"].as_str(), Some("not_gas_net_certified"));

    let dodo = &requirements["current_dodo_pmm_evidence"];
    let summary = dodo_gas_csv_summary();
    assert_eq!(dodo["rows"].as_u64(), Some(summary.rows as u64));
    assert_eq!(json_u64_set(dodo, "route_ids"), summary.route_ids);
    assert_eq!(
        json_u64_set(dodo, "measured_gas_used"),
        summary.measured_gas
    );
    assert_eq!(
        dodo["detector_gas_units"].as_u64(),
        Some(DODO_DETECTOR_GAS_UNITS)
    );
    assert_eq!(
        dodo["submit_floor_gas_units"].as_u64(),
        Some(DODO_SUBMIT_3HOP_FLOOR_GAS_UNITS)
    );
    assert_eq!(
        dodo["wire_gas_limit"].as_u64(),
        Some(DODO_SUBMIT_3HOP_WIRE_GAS_LIMIT)
    );
    assert_eq!(
        dodo["max_measured_gas_used"].as_u64(),
        Some(*MEASURED_DODO_GAS_USED.iter().max().unwrap())
    );
    assert_eq!(dodo["detector_gas_gap_units_max"].as_u64(), Some(0));
    assert_eq!(
        json_u64_set(dodo, "undercount_route_ids"),
        summary.undercount_route_ids
    );
    assert_eq!(
        summary.statuses,
        BTreeSet::from(["diagnostic_gas_only".to_string()])
    );
    assert_eq!(
        dodo["all_rows_status"].as_str(),
        Some("diagnostic_gas_only")
    );
    assert_eq!(summary.blank_gross_profit_rows, summary.rows);
    assert_eq!(summary.blank_l2_base_fee_rows, summary.rows);
    assert_eq!(summary.blank_l1_fee_rows, summary.rows);
    assert_eq!(summary.blank_total_gas_rows, summary.rows);
    assert_eq!(summary.blank_net_rows, summary.rows);

    let engine_gas = read_required(ENGINE_GAS_LIB);
    let model = &requirements["current_engine_gas_model_evidence"];
    for function_name in json_string_set(model, "required_functions") {
        assert!(
            engine_gas.contains(&format!("fn {function_name}")),
            "engine-gas source missing required function {function_name}"
        );
    }
    for field in json_string_set(model, "required_snapshot_fields") {
        assert!(
            engine_gas.contains(&format!("pub {field}:")),
            "engine-gas snapshot missing required field {field}"
        );
    }
    assert_eq!(
        requirements["certification_conclusion"].as_str(),
        Some("Current artifacts prove some routes execute and DODO gas has been measured, but they do not prove profitable net arbitrage after Arbitrum gas because required fee terms and net fields are missing.")
    );
}

#[test]
fn detector_dodo_gas_covers_measured_worst_case() {
    let pipeline = read_required(PIPELINE_RS);
    let gas_oracle = read_required(GAS_ORACLE_TEST);
    let pool_rs = read_required(POOL_RS);
    let gas_oracle_log = read_required(GAS_ORACLE_LOG);

    let detector_gas = first_u64_after_marker(
        &pipeline,
        "DODO/PMM 3-hop cycles measured",
        "let gas_units: u64 =",
    );
    assert_eq!(
        detector_gas, DODO_DETECTOR_GAS_UNITS,
        "DODO detector gas constant drifted"
    );

    assert!(
        pool_rs.contains("PoolKind::DodoV2 => 250_000"),
        "DODO submit per-hop gas estimate no longer has the expected 250k PMM floor"
    );
    assert!(
        pipeline.contains("const GAS_BASE: u64 = 350_000"),
        "submit route_gas_units base floor drifted"
    );
    assert!(
        pipeline.contains("let gas_limit = (gas_units.saturating_mul(3) / 2).max(1_300_000);"),
        "submit gas-limit sizing no longer applies 1.5x margin with 1.3M floor"
    );

    let measured_max = *MEASURED_DODO_GAS_USED.iter().max().unwrap();
    assert_eq!(measured_max, 933_529, "measured max gas drifted");
    assert!(
        detector_gas >= measured_max,
        "detector DODO gas ({detector_gas}) must cover the measured fork worst-case ({measured_max}); \
         a fast-lane candidate skips REVM so the detector EV gate must never under-state gas"
    );
    assert!(
        detector_gas <= DODO_SUBMIT_3HOP_FLOOR_GAS_UNITS,
        "detector DODO gas must not exceed the submit floor (would over-reject what submit accepts)"
    );
    assert!(
        measured_max <= DODO_SUBMIT_3HOP_FLOOR_GAS_UNITS,
        "submit min-profit gas floor no longer covers measured DODO gas"
    );
    assert!(
        measured_max < DODO_SUBMIT_3HOP_WIRE_GAS_LIMIT,
        "submit on-wire gas limit no longer covers measured DODO gas"
    );

    assert!(
        gas_oracle.contains("string.concat(\"route \", vm.toString(rid[i]), \" gasUsed\")"),
        "gas oracle should keep per-route gas logging enabled"
    );
    for route_gas in MEASURED_DODO_GAS_USED {
        assert!(route_gas > 0, "measured DODO gas samples must be nonzero");
    }
    assert!(
        gas_oracle.contains("assertLe(maxUsed, uint256(960_000)"),
        "gas oracle no longer asserts submit-floor coverage"
    );
    assert!(
        gas_oracle.contains("assertLt(maxUsed, uint256(WIRE_GAS_LIMIT_3HOP)"),
        "gas oracle no longer asserts on-wire gas-limit coverage"
    );

    assert!(
        !gas_oracle_log.contains("https://") && !gas_oracle_log.contains("arb-mainnet"),
        "gas oracle log should keep provider URLs redacted"
    );
    for required in [
        "[PASS] test_GasOracleVsAnvil()",
        "route 2665 gasUsed: 711810",
        "route 2967 gasUsed: 767097",
        "route 2664 gasUsed: 723622",
        "route 2668 gasUsed: 733820",
        "route 2972 gasUsed: 933529",
        "routes that ran through all legs: 5",
        "MAX gasUsed across DODO cycles: 933529",
        "engine DODO/PMM detect assumption (was 520k -> now): 960000",
        "engine 3-hop submit-floor gas_units (was 350k -> now): 960000",
        "on-wire gas_limit 3-hop (was 750k -> now): 1300000",
        "Suite result: ok. 1 passed; 0 failed",
    ] {
        assert!(
            gas_oracle_log.contains(required),
            "fresh gas oracle log missing required evidence: {required}"
        );
    }
}

#[test]
fn gate9_candidate_jsonl_does_not_persist_detector_or_submit_gas_terms() {
    let raw = read_required(GATE9_B_CANDIDATES);
    let mut row_count = 0usize;
    let mut estimated_gas_values = BTreeSet::new();
    let mut net_profit_values = BTreeSet::new();
    let mut dodo_route_ids = BTreeSet::new();

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        row_count += 1;
        let row = serde_json::from_str::<Value>(line)
            .unwrap_or_else(|err| panic!("candidate line {} invalid JSON: {err}", idx + 1));
        estimated_gas_values.insert(json_string_or_null(&row, "estimated_gas_wei"));
        net_profit_values.insert(json_string_or_null(&row, "net_profit_wei"));

        let route_id = row
            .get("route_id")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("candidate line {} missing route_id", idx + 1));
        if matches!(route_id, 3146 | 362 | 3188 | 268) {
            dodo_route_ids.insert(route_id);
        }
    }

    assert_eq!(row_count, 246, "Gate 9 candidate row count drifted");
    assert_eq!(
        estimated_gas_values,
        BTreeSet::from([None]),
        "Gate 9 candidate JSONL now persists estimated_gas_wei; update certification gate"
    );
    assert_eq!(
        net_profit_values,
        BTreeSet::from([None]),
        "Gate 9 candidate JSONL now persists net_profit_wei; update certification gate"
    );
    assert_eq!(
        dodo_route_ids,
        BTreeSet::from([268, 362, 3146, 3188]),
        "Gate 9 DODO-containing route coverage drifted"
    );
}

#[test]
fn dodo_pmm_gas_net_artifact_is_diagnostic_until_fee_terms_exist() {
    let raw = read_required(DODO_GAS_NET_CSV);
    let mut row_count = 0usize;
    let mut route_ids = BTreeSet::new();
    let mut measured_values = BTreeSet::new();
    let mut positive_after_measured_gas_rows = 0usize;
    let mut undercount_routes = BTreeSet::new();

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 {
            assert_eq!(
                line,
                "route_id,hop_count,input_amount,gross_profit_wei,measured_gas_used,l2_base_fee_wei,l1_fee_wei,total_gas_wei,net_after_measured_gas_wei,detector_gas_units,detector_gas_gap_units,submit_floor_gas_units,wire_gas_limit,status,blocker",
                "unexpected DODO gas/net CSV header"
            );
            continue;
        }

        let line_no = idx + 1;
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(fields.len(), 15, "line {line_no}: malformed DODO gas row");

        let route_id = parse_u64_field(fields[0], "route_id", line_no);
        let hop_count = parse_u64_field(fields[1], "hop_count", line_no);
        let input_amount = fields[2];
        let gross_profit_wei = fields[3];
        let measured_gas_used = parse_u64_field(fields[4], "measured_gas_used", line_no);
        let l2_base_fee_wei = fields[5];
        let l1_fee_wei = fields[6];
        let total_gas_wei = fields[7];
        let net_after_measured_gas_wei = fields[8];
        let detector_gas_units = parse_u64_field(fields[9], "detector_gas_units", line_no);
        let detector_gap = parse_u64_field(fields[10], "detector_gas_gap_units", line_no);
        let submit_floor = parse_u64_field(fields[11], "submit_floor_gas_units", line_no);
        let wire_gas_limit = parse_u64_field(fields[12], "wire_gas_limit", line_no);
        let status = fields[13];
        let blocker = fields[14];

        row_count += 1;
        assert!(
            route_ids.insert(route_id),
            "duplicate DODO route id {route_id}"
        );
        measured_values.insert(measured_gas_used);

        assert_eq!(
            hop_count, 3,
            "line {line_no}: DODO gas sample must be 3-hop"
        );
        assert_eq!(
            input_amount, DODO_GAS_ORACLE_AMOUNT_IN,
            "line {line_no}: gas oracle amount drifted"
        );
        assert!(
            measured_gas_used > 0,
            "line {line_no}: measured gas missing"
        );
        assert_eq!(
            detector_gas_units, DODO_DETECTOR_GAS_UNITS,
            "line {line_no}: detector gas constant drifted"
        );
        assert_eq!(
            detector_gap,
            measured_gas_used.saturating_sub(detector_gas_units),
            "line {line_no}: detector gas gap is wrong"
        );
        if detector_gap > 0 {
            undercount_routes.insert(route_id);
        }
        assert_eq!(
            submit_floor, DODO_SUBMIT_3HOP_FLOOR_GAS_UNITS,
            "line {line_no}: submit floor drifted"
        );
        assert_eq!(
            wire_gas_limit, DODO_SUBMIT_3HOP_WIRE_GAS_LIMIT,
            "line {line_no}: wire gas limit drifted"
        );
        assert!(
            measured_gas_used <= submit_floor,
            "line {line_no}: submit floor no longer covers measured gas"
        );
        assert!(
            measured_gas_used < wire_gas_limit,
            "line {line_no}: wire gas limit no longer covers measured gas"
        );

        match status {
            "diagnostic_gas_only" => {
                assert!(
                    gross_profit_wei.is_empty(),
                    "line {line_no}: diagnostic row must not invent gross profit"
                );
                assert!(
                    l2_base_fee_wei.is_empty(),
                    "line {line_no}: diagnostic row must not invent L2 base fee"
                );
                assert!(
                    l1_fee_wei.is_empty(),
                    "line {line_no}: diagnostic row must not invent L1 fee"
                );
                assert!(
                    total_gas_wei.is_empty(),
                    "line {line_no}: diagnostic row must not invent total gas wei"
                );
                assert!(
                    net_after_measured_gas_wei.is_empty(),
                    "line {line_no}: diagnostic row must not invent net profit"
                );
                assert!(
                    blocker.contains("missing_gross_profit") && blocker.contains("arbitrum_fee_terms"),
                    "line {line_no}: diagnostic blocker must name missing gross profit and fee terms"
                );
            }
            "positive_after_measured_gas" => {
                positive_after_measured_gas_rows += 1;
                assert!(
                    !net_after_measured_gas_wei.starts_with('-')
                        && !net_after_measured_gas_wei.is_empty()
                        && net_after_measured_gas_wei != "0",
                    "line {line_no}: positive row must carry positive net"
                );
            }
            "negative_after_measured_gas" | "diagnostic_only" => {}
            other => panic!("line {line_no}: unsupported DODO gas status {other}"),
        }
    }

    assert_eq!(row_count, 5, "DODO gas artifact row count drifted");
    assert_eq!(
        route_ids,
        MEASURED_DODO_ROUTE_IDS.into_iter().collect::<BTreeSet<_>>(),
        "DODO measured route set drifted"
    );
    assert_eq!(
        measured_values,
        MEASURED_DODO_GAS_USED.into_iter().collect::<BTreeSet<_>>(),
        "DODO measured gas set drifted"
    );
    assert_eq!(
        undercount_routes,
        BTreeSet::new(),
        "detector gas now covers every measured DODO route (no undercount)"
    );
    assert_eq!(
        positive_after_measured_gas_rows, 0,
        "current DODO artifact must not claim positive net until fee/profit terms exist"
    );
}
