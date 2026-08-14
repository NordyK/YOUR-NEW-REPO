use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const MANIFEST: &str = "../../../Codex/RUN_ARTIFACT_IMMUTABILITY_MANIFEST_2026-06-15.json";

fn workspace_path(path: &str) -> PathBuf {
    Path::new("../../..").join(path)
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn read_json(path: impl AsRef<Path>) -> Value {
    let raw = read(path);
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("invalid manifest JSON: {err}"))
}

fn obj<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("missing object key {key} in {value}"))
}

fn str_field(value: &Value, key: &str) -> String {
    obj(value, key)
        .as_str()
        .unwrap_or_else(|| panic!("missing string field {key} in {value}"))
        .to_string()
}

fn u64_field(value: &Value, key: &str) -> u64 {
    obj(value, key)
        .as_u64()
        .unwrap_or_else(|| panic!("missing u64 field {key} in {value}"))
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    obj(value, key)
        .as_array()
        .unwrap_or_else(|| panic!("missing array field {key} in {value}"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("non-string entry in {key}: {value}"))
                .to_string()
        })
        .collect()
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|err| panic!("failed to run sha256sum for {}: {err}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("sha256sum stdout must be utf8")
        .split_whitespace()
        .next()
        .expect("sha256sum output missing hash")
        .to_string()
}

fn assert_hash(section: &Value) {
    let path = workspace_path(&str_field(section, "path"));
    assert_eq!(
        sha256_file(&path),
        str_field(section, "sha256"),
        "artifact hash drifted for {}",
        path.display()
    );
}

fn jsonl(path: &str) -> Vec<Value> {
    read(workspace_path(path))
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{path} line {} invalid JSON: {err}", idx + 1))
        })
        .collect()
}

fn result_counts(rows: &[Value]) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for row in rows {
        let result = row
            .get("revm_result")
            .and_then(Value::as_str)
            .unwrap_or("missing");
        *out.entry(result.to_string()).or_insert(0) += 1;
    }
    out
}

fn non_null_field_count(rows: &[Value], key: &str) -> u64 {
    rows.iter()
        .filter(|row| row.get(key).is_some_and(|value| !value.is_null()))
        .count() as u64
}

fn positive_candidate_rows(rows: &[Value]) -> u64 {
    rows.iter()
        .filter(|row| {
            ["net_profit_wei", "balance_delta_wei"].iter().any(|key| {
                row.get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.parse::<i128>().is_ok_and(|parsed| parsed > 0))
            })
        })
        .count() as u64
}

