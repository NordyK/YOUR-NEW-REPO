//! WI-2 certification gate: candidate records persist full economics + a standalone-replay anchor.
//!
//! Codex flagged that Gate-8/9 candidate rows wrote `estimated_gas_wei:null`, `net_profit_wei:null`,
//! null `block`, and carried no `tx_hash` — so they were neither economically complete nor
//! standalone-replayable. After the WI-2 fix the engine's `write_candidate_record_jsonl` populates
//! `gas_used_units` (revm-measured), `estimated_gas_wei` (L2+L1 ETH-wei), `net_profit_wei` (ETH-wei),
//! a non-null `block` (seq+nitro-genesis when unpinned), and `tx_hash` (triggering feed tx).
//!
//! The fixture is a representative slice of a real controlled dry-run candidate_records.jsonl
//! (scripts/dryrun_replay_artifact.sh, REVM_VALIDATE=1, no broadcast). It MUST contain at least one
//! `success` row so the money economics are exercised.

use std::{fs, path::Path};

use serde_json::Value;

const FIXTURE: &str = "../../contracts/test/fixtures/candidate_records_economics_sample.jsonl";

fn rows() -> Vec<Value> {
    let raw =
        fs::read_to_string(Path::new(FIXTURE)).unwrap_or_else(|e| panic!("read {FIXTURE}: {e}"));
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("bad JSON {l}: {e}")))
        .collect()
}

fn is_decimal_string(row: &Value, key: &str) -> bool {
    row.get(key)
        .and_then(Value::as_str)
        .map(|s| {
            !s.is_empty()
                && s.trim_start_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_digit())
        })
        .unwrap_or(false)
}

#[test]
fn candidate_records_persist_economics_and_replay_anchor() {
    let rows = rows();
    assert!(!rows.is_empty(), "economics fixture is empty");

    let mut success = 0usize;
    for (i, r) in rows.iter().enumerate() {
        let n = i + 1;
        // Every row: schema present + block anchored (non-null) + revm enabled + known verdict.
        assert!(
            r.get("block").and_then(Value::as_u64).unwrap_or(0) > 0,
            "row {n}: block must be a non-null L2 block (WI-2 default)"
        );
        assert_eq!(
            r.get("revm_enabled").and_then(Value::as_bool),
            Some(true),
            "row {n}: revm_enabled must be true"
        );
        let result = r.get("revm_result").and_then(Value::as_str).unwrap_or("");
        assert!(
            matches!(result, "success" | "loss" | "revert" | "halt"),
            "row {n}: unexpected revm_result {result}"
        );
        // New economics keys MUST exist on every row (value may be null for never-executed routes).
        for key in [
            "gas_used_units",
            "estimated_gas_wei",
            "net_profit_wei",
            "tx_hash",
            "log_index",
        ] {
            assert!(r.get(key).is_some(), "row {n}: missing economics key {key}");
        }

        if result == "success" {
            success += 1;
            // A money row MUST carry full, consistent economics.
            assert!(
                r.get("gas_used_units").and_then(Value::as_u64).unwrap_or(0) > 0,
                "row {n}: success must persist measured gas_used_units"
            );
            assert!(
                is_decimal_string(r, "estimated_gas_wei"),
                "row {n}: success must persist estimated_gas_wei"
            );
            assert!(
                is_decimal_string(r, "net_profit_wei"),
                "row {n}: success must persist net_profit_wei"
            );
            assert!(
                is_decimal_string(r, "balance_delta_wei"),
                "row {n}: success must persist balance_delta_wei"
            );
            // Standalone-replay anchor: a 32-byte tx_hash OR snapshot_block+block_env.
            let tx = r.get("tx_hash").and_then(Value::as_str).unwrap_or("");
            let tx_anchor = tx.starts_with("0x") && tx.len() == 66;
            let block_anchor = r.get("snapshot_block").and_then(Value::as_u64).is_some()
                && r.get("block_env").and_then(Value::as_u64).is_some();
            assert!(
                tx_anchor || block_anchor,
                "row {n}: success must carry a replay anchor (tx_hash OR snapshot_block+block_env)"
            );
        }
    }
    assert!(
        success > 0,
        "fixture must contain at least one REVM success row to exercise money economics"
    );
}

/// Codex money-safety blocker: a candidate marked ACCEPTED (revm_result=success, reject_reason=none)
/// must be NET-POSITIVE after gas. The validator net-gates `pass`, and a net-negative contract-success
/// is recorded with reject_reason="net_negative_after_gas" — never "none". This test fails if any
/// accepted row has net_profit_wei <= 0 (i.e., the engine reached dry-run submission on a losing trade).
#[test]
fn accepted_success_rows_are_net_positive() {
    for (i, r) in rows().iter().enumerate() {
        let n = i + 1;
        let result = r.get("revm_result").and_then(Value::as_str).unwrap_or("");
        let reject = r.get("reject_reason").and_then(Value::as_str).unwrap_or("");
        if result == "success" && reject == "none" {
            let net: i128 = r
                .get("net_profit_wei")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| {
                    panic!("row {n}: accepted success row missing numeric net_profit_wei")
                });
            assert!(
                net > 0,
                "row {n}: ACCEPTED success row (reject_reason=none) is net-negative ({net} wei) — \
                 the engine would reach dry-run submission on a losing trade"
            );
        }
        // A net-negative contract-success must be explicitly rejected, not silently "none".
        if result == "success" && reject != "none" {
            assert!(
                reject.starts_with("net_negative_after_gas")
                    || reject.starts_with("net_unprovable"),
                "row {n}: success with non-none reject_reason must name the net reason, got {reject:?}"
            );
        }
    }
}
