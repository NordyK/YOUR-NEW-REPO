//! Full candidate-record schema certification (WI-2, expanded per Codex schema matrix).
//!
//! Two enforced invariants:
//!   1. The production writer `write_candidate_record_jsonl` emits EVERY field the matrix lists as
//!      required for standalone replay + net-after-gas economics.
//!   2. The regenerated current-source artifact (a committed fixture sliced from a real controlled
//!      dry-run) carries non-null economics + replay anchors on every REVM-success row.
//!
//! This supersedes the earlier frozen-gap snapshot (which hash-pinned pipeline.rs and asserted the
//! extra fields were ABSENT) now that the writer actually emits the full schema.

use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

const MATRIX: &str = "Codex/CANDIDATE_RECORD_SCHEMA_MATRIX_2026-06-15.json";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn matrix() -> Value {
    serde_json::from_str(&read(&repo_root().join(MATRIX))).expect("matrix json")
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array {key}"))
        .iter()
        .map(|v| v.as_str().expect("array item string").to_string())
        .collect()
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string {key}"))
        .to_string()
}

/// The `let record = serde_json::json!({ ... })` block inside the candidate-record writer.
fn writer_record_block(src: &str) -> &str {
    let f = src
        .find("fn write_candidate_record_jsonl(")
        .expect("writer function");
    let func = &src[f..];
    let start = func
        .find("let record = serde_json::json!({")
        .expect("record json start");
    let after = &func[start..];
    let end = after.find("});").expect("record json end");
    &after[..end]
}

#[test]
fn writer_emits_full_standalone_replay_and_gas_schema() {
    let m = matrix();
    let pipeline = read(&repo_root().join("engine/crates/engine/src/pipeline.rs"));
    let record = writer_record_block(&pipeline);

    for field in string_array(&m, "required_record_fields") {
        assert!(
            record.contains(&format!("\"{field}\":")),
            "writer record block is missing required field {field}"
        );
    }
    for marker in string_array(&m, "writer_wiring_markers") {
        assert!(
            pipeline.contains(&marker),
            "writer wiring marker absent: {marker}"
        );
    }
}

#[test]
fn regenerated_artifact_has_full_schema_with_nonnull_economics_on_success() {
    let m = matrix();
    let artifact = repo_root().join(string_field(&m, "regenerated_artifact_fixture"));
    let rows: Vec<Value> = read(&artifact)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("candidate row"))
        .collect();
    assert!(!rows.is_empty(), "regenerated artifact fixture is empty");

    let required = string_array(&m, "required_record_fields");
    let nonnull = string_array(&m, "required_non_null_on_success");

    let mut success = 0usize;
    for (i, r) in rows.iter().enumerate() {
        for field in &required {
            assert!(r.get(field).is_some(), "row {i}: missing field {field}");
        }
        let result = r.get("revm_result").and_then(Value::as_str).unwrap_or("");
        assert!(
            matches!(result, "success" | "loss" | "revert" | "halt"),
            "row {i}: unexpected revm_result {result}"
        );
        if result == "success" {
            success += 1;
            for field in &nonnull {
                assert!(
                    r.get(field).map(|v| !v.is_null()).unwrap_or(false),
                    "row {i}: REVM-success row has null required-economics field {field}"
                );
            }
            // Standalone-replay anchor: a 32-byte tx_hash (intra-block feed-triggered backrun)
            // OR snapshot_block+block_env (resync/boundary candidate). At least one must be present.
            let tx = r.get("tx_hash").and_then(Value::as_str).unwrap_or("");
            let tx_anchor = tx.starts_with("0x") && tx.len() == 66;
            let block_anchor = r.get("snapshot_block").and_then(Value::as_u64).is_some()
                && r.get("block_env").and_then(Value::as_u64).is_some();
            assert!(
                tx_anchor || block_anchor,
                "row {i}: success needs a replay anchor (tx_hash OR snapshot_block+block_env)"
            );
            let cd = r.get("calldata").and_then(Value::as_str).unwrap_or("");
            assert!(
                cd.starts_with("0x") && cd.len() > 10,
                "row {i}: success needs non-empty executor calldata for standalone replay"
            );
        }
    }
    assert!(
        success > 0,
        "fixture must contain at least one REVM-success row to prove net-after-gas economics"
    );
}
