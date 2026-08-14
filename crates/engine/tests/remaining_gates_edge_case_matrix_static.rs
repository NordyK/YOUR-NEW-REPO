use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde_json::Value;

const MATRIX: &str = "../../../Codex/REMAINING_GATES_EDGE_CASE_CLOSURE_MATRIX_2026-06-20.json";

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn matrix() -> Value {
    serde_json::from_str(&read(MATRIX)).unwrap_or_else(|err| panic!("{MATRIX} invalid JSON: {err}"))
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing key {key} in {value}"))
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    field(value, key)
        .as_str()
        .unwrap_or_else(|| panic!("missing string field {key} in {value}"))
}

fn bool_field(value: &Value, key: &str) -> bool {
    field(value, key)
        .as_bool()
        .unwrap_or_else(|| panic!("missing bool field {key} in {value}"))
}

fn array_field<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
    field(value, key)
        .as_array()
        .unwrap_or_else(|| panic!("missing array field {key} in {value}"))
}

fn all_text(value: &Value) -> String {
    match value {
        Value::String(raw) => raw.clone(),
        Value::Array(items) => items.iter().map(all_text).collect::<Vec<_>>().join("\n"),
        Value::Object(map) => map.values().map(all_text).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

fn gate<'a>(matrix: &'a Value, id: &str) -> &'a Value {
    array_field(matrix, "gates")
        .iter()
        .find(|gate| str_field(gate, "id") == id)
        .unwrap_or_else(|| panic!("missing gate {id}"))
}

#[test]
fn matrix_preserves_hard_no_capital_boundaries_and_research_basis() {
    let matrix = matrix();
    assert_eq!(
        str_field(&matrix, "schema"),
        "remaining_gates_edge_case_closure_matrix.v1"
    );

    let boundaries = field(&matrix, "hard_boundaries");
    assert!(!bool_field(
        boundaries,
        "production_logic_modified_by_this_artifact"
    ));
    assert!(!bool_field(boundaries, "live_capital_allowed"));
    assert!(bool_field(
        boundaries,
        "aws_no_capital_dry_run_allowed_after_review"
    ));
    assert!(bool_field(boundaries, "dune_rows_are_discovery_only"));
    assert!(bool_field(boundaries, "loss_rows_are_never_money_proof"));
    assert!(bool_field(
        boundaries,
        "parent_block_fork_is_not_exact_prestate_proof"
    ));
    assert!(bool_field(
        boundaries,
        "real_aws_artifact_contract_required"
    ));

    let sources = array_field(&matrix, "research_basis")
        .iter()
        .map(|row| str_field(row, "id").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "foundry_rollfork_tx_hash",
        "foundry_transact_tx_hash",
        "dune_dex_trades_schema",
        "dune_partition_pruning",
        "arbitrum_nodeinterface",
        "aws_spot_interruption",
    ] {
        assert!(
            sources.contains(required),
            "missing research basis {required}"
        );
    }

    let text = all_text(&matrix);
    for required in [
        "partition_columns=[blockchain, project, block_month]",
        "https://docs.arbitrum.io/for-devs/dev-tools-and-resources/partials/precompile-tables/_NodeInterface",
        "gasEstimateComponents",
        "gasEstimateL1Component",
    ] {
        assert!(
            text.contains(required),
            "matrix missing researched boundary detail {required}"
        );
    }
}

