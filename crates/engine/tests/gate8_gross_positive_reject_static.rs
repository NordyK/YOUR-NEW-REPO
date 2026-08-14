use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const ANALYSIS: &str = "../../../Codex/GATE8_GROSS_POSITIVE_REJECT_ANALYSIS_2026-06-14.json";
const ROUTE_CLASSIFICATION: &str =
    "../../../Codex/GATE8_RAW_GAS_SURPLUS_REJECT_ROUTE_CLASSIFICATION_2026-06-14.json";
const LOG: &str = "../../../engine/certification_runs/20260614_221000/gate8/engine_live_stdout.log";
const CANDIDATES: &str =
    "../../../engine/certification_runs/20260614_221000/gate8/candidate_records.jsonl";

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn read_json(path: &str) -> Value {
    let raw = read(path);
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("{path} invalid JSON: {err}"))
}

fn field_f64(line: &str, field: &str) -> Option<f64> {
    let start = line.find(field)? + field.len();
    let rest = &line[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    rest[..end].trim_matches('"').parse().ok()
}

fn field_u64(line: &str, field: &str) -> Option<u64> {
    let start = line.find(field)? + field.len();
    let rest = &line[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    rest[..end].trim_matches('"').parse().ok()
}

fn kind(line: &str) -> Option<&'static str> {
    if line.contains("kind=\"n_hop_near_miss\"") {
        Some("n_hop_near_miss")
    } else if line.contains("kind=\"triangle_near_miss\"") {
        Some("triangle_near_miss")
    } else {
        None
    }
}

fn assert_close(actual: f64, expected: f64, label: &str) {
    let tolerance = 1e-15_f64;
    assert!(
        (actual - expected).abs() <= tolerance,
        "{label}: actual {actual} expected {expected}"
    );
}

#[test]
fn gate8_gross_positive_reject_analysis_matches_current_log_and_source() {
    let analysis = read_json(ANALYSIS);
    let log = read(LOG);

    let mut total = 0usize;
    let mut shortfall_negative = 0usize;
    let mut shortfall_nonnegative = 0usize;
    let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut negative_by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut negative_routes = BTreeSet::new();
    let mut raw_surpluses = Vec::new();

    for line in log.lines() {
        if !line.contains("GROSS_POSITIVE_BUT_GAS_NEGATIVE") {
            continue;
        }
        total += 1;
        let k = kind(line).unwrap_or_else(|| panic!("unclassified diagnostic line: {line}"));
        *by_kind.entry(k).or_default() += 1;
        let shortfall = field_f64(line, "shortfall_eth=")
            .unwrap_or_else(|| panic!("missing shortfall_eth in line: {line}"));
        if shortfall < 0.0 {
            shortfall_negative += 1;
            *negative_by_kind.entry(k).or_default() += 1;
            raw_surpluses.push(-shortfall);
            negative_routes.insert(
                field_u64(line, "route_id=")
                    .unwrap_or_else(|| panic!("missing route_id in line: {line}")),
            );
        } else {
            shortfall_nonnegative += 1;
        }
    }

    raw_surpluses.sort_by(|a, b| a.partial_cmp(b).expect("surplus values must be finite"));

    assert_eq!(
        total,
        analysis["summary"]["gross_positive_but_gas_negative_total"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        shortfall_negative,
        analysis["summary"]["shortfall_negative_total"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        shortfall_nonnegative,
        analysis["summary"]["shortfall_nonnegative_total"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        by_kind["n_hop_near_miss"],
        analysis["summary"]["by_kind"]["n_hop_near_miss"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        by_kind["triangle_near_miss"],
        analysis["summary"]["by_kind"]["triangle_near_miss"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        negative_by_kind["n_hop_near_miss"],
        analysis["summary"]["shortfall_negative_by_kind"]["n_hop_near_miss"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        negative_by_kind["triangle_near_miss"],
        analysis["summary"]["shortfall_negative_by_kind"]["triangle_near_miss"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        negative_routes.len(),
        analysis["summary"]["unique_shortfall_negative_route_ids"]
            .as_u64()
            .unwrap() as usize
    );

    let max_surplus = *raw_surpluses
        .last()
        .expect("expected negative shortfall rows");
    let overhead_plus_buffer = analysis["effective_thresholds_from_log"]
        ["executor_overhead_plus_safety_buffer_eth"]
        .as_f64()
        .unwrap();
    assert!(
        max_surplus < overhead_plus_buffer,
        "max raw-gas surplus {max_surplus} should be below configured overhead+buffer {overhead_plus_buffer}"
    );

    let pipeline = read("src/pipeline.rs");
    assert!(
        pipeline.contains("let Some(net) = canonical_net_profit(terms) else"),
        "diagnostic must stay in the canonical_net_profit reject branch"
    );
    assert!(
        pipeline.contains("\"GROSS_POSITIVE_BUT_GAS_NEGATIVE (diagnostic)\""),
        "source no longer emits the analyzed diagnostic"
    );
    assert!(
        pipeline.contains("executor_overhead_wei: extra_cost_terms_wei().0"),
        "source must include executor overhead in canonical profit terms"
    );
    assert!(
        pipeline.contains("safety_buffer_wei: extra_cost_terms_wei().1"),
        "source must include safety buffer in canonical profit terms"
    );
}

#[test]
fn gate8_raw_gas_surplus_route_classification_matches_log_candidates_and_live_blockers() {
    let artifact = read_json(ROUTE_CLASSIFICATION);
    let log = read(LOG);
    let overhead_plus_buffer = artifact["effective_thresholds_from_log"]
        ["executor_overhead_plus_safety_buffer_eth"]
        .as_f64()
        .expect("executor_overhead_plus_safety_buffer_eth must be number");

    let mut diagnostic_rows = 0usize;
    let mut below_raw_gas_rows = 0usize;
    let mut raw_gas_surplus_rows = 0usize;
    let mut by_kind_total = BTreeMap::<String, usize>::new();
    let mut by_kind_raw_gas_surplus = BTreeMap::<String, usize>::new();
    let mut route_rows = BTreeMap::<u64, Vec<(usize, String, u64, f64, f64, f64, f64)>>::new();
    let mut freshness_counts = BTreeMap::<String, usize>::new();
    let mut ring_overflow_events = 0usize;
    let mut ring_overflow_max = 0u64;

    for (idx, line) in log.lines().enumerate() {
        if line.contains("FRESHNESS_AUDIT") && line.contains("verdict=") {
            let key = if line.contains("STALE") {
                "stale"
            } else if line.contains("FRESH") {
                "fresh"
            } else {
                "other"
            };
            *freshness_counts.entry(key.to_string()).or_default() += 1;
        }

        if line.contains("sim→REVM candidate ring overflow") {
            ring_overflow_events += 1;
            let dropped_total = field_u64(line, "dropped_total=")
                .unwrap_or_else(|| panic!("missing dropped_total in line: {line}"));
            ring_overflow_max = ring_overflow_max.max(dropped_total);
        }

        if !line.contains("GROSS_POSITIVE_BUT_GAS_NEGATIVE") {
            continue;
        }

        diagnostic_rows += 1;
        let line_no = idx + 1;
        let route_id = field_u64(line, "route_id=")
            .unwrap_or_else(|| panic!("missing route_id in line: {line}"));
        let k = kind(line)
            .unwrap_or_else(|| panic!("unclassified diagnostic line: {line}"))
            .to_string();
        let hop_count = field_u64(line, "hops=")
            .or_else(|| field_u64(line, "heavy_hop_count="))
            .unwrap_or_else(|| panic!("missing hop count in line: {line}"));
        let amount_in_eth = field_f64(line, "amount_in_eth=")
            .unwrap_or_else(|| panic!("missing amount_in_eth in line: {line}"));
        let gross_profit_eth = field_f64(line, "gross_profit_eth=")
            .unwrap_or_else(|| panic!("missing gross_profit_eth in line: {line}"));
        let raw_l1_l2_gas_eth = field_f64(line, "gas_total_eth=")
            .unwrap_or_else(|| panic!("missing gas_total_eth in line: {line}"));
        let shortfall_eth = field_f64(line, "shortfall_eth=")
            .unwrap_or_else(|| panic!("missing shortfall_eth in line: {line}"));
        *by_kind_total.entry(k.clone()).or_default() += 1;

        if shortfall_eth < 0.0 {
            let raw_gas_surplus_eth = -shortfall_eth;
            raw_gas_surplus_rows += 1;
            *by_kind_raw_gas_surplus.entry(k.clone()).or_default() += 1;
            assert!(
                raw_gas_surplus_eth < overhead_plus_buffer,
                "route {route_id} line {line_no} raw gas surplus {raw_gas_surplus_eth} unexpectedly cleared overhead+buffer {overhead_plus_buffer}"
            );
            route_rows.entry(route_id).or_default().push((
                line_no,
                k,
                hop_count,
                raw_gas_surplus_eth,
                amount_in_eth,
                gross_profit_eth,
                raw_l1_l2_gas_eth,
            ));
        } else {
            below_raw_gas_rows += 1;
        }
    }

    let summary = &artifact["summary"];
    assert_eq!(
        diagnostic_rows,
        summary["diagnostic_rows_total"].as_u64().unwrap() as usize
    );
    assert_eq!(
        below_raw_gas_rows,
        summary["below_raw_l1_l2_gas_rows"].as_u64().unwrap() as usize
    );
    assert_eq!(
        raw_gas_surplus_rows,
        summary["raw_gas_surplus_rows"].as_u64().unwrap() as usize
    );
    assert_eq!(
        route_rows.len(),
        summary["unique_raw_gas_surplus_route_ids"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        raw_gas_surplus_rows,
        summary["raw_gas_surplus_rows_below_overhead_plus_safety_buffer"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        summary["raw_gas_surplus_rows_at_or_above_overhead_plus_safety_buffer"]
            .as_u64()
            .unwrap(),
        0
    );
    assert_eq!(
        ring_overflow_events,
        summary["revm_ring_overflow_events"].as_u64().unwrap() as usize
    );
    assert_eq!(
        ring_overflow_max,
        summary["revm_ring_overflow_max_dropped_total"]
            .as_u64()
            .unwrap()
    );

    for (key, actual) in by_kind_total {
        assert_eq!(
            actual,
            summary["by_kind_total"][&key].as_u64().unwrap() as usize,
            "by_kind_total drift for {key}"
        );
    }
    for (key, actual) in by_kind_raw_gas_surplus {
        assert_eq!(
            actual,
            summary["by_kind_raw_gas_surplus"][&key].as_u64().unwrap() as usize,
            "by_kind_raw_gas_surplus drift for {key}"
        );
    }
    for (key, actual) in freshness_counts {
        assert_eq!(
            actual,
            summary["freshness_audit_counts"][&key].as_u64().unwrap() as usize,
            "freshness count drift for {key}"
        );
    }

    let mut candidate_results = BTreeMap::<u64, BTreeMap<String, usize>>::new();
    for (idx, line) in read(CANDIDATES)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let row: Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("candidate row {} invalid JSON: {err}", idx + 1));
        let route_id = row["route_id"].as_u64().expect("candidate route_id");
        let result = row["revm_result"].as_str().unwrap_or("null").to_string();
        *candidate_results
            .entry(route_id)
            .or_default()
            .entry(result)
            .or_default() += 1;
    }

    let route_summaries = artifact["route_summaries"]
        .as_array()
        .expect("route_summaries must be array");
    assert_eq!(route_summaries.len(), route_rows.len());

    let mut overlap = Vec::new();
    for route_summary in route_summaries {
        let route_id = route_summary["route_id"].as_u64().unwrap();
        let rows = route_rows
            .get(&route_id)
            .unwrap_or_else(|| panic!("route summary {route_id} not present in log"));
        assert_eq!(
            route_summary["raw_gas_surplus_rows"].as_u64().unwrap() as usize,
            rows.len(),
            "route {route_id} raw gas surplus count drifted"
        );
        assert_eq!(
            route_summary["classification"].as_str().unwrap(),
            "rejected_by_configured_executor_overhead_plus_safety_buffer_before_flash_fee_min_net_ev_revm"
        );

        let max_surplus = rows
            .iter()
            .map(|row| row.3)
            .max_by(|left, right| left.partial_cmp(right).unwrap())
            .expect("route must have raw-surplus rows");
        let recorded_best_line = route_summary["best_line"].as_u64().unwrap() as usize;
        let best = rows
            .iter()
            .find(|row| row.0 == recorded_best_line)
            .unwrap_or_else(|| panic!("route {route_id} recorded best line is absent from log"));
        assert_eq!(
            recorded_best_line, best.0,
            "route {route_id} best line drifted"
        );
        assert_close(
            route_summary["max_raw_gas_surplus_eth"].as_f64().unwrap(),
            max_surplus,
            "max_raw_gas_surplus_eth",
        );
        assert_close(
            best.3,
            max_surplus,
            "recorded best line must have max_raw_gas_surplus_eth",
        );
        assert_close(
            route_summary["min_gap_to_executor_overhead_plus_safety_buffer_eth"]
                .as_f64()
                .unwrap(),
            overhead_plus_buffer - max_surplus,
            "gap_to_overhead_plus_buffer",
        );
        assert_close(
            route_summary["best_amount_in_eth"].as_f64().unwrap(),
            best.4,
            "best_amount_in_eth",
        );
        assert_close(
            route_summary["best_gross_profit_eth"].as_f64().unwrap(),
            best.5,
            "best_gross_profit_eth",
        );
        assert_close(
            route_summary["best_raw_l1_l2_gas_eth"].as_f64().unwrap(),
            best.6,
            "best_raw_l1_l2_gas_eth",
        );

        let recorded_candidate_results = route_summary["candidate_record_results_for_same_route"]
            .as_object()
            .expect("candidate_record_results_for_same_route must be object")
            .iter()
            .map(|(key, value)| (key.clone(), value.as_u64().unwrap() as usize))
            .collect::<BTreeMap<_, _>>();
        let actual_candidate_results = candidate_results
            .get(&route_id)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            recorded_candidate_results, actual_candidate_results,
            "candidate overlap drift for route {route_id}"
        );
        if !recorded_candidate_results.is_empty() {
            overlap.push(route_id);
        }
    }

    let recorded_overlap = summary["candidate_route_overlap_ids"]
        .as_array()
        .expect("candidate_route_overlap_ids must be array")
        .iter()
        .map(|value| value.as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(recorded_overlap, overlap);
}

#[test]
fn gate8_candidate_records_are_revert_or_loss_only_and_lack_realized_profit_fields() {
    let analysis = read_json(ANALYSIS);
    let raw = read(CANDIDATES);
    let mut rows = 0usize;
    let mut loss = 0usize;
    let mut revert = 0usize;
    let mut routes = BTreeSet::new();
    let mut rows_with_net = 0usize;
    let mut rows_with_estimated_gas = 0usize;
    let mut rows_with_balance_delta = 0usize;
    let mut rows_with_block = 0usize;

    for (idx, line) in raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        rows += 1;
        let row: Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("candidate row {} invalid JSON: {err}", idx + 1));
        routes.insert(
            row["route_id"]
                .as_u64()
                .expect("candidate row missing route_id"),
        );
        match row["revm_result"].as_str() {
            Some("loss") => loss += 1,
            Some("revert") => revert += 1,
            other => panic!("unexpected Gate8 candidate revm_result {other:?}"),
        }
        rows_with_net += usize::from(!row["net_profit_wei"].is_null());
        rows_with_estimated_gas += usize::from(!row["estimated_gas_wei"].is_null());
        rows_with_balance_delta += usize::from(!row["balance_delta_wei"].is_null());
        rows_with_block += usize::from(!row["block"].is_null());
    }

    assert_eq!(
        rows,
        analysis["candidate_records_summary"]["rows"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        routes.len(),
        analysis["candidate_records_summary"]["unique_route_ids"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        loss,
        analysis["candidate_records_summary"]["revm_result_loss"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        revert,
        analysis["candidate_records_summary"]["revm_result_revert"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(
        rows_with_net, 0,
        "Gate8 unexpectedly recorded net_profit_wei"
    );
    assert_eq!(
        rows_with_estimated_gas, 0,
        "Gate8 unexpectedly recorded estimated_gas_wei"
    );
    assert_eq!(
        rows_with_balance_delta, 0,
        "Gate8 unexpectedly recorded balance_delta_wei"
    );
    assert_eq!(rows_with_block, 0, "Gate8 unexpectedly recorded block");
}