#[test]
fn gate8_and_gate9_run_artifacts_are_hash_pinned_and_non_green_money() {
    let manifest = read_json(MANIFEST);

    let gate8 = obj(&manifest, "gate8_canonical_live_run");
    assert_eq!(
        str_field(gate8, "classification"),
        "capture_and_candidate_activity_green_but_money_freshness_economics_red"
    );
    let raw = obj(gate8, "raw_feed_capture");
    let candidates = obj(gate8, "candidate_records");
    let stdout = obj(gate8, "engine_live_stdout");
    for section in [raw, candidates, stdout] {
        assert_hash(section);
    }

    let raw_rows = jsonl(&str_field(raw, "path"));
    assert_eq!(raw_rows.len() as u64, u64_field(raw, "jsonl_rows"));
    let mut messages = 0u64;
    let mut min_seq = u64::MAX;
    let mut max_seq = 0u64;
    let mut min_ts = u64::MAX;
    let mut max_ts = 0u64;
    let mut recv_age_fields = 0u64;
    for row in &raw_rows {
        if row.get("recv_ns").is_some()
            || row.get("capture_recv_ns").is_some()
            || row.get("event_age_ms").is_some()
        {
            recv_age_fields += 1;
        }
        let Some(row_messages) = row.get("messages").and_then(Value::as_array) else {
            continue;
        };
        for message in row_messages {
            messages += 1;
            let seq = u64_field(message, "sequenceNumber");
            min_seq = min_seq.min(seq);
            max_seq = max_seq.max(seq);
            let ts = message
                .get("message")
                .and_then(|value| value.get("message"))
                .and_then(|value| value.get("header"))
                .and_then(|value| value.get("timestamp"))
                .and_then(Value::as_u64)
                .unwrap_or_else(|| panic!("Gate8 raw message missing header timestamp: {message}"));
            min_ts = min_ts.min(ts);
            max_ts = max_ts.max(ts);
        }
    }
    assert_eq!(messages, u64_field(raw, "sequencer_messages"));
    assert_eq!(min_seq, u64_field(raw, "min_sequence"));
    assert_eq!(max_seq, u64_field(raw, "max_sequence"));
    assert_eq!(min_ts, u64_field(raw, "min_header_timestamp"));
    assert_eq!(max_ts, u64_field(raw, "max_header_timestamp"));
    assert_eq!(
        recv_age_fields,
        u64_field(raw, "rows_with_recv_or_event_age_fields")
    );

    let candidate_rows = jsonl(&str_field(candidates, "path"));
    assert_eq!(
        candidate_rows.len() as u64,
        u64_field(candidates, "jsonl_rows")
    );
    let counts = result_counts(&candidate_rows);
    assert_eq!(
        counts.get("loss").copied().unwrap_or(0),
        u64_field(candidates, "revm_result_loss")
    );
    assert_eq!(
        counts.get("revert").copied().unwrap_or(0),
        u64_field(candidates, "revm_result_revert")
    );
    assert_eq!(
        positive_candidate_rows(&candidate_rows),
        u64_field(candidates, "positive_rows")
    );
    for key in [
        "net_profit_wei",
        "estimated_gas_wei",
        "balance_delta_wei",
        "block",
    ] {
        assert_eq!(
            non_null_field_count(&candidate_rows, key),
            u64_field(
                candidates,
                &format!(
                    "rows_with_{key_without_suffix}",
                    key_without_suffix = key.trim_end_matches("_wei")
                )
            ),
            "Gate8 field count drifted for {key}"
        );
    }

    let stdout_raw = read(workspace_path(&str_field(stdout, "path")));
    for required in string_array(stdout, "final_heartbeat_required_substrings") {
        assert!(
            stdout_raw.contains(&required),
            "Gate8 stdout missing required final heartbeat substring: {required}"
        );
    }
    for forbidden in string_array(stdout, "must_not_contain") {
        assert!(
            !stdout_raw.contains(&forbidden),
            "Gate8 stdout unexpectedly contains {forbidden}; update freshness verdict"
        );
    }

    let gate9 = obj(&manifest, "gate9_revm_transport_run");
    assert_eq!(
        str_field(gate9, "classification"),
        "transport_equivalence_green_replay_fields_and_partial_submit_economics_present_release_still_red"
    );
    let report = obj(gate9, "equivalence_report");
    let a_candidates = obj(gate9, "a_file_candidates");
    let b_candidates = obj(gate9, "b_binary_candidates");
    for section in [report, a_candidates, b_candidates] {
        assert_hash(section);
    }

    let report_text = read(workspace_path(&str_field(report, "path")));
    for line in string_array(report, "required_lines") {
        assert!(
            report_text.contains(&line),
            "Gate9 equivalence report missing line: {line}"
        );
    }

    for (label, section) in [("A", a_candidates), ("B", b_candidates)] {
        let rows = jsonl(&str_field(section, "path"));
        assert_eq!(
            rows.len() as u64,
            u64_field(section, "jsonl_rows"),
            "Gate9 {label} row count drifted"
        );
        let counts = result_counts(&rows);
        assert_eq!(
            counts.get("success").copied().unwrap_or(0),
            u64_field(section, "revm_result_success"),
            "Gate9 {label} success count drifted"
        );
        assert_eq!(
            counts.get("loss").copied().unwrap_or(0),
            u64_field(section, "revm_result_loss"),
            "Gate9 {label} loss count drifted"
        );
        assert_eq!(
            counts.get("revert").copied().unwrap_or(0),
            u64_field(section, "revm_result_revert"),
            "Gate9 {label} revert count drifted"
        );
        assert_eq!(
            non_null_field_count(&rows, "net_profit_wei"),
            u64_field(section, "rows_with_net_profit"),
            "Gate9 {label} net_profit presence drifted"
        );
        assert_eq!(
            non_null_field_count(&rows, "estimated_gas_wei"),
            u64_field(section, "rows_with_estimated_gas"),
            "Gate9 {label} estimated_gas presence drifted"
        );
    }

    let b_rows = jsonl(&str_field(b_candidates, "path"));
    let fields = b_rows
        .iter()
        .flat_map(|row| {
            row.as_object()
                .unwrap_or_else(|| panic!("Gate9 B row is not an object: {row}"))
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        fields,
        string_array(b_candidates, "expected_fields")
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "Gate9 B candidate field set drifted"
    );
    for missing in string_array(b_candidates, "missing_standalone_replay_fields") {
        assert!(
            !fields.contains(&missing),
            "Gate9 now persists {missing}; update standalone replay verdict"
        );
    }
}
