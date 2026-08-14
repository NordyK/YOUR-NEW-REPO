use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde_json::Value;

const PLAN: &str = "../../../Codex/REMAINING_GATES_DETAILED_EXECUTION_PLAN_2026-06-20.json";

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn plan() -> Value {
    serde_json::from_str(&read(PLAN)).unwrap_or_else(|err| panic!("{PLAN} invalid JSON: {err}"))
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

fn gate<'a>(plan: &'a Value, id: &str) -> &'a Value {
    array_field(plan, "gates")
        .iter()
        .find(|gate| str_field(gate, "id") == id)
        .unwrap_or_else(|| panic!("missing gate {id}"))
}

fn string_set(value: &Value, key: &str) -> BTreeSet<String> {
    array_field(value, key)
        .iter()
        .map(|item| item.as_str().expect("string array item").to_string())
        .collect()
}

#[test]
fn detailed_plan_preserves_scope_source_discipline_and_research_basis() {
    let plan = plan();
    assert_eq!(
        str_field(&plan, "schema"),
        "remaining_gates_detailed_execution_plan.v1"
    );
    assert!(bool_field(&plan, "plan_mode"));
    assert!(bool_field(&plan, "non_production_only"));
    assert!(!bool_field(
        &plan,
        "production_code_logic_modifications_allowed"
    ));
    assert_eq!(str_field(&plan, "live_capital_decision"), "no_go");
    assert_eq!(
        str_field(&plan, "aws_no_capital_dry_run_decision"),
        "go_after_commit_and_operator_review"
    );

    let discipline = string_set(&plan, "source_discipline");
    for required in [
        "current_worktree_and_external_docs_are_authority",
        "do_not_read_old_md_files",
        "do_not_rely_on_memory_as_proof",
        "dune_rows_are_discovery_only",
        "loss_rows_are_never_money_proof",
        "parent_block_fork_is_not_exact_prestate_proof",
        "tests_benchmarks_fixtures_reports_only_no_production_logic",
    ] {
        assert!(
            discipline.contains(required),
            "missing source discipline {required}"
        );
    }

    let research = array_field(&plan, "research_basis")
        .iter()
        .map(|row| str_field(row, "id").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "context7_foundry_rollfork_tx_hash",
        "context7_foundry_transact_tx_hash",
        "dune_dex_trades_schema",
        "dune_partition_pruning",
        "arbitrum_nodeinterface",
        "aws_spot_interruption",
        "sequential_thinking_scope_decomposition",
    ] {
        assert!(
            research.contains(required),
            "missing research basis {required}"
        );
    }

    let text = all_text(&plan);
    for required in [
        "partition_columns=[blockchain, project, block_month]",
        "gasEstimateComponents",
        "gasEstimateL1Component",
        "rollFork(txHash)",
        "transact(txHash)",
    ] {
        assert!(
            text.contains(required),
            "plan missing research detail {required}"
        );
    }

    for forbidden in ["TBD", "TODO", "FIXME", "fill in", "implement later"] {
        assert!(
            !text.contains(forbidden),
            "detailed plan must not contain placeholder {forbidden}"
        );
    }
}

#[test]
fn detailed_plan_has_exact_phase_and_gate_coverage() {
    let plan = plan();
    let phases = array_field(&plan, "execution_phases");
    let phase_ids = phases
        .iter()
        .map(|phase| str_field(phase, "id").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "phase_0_freeze_tree_and_research",
        "phase_1_local_wrong_test_and_money_guardrails",
        "phase_2_historical_exact_state_replay",
        "phase_3_aws_no_capital_live_measurement",
        "phase_4_release_decision_and_capital_preconditions",
    ] {
        assert!(phase_ids.contains(required), "missing phase {required}");
    }

    let gates = array_field(&plan, "gates");
    assert_eq!(gates.len(), 7, "plan must cover exactly 7 remaining gates");
    let gate_ids = gates
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
        assert!(gate_ids.contains(required), "missing gate {required}");
    }

    let mut production_logic_issue_ids = BTreeSet::<String>::new();
    let mut status_counts = BTreeMap::<String, usize>::new();
    for gate in gates {
        *status_counts
            .entry(str_field(gate, "status_now").to_string())
            .or_default() += 1;
        if bool_field(gate, "production_logic_issue") {
            production_logic_issue_ids.insert(str_field(gate, "id").to_string());
        }
        assert!(
            array_field(gate, "edge_cases").len() >= 6,
            "{} must enumerate deep edge cases",
            str_field(gate, "id")
        );
        assert!(
            array_field(gate, "local_verification_commands").len() >= 2,
            "{} must provide local verification commands",
            str_field(gate, "id")
        );
        assert!(
            array_field(gate, "close_conditions").len() >= 4,
            "{} must provide close conditions",
            str_field(gate, "id")
        );
        assert!(
            array_field(gate, "no_go_if").len() >= 3,
            "{} must provide no-go conditions",
            str_field(gate, "id")
        );
        assert!(
            !str_field(gate, "fix_strategy").trim().is_empty(),
            "{} must provide a fix strategy",
            str_field(gate, "id")
        );
    }

    assert_eq!(
        production_logic_issue_ids,
        BTreeSet::from(["venue_parity_and_fastlane_promotion".to_string()]),
        "only contained venue parity may be classified as production logic risk"
    );
    assert_eq!(
        status_counts.get("green_guardrail_not_release_profit"),
        Some(&1)
    );
    assert_eq!(
        status_counts.get("red_release_money_path_not_standalone_clean"),
        Some(&1)
    );
    assert_eq!(
        status_counts.get("red_aws_live_measurement_required"),
        Some(&1)
    );
    assert_eq!(
        status_counts.get("red_if_kyber_or_balancer_weighted_enabled_or_promoted"),
        Some(&1)
    );
}

