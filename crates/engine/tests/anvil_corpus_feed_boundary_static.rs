use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use serde_json::Value;

const DIRECT_FEED_ROW_KEYS: &[&str] = &["l2Msg", "sequenceNumber", "timestamp"];
const CAPTURE_ROW_KEYS: &[&str] = &["messages", "version"];
const CONFIRMED_SEQUENCE_ROW_KEYS: &[&str] = &["confirmedSequenceNumberMessage", "version"];
const CAPTURE_MESSAGE_KEYS: &[&str] = &[
    "blockHash",
    "blockMetadata",
    "message",
    "sequenceNumber",
    "signatureV2",
];

const EXECUTABLE_STATE_KEYS: &[&str] = &[
    "account_proofs",
    "accounts",
    "amp",
    "balance_delta_wei",
    "balances",
    "block_env",
    "current_tick",
    "exchange_price",
    "fee_growth",
    "initialised_ticks",
    "initialized_ticks",
    "liquidity",
    "net_profit_wei",
    "price_scale",
    "reserve0",
    "reserve1",
    "reserves",
    "snapshot_block",
    "sqrtPriceX96",
    "sqrt_price",
    "sqrt_price_x96",
    "slot0",
    "state",
    "state_block",
    "state_root",
    "storage",
    "storage_proof",
    "tick",
    "tick_window",
    "ticks",
];

#[derive(Debug, Default)]
struct CorpusFeedStats {
    files: usize,
    rows: usize,
    direct_feed_rows: usize,
    capture_rows: usize,
    capture_messages: usize,
    confirmed_sequence_rows: usize,
    unexpected_schemas: Vec<String>,
    state_key_hits: Vec<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn corpus_paths() -> Vec<PathBuf> {
    let mut paths = fs::read_dir(repo_root().join("anvil/corpus"))
        .expect("read anvil corpus dir")
        .map(|entry| entry.expect("read anvil corpus dir entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn sorted_object_keys(value: &Value) -> Option<Vec<String>> {
    Some(
        value
            .as_object()?
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    )
}

fn expected_keys(expected: &[&str]) -> Vec<String> {
    expected
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn push_limited(out: &mut Vec<String>, value: String) {
    if out.len() < 20 {
        out.push(value);
    }
}

fn collect_executable_state_keys(path: &str, value: &Value, hits: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let child_path = format!("{path}.{key}");
                if EXECUTABLE_STATE_KEYS.contains(&key.as_str()) {
                    push_limited(hits, child_path.clone());
                }
                collect_executable_state_keys(&child_path, nested, hits);
            }
        }
        Value::Array(items) => {
            for (idx, nested) in items.iter().enumerate() {
                collect_executable_state_keys(&format!("{path}[{idx}]"), nested, hits);
            }
        }
        _ => {}
    }
}

fn scan_corpus_file(path: &Path, stats: &mut CorpusFeedStats) {
    let file = File::open(path).unwrap_or_else(|err| panic!("open {}: {err}", path.display()));
    let reader = BufReader::new(file);
    let display = path
        .strip_prefix(repo_root())
        .unwrap_or(path)
        .display()
        .to_string();

    stats.files += 1;
    for (line_idx, line) in reader.lines().enumerate() {
        let line = line.unwrap_or_else(|err| panic!("read {}:{}: {err}", display, line_idx + 1));
        if line.trim().is_empty() {
            continue;
        }

        stats.rows += 1;
        let value = serde_json::from_str::<Value>(&line)
            .unwrap_or_else(|err| panic!("parse {}:{} json: {err}", display, line_idx + 1));
        let path_label = format!("{}:{}", display, line_idx + 1);
        collect_executable_state_keys(&path_label, &value, &mut stats.state_key_hits);

        let keys = sorted_object_keys(&value)
            .unwrap_or_else(|| panic!("{path_label} must be a JSON object"));
        if keys == expected_keys(DIRECT_FEED_ROW_KEYS) {
            stats.direct_feed_rows += 1;
            assert!(
                value.get("l2Msg").and_then(Value::as_str).is_some(),
                "{path_label} l2Msg must be a string feed payload"
            );
            assert!(
                value
                    .get("sequenceNumber")
                    .and_then(Value::as_u64)
                    .is_some(),
                "{path_label} sequenceNumber must be numeric"
            );
            assert!(
                value.get("timestamp").and_then(Value::as_u64).is_some(),
                "{path_label} timestamp must be numeric"
            );
            continue;
        }

        if keys == expected_keys(CAPTURE_ROW_KEYS) {
            stats.capture_rows += 1;
            assert_eq!(
                value.get("version").and_then(Value::as_u64),
                Some(1),
                "{path_label} capture row version drifted"
            );
            let messages = value
                .get("messages")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("{path_label} messages must be an array"));
            assert!(
                !messages.is_empty(),
                "{path_label} capture row must contain at least one message"
            );
            stats.capture_messages += messages.len();
            for (msg_idx, message) in messages.iter().enumerate() {
                let message_keys = sorted_object_keys(message)
                    .unwrap_or_else(|| panic!("{path_label}.messages[{msg_idx}] must be object"));
                assert_eq!(
                    message_keys,
                    expected_keys(CAPTURE_MESSAGE_KEYS),
                    "{path_label}.messages[{msg_idx}] schema drifted"
                );
                let payload = message
                    .get("message")
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| {
                        panic!("{path_label}.messages[{msg_idx}].message must be an object")
                    });
                assert!(
                    payload
                        .get("delayedMessagesRead")
                        .and_then(Value::as_u64)
                        .is_some(),
                    "{path_label}.messages[{msg_idx}].message.delayedMessagesRead must be numeric"
                );
                let nested_message = payload
                    .get("message")
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| {
                        panic!("{path_label}.messages[{msg_idx}].message.message must be an object")
                    });
                assert!(
                    nested_message.get("l2Msg").and_then(Value::as_str).is_some(),
                    "{path_label}.messages[{msg_idx}].message.message.l2Msg must be a string feed payload"
                );
            }
            continue;
        }

        if keys == expected_keys(CONFIRMED_SEQUENCE_ROW_KEYS) {
            stats.confirmed_sequence_rows += 1;
            assert_eq!(
                value.get("version").and_then(Value::as_u64),
                Some(1),
                "{path_label} confirmed-sequence row version drifted"
            );
            assert!(
                value
                    .get("confirmedSequenceNumberMessage")
                    .and_then(|message| message.get("sequenceNumber"))
                    .and_then(Value::as_u64)
                    .is_some(),
                "{path_label} confirmed-sequence row must carry numeric sequenceNumber"
            );
            continue;
        }

        push_limited(
            &mut stats.unexpected_schemas,
            format!("{path_label} keys={keys:?}"),
        );
    }
}

