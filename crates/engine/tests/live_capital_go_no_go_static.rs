use std::{collections::BTreeSet, fs, path::Path};

use serde_json::Value;

const MATRIX: &str = "../../../Codex/LIVE_CAPITAL_GO_NO_GO_2026-06-19.json";

fn read_json(path: &str) -> Value {
    let raw = fs::read_to_string(Path::new(path))
        .unwrap_or_else(|err| panic!("failed to read {path}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("{path} is not valid JSON: {err}"))
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
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(all_text).collect::<Vec<_>>().join("\n"),
        Value::Object(map) => map.values().map(all_text).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

#[test]
fn go_no_go_matrix_preserves_current_release_boundary() {
    let matrix = read_json(MATRIX);
    assert_eq!(str_field(&matrix, "schema"), "live_capital_go_no_go.v1");
    assert!(
        str_field(&matrix, "created_utc").starts_with("2026-06-19T"),
        "go/no-go matrix must be current to this pass"
    );

    let hard = field(&matrix, "hard_boundaries");
    assert!(!bool_field(
        hard,
        "production_logic_modified_by_this_artifact"
    ));
    assert!(bool_field(hard, "dry_run_may_run_on_aws"));
    assert!(!bool_field(hard, "live_capital_may_run_on_aws"));
    assert!(!bool_field(hard, "executor_deploy_required_for_dry_run"));
    assert!(!bool_field(hard, "funded_key_allowed_for_dry_run"));
    assert!(!bool_field(hard, "submit_tx_allowed_for_dry_run"));
    assert!(!bool_field(hard, "dune_rows_are_money_proof"));
    assert!(!bool_field(hard, "loss_selector_is_money_proof"));
    assert!(!bool_field(
        hard,
        "parent_block_fork_is_exact_prestate_proof"
    ));

    let decision = field(&matrix, "current_decision");
    // 2026-07-04: operator reviewed + committed the tested tree (143fba0), so aws_dry_run advanced
    // from "go_after_operator_reviews_and_commits_current_tree" to "go". aws_live_capital MUST stay
    // no_go (AWS Gate-10 preconditions unrun) — that is the load-bearing safety assertion here.
    assert_eq!(str_field(decision, "aws_dry_run"), "go");
    assert_eq!(str_field(decision, "aws_live_capital"), "no_go");
}

#[test]
fn dry_run_invariants_forbid_accidental_capital_path() {
    let matrix = read_json(MATRIX);
    let invariants = array_field(&matrix, "dry_run_required_invariants")
        .iter()
        .map(|value| value.as_str().expect("invariant string").to_string())
        .collect::<BTreeSet<_>>();

    for required in [
        "DRY_RUN=true",
        "SUBMIT_TX=false",
        "REVM_VALIDATE=1",
        "no funded private key loaded",
        "no executor deployment dependency",
        "PAPER_MODE_LOG is set",
        "CANDIDATE_RECORD_JSONL is set",
        "raw sequencer capture is enabled",
        "freshness sidecar JSONL is enabled",
    ] {
        assert!(
            invariants.contains(required),
            "missing dry-run invariant: {required}"
        );
    }

    let text = all_text(&matrix);
    for unsafe_setting in [
        "SUBMIT_TX=true during dry-run",
        "funded private key loaded during dry-run",
        "REVM_VALIDATE=0 with live_submit and broadcast enabled",
    ] {
        assert!(
            text.contains(unsafe_setting),
            "missing explicit no-go condition: {unsafe_setting}"
        );
    }
}

#[test]
fn live_capital_preconditions_cover_all_current_blocker_classes() {
    let matrix = read_json(MATRIX);
    let preconditions = array_field(&matrix, "live_capital_preconditions");
    let ids = preconditions
        .iter()
        .map(|row| str_field(row, "id").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "standalone_money_path_green",
        "historical_exact_state_green",
        "feed_freshness_green",
        "submit_economics_green",
        "venue_parity_green",
        "deploy_atomicity_green",
    ] {
        assert!(ids.contains(required), "missing precondition {required}");
    }

    let classes = preconditions
        .iter()
        .map(|row| str_field(row, "blocking_class").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "certification_artifact_and_standalone_replay_gap",
        "historical_fork_proof_blocker",
        "aws_live_measurement_blocker",
        "production_economics_watch_gate",
        "contained_amm_kernel_quote_parity_issue",
        "deploy_process_blocker",
    ] {
        assert!(
            classes.contains(required),
            "missing blocking class {required}"
        );
    }

    for row in preconditions {
        assert!(bool_field(row, "required_before_live"));
        assert!(
            array_field(row, "must_prove").len() >= 4,
            "{} needs concrete close criteria",
            str_field(row, "id")
        );
        assert!(
            !str_field(row, "close_command").is_empty(),
            "{} needs a close command",
            str_field(row, "id")
        );
    }
}

#[test]
fn dry_run_artifacts_are_non_vacuous_and_joinable() {
    let matrix = read_json(MATRIX);
    let artifacts = array_field(&matrix, "aws_dry_run_artifacts");
    let names = artifacts
        .iter()
        .map(|row| str_field(row, "name").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "commit_and_binary_identity",
        "safe_environment_snapshot",
        "raw_feed_and_freshness",
        "candidate_and_paper_records",
        "gas_submit_state_matrix",
        "live_parity_output",
    ] {
        assert!(names.contains(required), "missing artifact {required}");
    }

    for row in artifacts {
        assert!(bool_field(row, "required"));
        let check = str_field(row, "non_vacuous_check");
        assert!(
            check.contains("record")
                || check.contains("proves")
                || check.contains("share")
                || check.contains("joinable")
                || check.contains("recorded")
                || check.contains("no unsafe over-quote"),
            "{} has weak non-vacuous check: {check}",
            str_field(row, "name")
        );
    }
}

#[test]
fn artifact_contract_test_is_pinned_for_real_aws_dryrun_directory() {
    let matrix = read_json(MATRIX);
    let contract = field(&matrix, "artifact_contract_test");
    assert_eq!(
        str_field(contract, "command"),
        "cargo test -p engine --test aws_dryrun_artifact_contract -- --nocapture"
    );
    assert_eq!(
        str_field(contract, "default_synthetic_command"),
        "cargo test -p engine --test aws_dryrun_artifact_contract -- --nocapture"
    );

    let required_files = array_field(contract, "required_files")
        .iter()
        .map(|value| value.as_str().expect("required file string").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "env.redacted",
        "binary.sha256",
        "run_metadata.json",
        "artifact_manifest.json",
        "raw_feed.jsonl",
        "raw_feed.jsonl.freshness.jsonl",
        "feed_candidate_freshness_report.json",
        "candidates.jsonl",
        "paper.jsonl",
        "nodeinterface_gas_matrix.json",
        "live_parity.json",
    ] {
        assert!(
            required_files.contains(required),
            "artifact contract missing required file {required}"
        );
    }

    let text = all_text(contract);
    for required in [
        "SUBMIT_TX=true",
        "funded private key loaded",
        "executor address dependency present during dry-run",
        "candidate feed_seq not joinable to freshness sidecar",
        "candidate recv_ns mismatches freshness capture_recv_ns",
        "accepted candidate trigger_seq or trigger_recv_ns mismatches feed anchor",
        "freshness seq missing from raw feed capture",
        "freshness sequencer_timestamp mismatches raw feed header timestamp",
        "freshness report p95/p99/max does not match joined candidates",
        "accepted stale candidate count greater than zero",
        "stale candidate missing reject_reason",
        "stale candidate rejected for a non-stale/non-freshness reason",
        "accepted success with net_profit_wei <= 0",
        "accepted candidate missing exact replay calldata/trigger anchors",
        "accepted candidate malformed route-shape arrays or hop metadata",
        "accepted candidate missing matching paper-mode route record",
        "accepted candidate paper-mode record mismatches timestamp, block, gross profit, or net profit",
        "candidate missing submit_min_profit_wei",
        "NodeInterface gas matrix missing candidate submit_calldata_bytes",
        "NodeInterface gas matrix l1_fee_wei or l2_gas_price_wei mismatches candidate submit_calldata_bytes",
        "binary.sha256 mismatch against expected release binary",
        "run_metadata.json missing or malformed",
        "run_metadata binary_sha256 mismatch against binary.sha256",
        "run_metadata run_duration_seconds below 300",
        "run_metadata command_line missing DRY_RUN=true, SUBMIT_TX=false, or REVM_VALIDATE=1",
        "run_metadata command_line includes SUBMIT_TX=true",
        "artifact_manifest.json missing, malformed, or hash mismatch",
        "live parity output reports unsafe over-quotes",
        "A real AWS_DRYRUN_ARTIFACT_DIR review is still required to certify an actual AWS run.",
    ] {
        assert!(
            text.contains(required),
            "artifact contract missing safety rejection or limit: {required}"
        );
    }
}

#[test]
fn research_basis_and_operator_sequence_match_the_deployment_risks() {
    let matrix = read_json(MATRIX);
    let sources = array_field(&matrix, "research_basis")
        .iter()
        .map(|row| str_field(row, "id").to_string())
        .collect::<BTreeSet<_>>();
    for required in [
        "foundry_rollfork_tx_hash",
        "foundry_transact_tx_hash",
        "dune_dex_trades_discovery",
        "arbitrum_nodeinterface",
        "aws_spot_interruption",
    ] {
        assert!(
            sources.contains(required),
            "missing research source {required}"
        );
    }

    let text = all_text(&matrix);
    for required in [
        "rollFork(txHash)",
        "transact(txHash)",
        "Dune tx_hash/project_contract_address/block_number rows are useful for corpus discovery",
        "NodeInterface L1 fee and L2 gas",
        "matching NodeInterface l1_fee_wei/l2_gas_price_wei samples",
        "submit_min_profit_wei",
        "submit_calldata_bytes",
        "current local A/B reruns prove measured-negative contract-success routes are rejected",
        "zero accepted success rows with net_profit_wei <= 0",
        "net_negative_after_gas",
        "exact calldata/replay anchors",
        "trigger tx hash",
        "internally consistent tokens/pools/exchange_ids/exchange_metadata",
        "every accepted net-positive candidate has a matching paper-mode route record",
        "same route_id, timestamp, block, gross profit, and net profit",
        "foundry_profit_proof_classification_static",
        "loss_as_pass_static",
        "FOUNDRY_PROFIT_PROOF_CLASSIFICATION_2026-06-15.json",
        "Loss-accepting fork tests are not release detector-money proof",
        "Review dirty tree and commit the exact tested state",
        "run_duration_seconds >= 300",
        "artifact_manifest.json",
        "command_line must include DRY_RUN=true, SUBMIT_TX=false, and REVM_VALIDATE=1",
        "Join feed freshness to candidates and compute p95, p99, and max event_age_ms",
        "raw feed sequenceNumber/header timestamp",
        "matching recv_ns/capture_recv_ns",
        "trigger_seq",
        "trigger_recv_ns",
        "feed_candidate_freshness_report.json recomputes joined candidate count",
        "Validate the completed AWS dry-run directory with aws_dryrun_artifact_contract",
        "Only after all live_capital_preconditions are green",
    ] {
        assert!(
            text.contains(required),
            "missing deployment risk detail: {required}"
        );
    }
}