#[test]
fn every_objective_gate_has_edge_cases_proof_and_no_go_conditions() {
    let matrix = matrix();
    let gates = array_field(&matrix, "gates");
    let ids = gates
        .iter()
        .map(|gate| str_field(gate, "id").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "detector_profitability_across_families",
        "loss_as_pass_exclusion",
        "live_relay_feed_freshness_raw_capture",
        "revm_live_fork_equivalence",
        "submit_state_gas_slippage_sizing",
        "venue_parity_and_fastlane_promotion",
        "aws_deploy_artifact_readiness",
    ] {
        assert!(ids.contains(required), "missing gate {required}");
    }

    for gate in gates {
        assert!(
            !str_field(gate, "current_status").trim().is_empty(),
            "{} missing current_status",
            str_field(gate, "id")
        );
        assert!(
            !str_field(gate, "blocking_class").trim().is_empty(),
            "{} missing blocking_class",
            str_field(gate, "id")
        );
        assert!(
            array_field(gate, "current_green_evidence").len() >= 2,
            "{} must list current green evidence",
            str_field(gate, "id")
        );
        assert!(
            array_field(gate, "current_red_evidence").len() >= 2,
            "{} must list current red evidence",
            str_field(gate, "id")
        );
        assert!(
            array_field(gate, "edge_cases_that_must_be_covered").len() >= 5,
            "{} must list edge cases",
            str_field(gate, "id")
        );
        assert!(
            array_field(gate, "proof_to_close").len() >= 3,
            "{} must list proof-to-close criteria",
            str_field(gate, "id")
        );
        assert!(
            array_field(gate, "no_go_if").len() >= 3,
            "{} must list no-go conditions",
            str_field(gate, "id")
        );
    }
}

#[test]
fn matrix_has_exact_remaining_gate_status_distribution_and_logic_issue_scope() {
    let matrix = matrix();
    let gates = array_field(&matrix, "gates");
    assert_eq!(
        gates.len(),
        7,
        "remaining-gates matrix must not silently gain or lose objective gates"
    );

    let mut status_counts = BTreeMap::<String, usize>::new();
    let mut production_logic_issue_ids = BTreeSet::<String>::new();
    for gate in gates {
        *status_counts
            .entry(str_field(gate, "current_status").to_string())
            .or_default() += 1;
        if bool_field(gate, "production_logic_issue") {
            production_logic_issue_ids.insert(str_field(gate, "id").to_string());
        }
    }

    assert_eq!(
        status_counts.get("green_guardrail_not_release_profit"),
        Some(&1),
        "only the Loss-as-pass accounting guardrail is green"
    );
    assert_eq!(
        status_counts.get("red_release_money_path_not_standalone_clean"),
        Some(&1),
        "detector profitability must remain release-red until standalone money proof closes"
    );
    assert_eq!(
        status_counts.get("red_aws_live_measurement_required"),
        Some(&1),
        "feed freshness must remain AWS-live-measurement red"
    );
    assert_eq!(
        status_counts.get("red_if_kyber_or_balancer_weighted_enabled_or_promoted"),
        Some(&1),
        "venue parity must stay conditionally red while unsafe over-quote venues are contained"
    );
    assert_eq!(
        status_counts
            .keys()
            .filter(|status| status.starts_with("yellow_"))
            .count(),
        3,
        "exactly three gates should be yellow/partial in the current closure matrix"
    );
    assert_eq!(
        production_logic_issue_ids,
        BTreeSet::from(["venue_parity_and_fastlane_promotion".to_string()]),
        "contained venue parity is the only remaining gate classified as production logic risk"
    );

    let decision = field(&matrix, "release_decision");
    assert_eq!(
        str_field(decision, "aws_no_capital_dry_run"),
        "go_after_commit_and_operator_review"
    );
    assert_eq!(str_field(decision, "aws_live_capital"), "no_go");
    assert!(
        str_field(decision, "why").contains("aws_live_capital remains no_go"),
        "release decision must not soften the live-capital no-go verdict"
    );
}

#[test]
fn detector_profitability_gate_pins_current_release_failure_counts() {
    let matrix = matrix();
    let gate = gate(&matrix, "detector_profitability_across_families");
    assert!(!bool_field(gate, "production_logic_issue"));
    let text = all_text(gate);
    for required in [
        "money_path_release_certification_static",
        "total=29, direct_positive=12, losses=11, metadata_limited_other_reverts=6",
        "zero metadata_limited_other_reverts",
        "11 Loss rows are state-point-sensitive",
        "V4/Curve/Fluid/Balancer/LB/Maverick/DODO",
        "Same-venue, cross-venue, 2-hop, 3-hop, 4-hop, and over-4-hop",
        "net-profit",
    ] {
        assert!(
            text.contains(required),
            "detector profitability gate missing {required}"
        );
    }
}