#[test]
fn saved_anvil_corpus_jsonl_is_feed_only_not_state_snapshot() {
    let corpus_dir = repo_root().join("anvil/corpus");
    if !corpus_dir.exists() {
        eprintln!(
            "SKIP: artifact {} absent (out-of-branch)",
            corpus_dir.display()
        );
        return;
    }
    let paths = corpus_paths();
    assert!(
        paths.len() >= 12,
        "saved corpus must remain non-vacuous; paths={paths:?}"
    );

    let mut stats = CorpusFeedStats::default();
    for path in paths {
        scan_corpus_file(&path, &mut stats);
    }

    assert!(
        stats.rows >= 156_000,
        "saved corpus row count drifted below useful laptop-certification size: {stats:?}"
    );
    assert!(
        stats.direct_feed_rows >= 400,
        "block/window corpus rows must remain represented: {stats:?}"
    );
    assert!(
        stats.capture_rows >= 155_000 && stats.capture_messages >= stats.capture_rows,
        "large capture corpus rows/messages must remain represented: {stats:?}"
    );
    assert!(
        stats.confirmed_sequence_rows >= 300,
        "confirmed-sequence feed marker rows must remain represented: {stats:?}"
    );
    assert!(
        stats.unexpected_schemas.is_empty(),
        "saved corpus JSONL schemas drifted; review before counting as feed-only proof: {stats:?}"
    );

    assert!(
        stats.state_key_hits.is_empty(),
        "saved anvil corpus JSONL now contains executable AMM state keys; review before treating corpus rows as feed-only route/replay inputs: {stats:?}"
    );
}
