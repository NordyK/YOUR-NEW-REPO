use std::{collections::BTreeSet, fs, path::PathBuf};

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn read_repo(path: &str) -> String {
    fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", repo_root().join(path).display()))
}

fn read_json(path: &str) -> Value {
    serde_json::from_str(&read_repo(path))
        .unwrap_or_else(|err| panic!("{path} invalid JSON: {err}"))
}

fn csv_rows(path: &str, expected_header: &str, expected_cols: usize) -> Vec<Vec<String>> {
    read_repo(path)
        .lines()
        .enumerate()
        .filter_map(|(idx, raw)| {
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() {
                return None;
            }
            if idx == 0 {
                assert_eq!(line, expected_header, "unexpected CSV header in {path}");
                return None;
            }
            let fields = line.split(',').map(str::to_string).collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                expected_cols,
                "line {} in {path} has wrong field count",
                idx + 1
            );
            Some(fields)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct StrictCounts {
    positive: u64,
    loss: u64,
    other_revert: u64,
}

fn strict_detector_rows() -> Vec<Vec<String>> {
    csv_rows(
        "Codex/TOKEN_AWARE_STRICT_DETECTOR_VERDICT_2026-06-14.csv",
        "route_id,block_number,hop_count,exchange_ids,start_token,end_token,input_amount,gross_profit,outcome,realized_net_wei,reason,token_path,pool_path",
        13,
    )
}

fn state_point_loss_rows() -> Vec<Vec<String>> {
    csv_rows(
        "Codex/STATE_POINT_SENSITIVE_LOSS_ROWS_2026-06-14.csv",
        "route_id,detector_block_tag,hop_count,exchange_ids,family_path,start_is_weth,input_amount,gross_profit,foundry_loss_wei,occurrences,success_occurrences,loss_occurrences,first_seq,first_balance_delta_wei,last_seq,last_loss_shortfall_wei,last_gas_used,seq_results,token_path,pool_path",
        20,
    )
}

fn strict_key(row: &[String]) -> (String, String, String) {
    (row[0].clone(), row[6].clone(), row[3].clone())
}

fn state_point_key(row: &[String]) -> (String, String, String) {
    (row[0].clone(), row[6].clone(), row[3].clone())
}

#[test]
fn loss_accepting_fork_tests_are_never_counted_as_release_money_proof() {
    let matrix = read_json("Codex/FOUNDRY_PROFIT_PROOF_CLASSIFICATION_2026-06-15.json");
    assert_eq!(
        matrix.get("schema").and_then(Value::as_str),
        Some("foundry_profit_proof_classification.v1")
    );

    let rows = matrix
        .get("rows")
        .and_then(Value::as_array)
        .expect("matrix rows");
    let mut counted_money = BTreeSet::new();
    let mut non_money_loss_acceptors = 0usize;

    for row in rows {
        let file = row.get("file").and_then(Value::as_str).expect("file field");
        let class = row
            .get("class")
            .and_then(Value::as_str)
            .expect("class field");
        let loss_role = row
            .get("loss_acceptance_role")
            .and_then(Value::as_str)
            .expect("loss_acceptance_role field");
        let counts_as_money = row
            .get("counts_as_release_detector_money_proof")
            .and_then(Value::as_bool)
            .expect("counts_as_release_detector_money_proof field");

        if counts_as_money {
            counted_money.insert(file.to_string());
            assert!(
                matches!(
                    class,
                    "direct_realized_profit_proof" | "strict_detector_profit_gate"
                ),
                "{file} is counted as detector money but class={class}"
            );
            assert!(
                matches!(
                    loss_role,
                    "loss_rejected_for_money" | "loss_rejected_for_strict_gate" | "no_loss_surface"
                ),
                "{file} is counted as detector money but loss role is {loss_role}"
            );
        }

        if matches!(
            loss_role,
            "loss_as_mechanical_success"
                | "loss_as_diagnostic_classification"
                | "infra_reverts_allowed_not_money"
        ) {
            non_money_loss_acceptors += 1;
            assert!(
                !counts_as_money,
                "{file} accepts Loss/revert as a non-money condition but is counted as release money"
            );
        }
    }

    assert_eq!(
        counted_money,
        BTreeSet::from([
            "MantisDetectExecuteProfit.Fork.t.sol".to_string(),
            "MantisDetectorVerdict.Fork.t.sol".to_string(),
            "MantisGate9CalldataReplay.Fork.t.sol".to_string(),
            "MantisGate9CleanSuccessCorpusReplay.Fork.t.sol".to_string(),
        ]),
        "release detector-money proof set drifted"
    );
    assert!(
        non_money_loss_acceptors > 0,
        "test must remain non-vacuous: no Loss-accepting non-money tests found"
    );
}

#[test]
fn strict_detector_loss_rows_are_classified_state_point_artifacts_not_money_proof() {
    let strict_rows = strict_detector_rows();
    let mut counts = StrictCounts::default();
    let mut loss_keys = BTreeSet::new();
    let mut positive_net_sum = 0i128;

    for row in &strict_rows {
        let outcome = row[8].as_str();
        let realized_net = row[9].parse::<i128>().ok();
        match outcome {
            "positive" => {
                counts.positive += 1;
                let net = realized_net.expect("positive row realized_net_wei");
                assert!(net > 0, "positive strict row has non-positive net: {row:?}");
                positive_net_sum += net;
            }
            "loss" => {
                counts.loss += 1;
                let net = realized_net.expect("loss row realized_net_wei");
                assert!(net < 0, "Loss strict row has non-negative net: {row:?}");
                assert!(
                    row[10].contains("Loss profit gate shortfall"),
                    "Loss row reason does not identify profit gate: {row:?}"
                );
                loss_keys.insert(strict_key(row));
            }
            "other_revert" => {
                counts.other_revert += 1;
                assert!(
                    row[9].is_empty(),
                    "metadata/other revert row must not carry realized net: {row:?}"
                );
            }
            other => panic!("unexpected strict detector outcome {other}"),
        }
    }

    assert_eq!(
        counts,
        StrictCounts {
            positive: 12,
            loss: 11,
            other_revert: 6,
        },
        "strict detector outcome counts drifted"
    );
    assert!(
        positive_net_sum > 0,
        "strict positives should have positive realized-net sum"
    );

    let state_rows = state_point_loss_rows();
    let state_keys = state_rows.iter().map(|row| state_point_key(row)).collect();
    assert_eq!(
        loss_keys, state_keys,
        "every strict Loss row must be covered by the state-point-sensitive artifact"
    );

    for row in state_rows {
        let occurrences = row[9].parse::<u64>().expect("occurrences");
        let success_occurrences = row[10].parse::<u64>().expect("success_occurrences");
        let loss_occurrences = row[11].parse::<u64>().expect("loss_occurrences");
        let first_delta = row[13].parse::<i128>().expect("first_balance_delta_wei");
        let last_shortfall = row[15].parse::<u128>().expect("last_loss_shortfall_wei");
        assert_eq!(occurrences, success_occurrences + loss_occurrences);
        assert!(
            success_occurrences > 0 && first_delta > 0,
            "state-point Loss row lacks first positive occurrence: {row:?}"
        );
        assert!(
            loss_occurrences > 0 && last_shortfall > 0,
            "state-point Loss row lacks later loss occurrence: {row:?}"
        );
    }
}

#[test]
fn current_report_keeps_detector_profitability_red_without_calling_it_production_bug() {
    let report = read_json("Codex/CURRENT_REMAINING_GAPS_CERTIFICATION_REPORT_2026-06-18.json");
    let blocker = report
        .get("verified_red_or_blocked")
        .and_then(Value::as_array)
        .expect("verified_red_or_blocked")
        .iter()
        .find(|row| {
            row.get("id").and_then(Value::as_str) == Some("release_money_path_not_fully_certified")
        })
        .expect("release money path blocker");

    assert_eq!(
        blocker
            .get("production_logic_issue")
            .and_then(Value::as_bool),
        Some(false),
        "strict detector artifact gap should not be reported as a proven production bug"
    );
    let evidence = blocker
        .get("evidence")
        .and_then(Value::as_str)
        .expect("blocker evidence");
    for required in [
        "token-aware strict detector artifact is not standalone-clean",
        "direct_positive=12",
        "losses=11",
        "metadata_limited_other_reverts=6",
        "state-point-sensitive",
    ] {
        assert!(
            evidence.contains(required),
            "release money-path blocker evidence missing {required}"
        );
    }
}