#[test]
fn loss_as_pass_gate_forbids_mechanical_tests_as_money_proof() {
    let matrix = matrix();
    let gate = gate(&matrix, "loss_as_pass_exclusion");
    let text = all_text(gate);
    for required in [
        "green_guardrail_not_release_profit",
        "loss_as_pass_static",
        "foundry_profit_proof_classification_static",
        "A test that accepts Loss selector can only prove executor/route mechanics",
        "gross-only success is treated as capital-ready",
        "Any new fork test that accepts Loss is added to the not-money-proof inventory",
    ] {
        assert!(
            text.contains(required),
            "loss-as-pass gate missing {required}"
        );
    }
}

#[test]
fn feed_freshness_gate_requires_joined_aws_artifact_and_stale_rejection() {
    let matrix = matrix();
    let gate = gate(&matrix, "live_relay_feed_freshness_raw_capture");
    let text = all_text(gate);
    for required in [
        "red_aws_live_measurement_required",
        "raw_feed.jsonl",
        "raw_feed.jsonl.freshness.jsonl",
        "artifact_manifest.json",
        "feed_candidate_freshness_report.json",
        "matching recv_ns/capture_recv_ns",
        "accepted candidate trigger_seq or trigger_recv_ns mismatches feed anchor",
        "accepted candidate paper-mode record mismatches timestamp, block, gross profit, or net profit",
        "same route_id, timestamp, block, gross profit, and net profit",
        "p95_event_age_ms",
        "p99_event_age_ms",
        "max_event_age_ms",
        "accepted_stale_candidate_count",
        "accepted_stale_candidate_count > 0",
        "frames=0 messages=0 samples=0",
        "aws_dryrun_artifact_contract",
    ] {
        assert!(
            text.contains(required),
            "feed freshness gate missing {required}"
        );
    }
}

#[test]
fn revm_live_equivalence_gate_rejects_parent_block_and_singleton_overclaims() {
    let matrix = matrix();
    let gate = gate(&matrix, "revm_live_fork_equivalence");
    let text = all_text(gate);
    for required in [
        "gate9_statepoint_classification_static",
        "0 real-loss/kernel-executor-bug rows",
        "rollFork(txHash)",
        "transact(txHash)",
        "Parent-block forks are rejected",
        "Singleton venues mutated earlier in the block",
        "Gross-vs-net comparisons",
        "AWS live-tip dry-run",
    ] {
        assert!(text.contains(required), "REVM/live gate missing {required}");
    }
}

#[test]
fn economics_venue_and_deploy_gates_pin_capital_safety_edges() {
    let matrix = matrix();
    let text = all_text(&matrix);
    for required in [
        "offchain_net_gate_current_static",
        "net_negative_after_gas",
        "NodeInterface gas samples",
        "NodeInterface gas matrix l1_fee_wei or l2_gas_price_wei mismatches candidate submit_calldata_bytes",
        "matches candidate l1_fee_wei/l2_gas_price_wei",
        "submit_min_profit_wei",
        "submit_calldata_bytes",
        "Gross-only capital mode is impossible",
        "KYBER_ENABLE is set while Kyber parity is red",
        "BalancerWeighted is fast-lane promoted",
        "unsafe_overquotes > 0",
        "env.redacted",
        "binary.sha256",
        "run_metadata command_line missing no-capital flags or including SUBMIT_TX=true",
        "artifact_manifest.json missing, malformed, or hash mismatch",
        "SUBMIT_TX=true",
        "funded private key present",
        "No executor deployment or funded key is required for no-capital dry-run",
        "go_after_commit_and_operator_review",
        "aws_live_capital",
        "no_go",
    ] {
        assert!(
            text.contains(required),
            "matrix missing capital/deploy safety edge: {required}"
        );
    }
}
