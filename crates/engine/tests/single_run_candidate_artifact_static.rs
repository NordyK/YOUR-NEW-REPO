use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

const CERT_DIR: &str = "certification_runs/mantis_cert_20260624T142153Z_62a2f45";
const CANDIDATES: &str =
    "certification_runs/mantis_cert_20260624T142153Z_62a2f45/bridge_run/candidate_records.jsonl";

const REQUIRED_KEYS: &[&str] = &[
    "calldata",
    "mantis_calldata",
    "executor_calldata",
    "block",
    "block_env",
    "block_timestamp",
    "snapshot_block",
    "changed_slots",
    "mantis_executor",
    "payee",
    "gas_limit",
    "min_profit",
    "exchange_ids",
    "exchange_metadata",
    "l1_fee_wei",
    "l2_gas_price_wei",
    "submit_min_profit_wei",
    "submit_calldata_bytes",
    "net_profit_wei",
    "tx_hash",
    "trigger_tx_hash",
    "transaction_index",
    "log_index",
];

fn engine_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("engine/crates/engine has engine root ancestor")
        .to_path_buf()
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let raw =
        fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    raw.lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            if line.trim().is_empty() {
                return None;
            }
            Some(
                serde_json::from_str::<Value>(line).unwrap_or_else(|err| {
                    panic!("parse {} line {}: {err}", path.display(), idx + 1)
                }),
            )
        })
        .collect()
}

fn decimal_i128(row: &Value, key: &str) -> i128 {
    match row.get(key).unwrap_or_else(|| panic!("missing {key}")) {
        Value::String(raw) => raw
            .parse()
            .unwrap_or_else(|err| panic!("parse {key}={raw}: {err}")),
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .unwrap_or_else(|| panic!("numeric {key} is not i64-compatible: {number}")),
        other => panic!("{key} is not a decimal string/number: {other:?}"),
    }
}

fn non_empty(row: &Value, key: &str) -> bool {
    match row.get(key) {
        Some(Value::Null) | None => false,
        Some(Value::String(s)) => !s.is_empty() && s != "0x",
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(items)) => !items.is_empty(),
        Some(_) => true,
    }
}

fn hex_bytes_len(hex: &str) -> usize {
    assert!(hex.starts_with("0x"), "hex payload must start with 0x");
    assert_eq!(hex.len() % 2, 0, "hex payload must have even length");
    (hex.len() - 2) / 2
}

#[test]
fn latest_bridge_candidate_records_are_exact_state_complete_for_accepted_rows() {
    let path = engine_root().join(CANDIDATES);
    let rows = read_jsonl(&path);
    assert_eq!(
        rows.len(),
        114,
        "latest bridge-run candidate row count drifted; reclassify the artifact"
    );

    let mut accepted = 0usize;
    let mut accepted_positive = 0usize;
    let mut accepted_routes = BTreeSet::<u64>::new();
    let mut accepted_hop_counts = BTreeSet::<usize>::new();

    for (idx, row) in rows.iter().enumerate() {
        let line = idx + 1;
        for key in REQUIRED_KEYS {
            assert!(row.get(*key).is_some(), "line {line}: missing {key}");
        }

        if row.get("revm_result").and_then(Value::as_str) == Some("success")
            && row.get("reject_reason").and_then(Value::as_str) == Some("none")
        {
            accepted += 1;
            let net = decimal_i128(row, "net_profit_wei");
            assert!(
                net > 0,
                "line {line}: accepted success has non-positive net {net}"
            );
            accepted_positive += 1;

            for key in [
                "calldata",
                "mantis_calldata",
                "executor_calldata",
                "block_env",
                "changed_slots",
                "mantis_executor",
                "payee",
                "gas_limit",
                "min_profit",
                "exchange_ids",
                "exchange_metadata",
                "l1_fee_wei",
                "l2_gas_price_wei",
                "submit_min_profit_wei",
                "submit_calldata_bytes",
                "tx_hash",
            ] {
                assert!(
                    non_empty(row, key),
                    "line {line}: accepted row has empty {key}"
                );
            }

            let calldata = row["calldata"].as_str().expect("calldata string");
            assert_eq!(
                row["mantis_calldata"].as_str(),
                Some(calldata),
                "line {line}: mantis_calldata must equal calldata"
            );
            assert_eq!(
                row["executor_calldata"].as_str(),
                Some(calldata),
                "line {line}: executor_calldata must equal calldata"
            );

            let submit_calldata_bytes = row["submit_calldata_bytes"]
                .as_u64()
                .expect("submit_calldata_bytes") as usize;
            assert_eq!(
                hex_bytes_len(calldata),
                submit_calldata_bytes,
                "line {line}: calldata hex length must match submit_calldata_bytes"
            );

            let exchange_ids = row["exchange_ids"].as_array().expect("exchange_ids array");
            let hop_count = exchange_ids.len();
            accepted_hop_counts.insert(hop_count);
            assert_eq!(
                submit_calldata_bytes,
                engine_routes::calldata::mantis_calldata_len(hop_count),
                "line {line}: submit_calldata_bytes must match Mantis hop encoder length"
            );
            assert_eq!(
                row["exchange_metadata"]
                    .as_array()
                    .expect("exchange_metadata")
                    .len(),
                hop_count,
                "line {line}: exchange_metadata length must match exchange_ids"
            );

            accepted_routes.insert(row["route_id"].as_u64().expect("route_id"));
        }
    }

    assert_eq!(accepted, 11, "accepted success row count drifted");
    assert_eq!(accepted_positive, 11, "accepted positive row count drifted");
    assert_eq!(
        accepted_routes.len(),
        10,
        "accepted unique route count drifted"
    );
    assert!(
        accepted_hop_counts.contains(&2)
            && accepted_hop_counts.contains(&3)
            && accepted_hop_counts.contains(&4),
        "accepted artifact should cover 2/3/4-hop rows, got {accepted_hop_counts:?}"
    );
}

#[test]
fn latest_mantis_cert_directory_is_not_a_complete_release_run_artifact() {
    let dir = engine_root().join(CERT_DIR);
    assert!(
        dir.join("MANIFEST.md").is_file(),
        "human manifest should exist"
    );
    assert!(
        dir.join("bridge_run/candidate_records.jsonl").is_file(),
        "bridge candidate records should exist"
    );
    assert!(
        !dir.join("run_metadata.json").exists(),
        "run_metadata.json now exists; update this blocker classification"
    );
    assert!(
        !dir.join("artifact_manifest.json").exists(),
        "artifact_manifest.json now exists; update this blocker classification"
    );
}