#[test]
fn detector_and_loss_gates_forbid_false_money_proofs() {
    let plan = plan();
    let detector = all_text(gate(&plan, "detector_profitability_across_families"));
    for required in [
        "total=29, direct_positive=12, losses=11, metadata_limited_other_reverts=6",
        "2-hop",
        "3-hop",
        "4-hop",
        "over-4-hop",
        "same-venue",
        "cross-venue",
        "V4",
        "Curve",
        "Fluid",
        "Balancer",
        "LB",
        "Maverick",
        "DODO",
        "net-positive after gas",
        "zero metadata_limited_other_reverts",
    ] {
        assert!(
            detector.contains(required),
            "detector gate missing required proof boundary {required}"
        );
    }

    let loss = all_text(gate(&plan, "loss_as_pass_exclusion"));
    for required in [
        "Loss selector 0xb65b2e08",
        "mechanical route execution",
        "gross-only",
        "net-negative-after-gas",
        "foundry_profit_proof_classification_static",
        "loss_as_pass_static",
        "not release detector-money proof",
    ] {
        assert!(loss.contains(required), "loss gate missing {required}");
    }
}

#[test]
fn live_feed_aws_and_gas_gates_cover_local_latency_and_artifact_contract() {
    let plan = plan();
    let text = all_text(&plan);
    for required in [
        "local network latency can exceed 1s",
        "accepted_stale_candidate_count",
        "p95_event_age_ms",
        "p99_event_age_ms",
        "max_event_age_ms",
        "raw_feed.jsonl",
        "raw_feed.jsonl.freshness.jsonl",
        "artifact_manifest.json",
        "feed_candidate_freshness_report.json",
        "candidates.jsonl",
        "paper.jsonl",
        "nodeinterface_gas_matrix.json",
        "live_parity.json",
        "cargo test -p engine --test aws_dryrun_artifact_contract -- --nocapture",
        "NodeInterface",
        "gasEstimateComponents",
        "gasEstimateL1Component",
        "NodeInterface gas matrix l1_fee_wei mismatches candidate submit_calldata_bytes",
        "NodeInterface gas matrix l2_gas_price_wei mismatches candidate submit_calldata_bytes",
        "matches candidate l1_fee_wei/l2_gas_price_wei",
        "submit_calldata_bytes",
        "submit_min_profit_wei",
        "net_profit_wei <= 0",
        "SUBMIT_TX=false",
        "DRY_RUN=true",
        "REVM_VALIDATE=1",
        "command_line includes DRY_RUN=true, SUBMIT_TX=false, and REVM_VALIDATE=1",
        "candidate recv_ns mismatches freshness capture_recv_ns",
        "accepted candidate trigger_seq or trigger_recv_ns mismatches feed anchor",
        "accepted candidate paper-mode record mismatches timestamp, block, gross profit, or net profit",
        "matching paper route at the same timestamp/block/profit",
        "artifact_manifest.json missing, malformed, or hash mismatch",
        "artifact_manifest.json hashes every required artifact file and the contract rejects post-assembly drift",
        "no funded key",
        "no executor deployment",
    ] {
        assert!(text.contains(required), "plan missing live/AWS/gas detail {required}");
    }
}

#[test]
fn detailed_plan_contains_operator_sequence_and_final_verification_commands() {
    let plan = plan();
    let text = all_text(&plan);
    for required in [
        "cargo fmt --check -p engine -p engine-feed",
        "git diff --check",
        "cargo build --workspace --release",
        "cargo test --workspace --quiet",
        "cargo test -p engine --test remaining_gates_detailed_execution_plan_static -- --nocapture",
        "cargo test -p engine --test money_path_release_certification_static -- --nocapture",
        "cargo test -p engine --test gate9_statepoint_classification_static -- --nocapture",
        "cargo test -p engine --test aws_dryrun_artifact_contract -- --nocapture",
        "cargo test -p engine-feed --test live_capture_artifact_contract -- --nocapture",
        "scripts/assemble_aws_dryrun_artifact_dir.sh",
        "commit/review exact tested tree",
        "run AWS no-capital dry-run",
        "validate real AWS_DRYRUN_ARTIFACT_DIR",
        "only then reconsider live capital",
    ] {
        assert!(
            text.contains(required),
            "plan missing operator sequence {required}"
        );
    }
}
