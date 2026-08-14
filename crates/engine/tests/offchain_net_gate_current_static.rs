use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const MATRIX: &str = "../../../Codex/OFFCHAIN_NET_GATE_CURRENT_EVIDENCE_2026-06-15.json";
const MASTER: &str = "../../../Codex/MONEY_PATH_FULL_CERTIFICATION_MASTER_MATRIX_2026-06-15.json";
const A_RERUN: &str = "../../../Codex/GATE9_CURRENT_SOURCE_UNIQUE_RERUN_2026-06-16.json";
const B_RERUN: &str = "../../../Codex/GATE9_B_BINARY_MONITORED_RERUN_2026-06-16.json";

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
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("invalid JSON: {err}"))
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

fn bool_field(value: &Value, key: &str) -> bool {
    obj(value, key)
        .as_bool()
        .unwrap_or_else(|| panic!("missing bool field {key} in {value}"))
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

fn jsonl_rows(path: impl AsRef<Path>) -> Vec<Value> {
    read(path)
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            if line.trim().is_empty() {
                return None;
            }
            Some(
                serde_json::from_str::<Value>(line)
                    .unwrap_or_else(|err| panic!("line {} invalid JSON: {err}", idx + 1)),
            )
        })
        .collect()
}

fn route_id(row: &Value) -> u64 {
    u64_field(row, "route_id")
}

fn decimal_i128(row: &Value, key: &str) -> Option<i128> {
    row.get(key).and_then(Value::as_str).map(|raw| {
        raw.parse::<i128>()
            .unwrap_or_else(|err| panic!("bad {key}={raw}: {err}"))
    })
}

fn negative_success_route_ids(rows: &[Value]) -> BTreeSet<u64> {
    rows.iter()
        .filter(|row| {
            row.get("revm_result").and_then(Value::as_str) == Some("success")
                && row.get("reject_reason").and_then(Value::as_str) == Some("none")
                && decimal_i128(row, "net_profit_wei").is_some_and(|net| net < 0)
        })
        .map(route_id)
        .collect()
}

fn rejected_net_negative_route_ids(rows: &[Value]) -> BTreeSet<u64> {
    rows.iter()
        .filter(|row| {
            row.get("revm_result").and_then(Value::as_str) == Some("success")
                && row
                    .get("reject_reason")
                    .and_then(Value::as_str)
                    .is_some_and(|reason| reason.starts_with("net_negative_after_gas"))
                && decimal_i128(row, "net_profit_wei").is_some_and(|net| net < 0)
        })
        .map(route_id)
        .collect()
}

fn artifact_path(artifact: &Value, leg: &str, key: &str) -> PathBuf {
    workspace_path(str_field(obj(artifact, leg), key).as_str())
}

#[test]
fn stale_matrix_is_superseded_not_current_offchain_net_gate_verdict() {
    let matrix = read_json(MATRIX);
    assert_eq!(
        sha256_file(Path::new(MATRIX)),
        "f3258b5706f680242031f9782100595ff7cf91fe1f0b2271c9fc2301344503ac",
        "update deliberately if the stale off-chain net-gate evidence matrix is regenerated"
    );

    let verdict = obj(&matrix, "verdict");
    assert!(!bool_field(verdict, "offchain_measured_net_gate_certified"));
    assert!(bool_field(
        verdict,
        "would_submit_trace_contains_negative_measured_net_routes"
    ));
    assert_eq!(u64_field(verdict, "dry_run_submitted_counter"), 29);

    let routes = obj(verdict, "negative_success_unique_routes")
        .as_array()
        .expect("routes array")
        .iter()
        .map(|value| value.as_u64().expect("route u64"))
        .collect::<Vec<_>>();
    assert_eq!(routes, vec![16803, 21919]);

    let classifications = obj(verdict, "classification").to_string();
    assert!(classifications.contains("AWS is not required"));
    assert!(classifications.contains("burns gas"));

    let master = read_json(MASTER);
    let snapshot = obj(&master, "latest_red_artifact_snapshot");
    assert_eq!(
        str_field(snapshot, "offchain_net_gate_matrix"),
        "Codex/OFFCHAIN_NET_GATE_CURRENT_EVIDENCE_2026-06-15.json"
    );
    assert!(
        str_field(snapshot, "stale_artifact_release_implication")
            .contains("not enough to certify or condemn the newer dirty source"),
        "master matrix must keep the old red trace classified as stale evidence, not current source verdict"
    );
}

#[test]
fn fresh_current_source_reruns_reject_measured_negative_successes() {
    let a = read_json(A_RERUN);
    let b = read_json(B_RERUN);
    let expected = [16803u64, 21919u64].into_iter().collect::<BTreeSet<_>>();

    let a_leg = obj(&a, "a_file_leg");
    assert_eq!(
        u64_field(a_leg, "clean_accepts_with_non_positive_net_profit_wei"),
        0
    );
    let a_rows = obj(a_leg, "net_negative_after_gas_rows")
        .as_array()
        .expect("a net_negative_after_gas_rows");
    assert_eq!(
        a_rows.iter().map(route_id).collect::<BTreeSet<_>>(),
        expected
    );

    let b_leg = obj(&b, "b_binary_leg");
    assert_eq!(
        u64_field(b_leg, "clean_accepts_with_non_positive_net_profit_wei"),
        0
    );
    assert_eq!(
        u64_field(obj(b_leg, "reject_class_counts"), "net_negative_after_gas"),
        2
    );
    assert_eq!(u64_field(obj(b_leg, "reject_class_counts"), "none"), 27);
    let b_rows = obj(b_leg, "net_negative_after_gas_rows")
        .as_array()
        .expect("b net_negative_after_gas_rows");
    assert_eq!(
        b_rows.iter().map(route_id).collect::<BTreeSet<_>>(),
        expected
    );

    for row in a_rows.iter().chain(b_rows.iter()) {
        let net = str_field(row, "net_profit_wei")
            .parse::<i128>()
            .expect("net_profit_wei");
        assert!(
            net < 0,
            "net_negative_after_gas row must be measured negative: {row}"
        );
        assert!(
            str_field(row, "reject_reason").starts_with("net_negative_after_gas"),
            "measured-negative success must carry net_negative_after_gas reject reason: {row}"
        );
    }
}

#[test]
fn fresh_candidate_jsonl_has_no_accepted_net_negative_success_rows() {
    let a = read_json(A_RERUN);
    let b = read_json(B_RERUN);
    let expected = [16803u64, 21919u64].into_iter().collect::<BTreeSet<_>>();

    for (transport, rows) in [
        (
            "A_file",
            jsonl_rows(artifact_path(&a, "a_file_leg", "candidate_file")),
        ),
        (
            "B_binary",
            jsonl_rows(artifact_path(&b, "b_binary_leg", "candidate_file")),
        ),
    ] {
        let accepted_negative_routes = negative_success_route_ids(&rows);
        assert_eq!(
            accepted_negative_routes,
            BTreeSet::new(),
            "{transport} must not contain accepted success/none rows with net_profit_wei < 0"
        );
        let rejected_negative_routes = rejected_net_negative_route_ids(&rows);
        assert_eq!(
            rejected_negative_routes, expected,
            "{transport} must reject the known measured-negative contract-success routes"
        );
        let success_rows = rows
            .iter()
            .filter(|row| row.get("revm_result").and_then(Value::as_str) == Some("success"))
            .count();
        assert_eq!(success_rows, 29);
    }
}

#[test]
fn current_source_rejects_net_negative_before_validated_candidate_handoff() {
    let matrix = read_json(MATRIX);
    let files = obj(&matrix, "files");
    let pipeline = read(workspace_path(&str_field(
        obj(files, "pipeline_rs"),
        "path",
    )));
    let contract = read(workspace_path(&str_field(
        obj(files, "mantis_executor_sol"),
        "path",
    )));

    assert!(pipeline.contains("fn candidate_net_profit_eth_wei"));
    assert!(pipeline.contains("saturating_sub(gas_wei as i128)"));
    assert!(pipeline.contains("fn revm_capital_verdict"));
    assert!(pipeline.contains("let net_positive = net_eth.is_some_and(|n| n > 0);"));
    assert!(pipeline.contains("if contract_success && net_positive"));
    assert!(pipeline.contains("net_negative_after_gas net_wei={n}"));
    assert!(
        pipeline
            .find("let (accept, reject_reason) = revm_capital_verdict")
            .expect("capital verdict call")
            < pipeline
                .find("if pass {")
                .expect("validated candidate branch"),
        "current source must compute net-gated capital verdict before ValidatedCandidate handoff"
    );
    assert!(pipeline.contains("outcome.success"));
    assert!(pipeline.contains("if pass {"));
    // The handoff push moved to a counted-overflow form (`if producer\n .push(ValidatedCandidate`,
    // Codex 2026-07-10 vc-ring accounting) — probe the call itself, format-agnostic.
    assert!(pipeline.contains(".push(ValidatedCandidate"));
    assert!(pipeline.contains("submitted.fetch_add"));

    assert!(contract.contains("revert Loss"));
    assert!(contract.contains("revert InsufficientProfit"));
    assert!(contract.contains("_minNetProfit = minNetProfit"));
}
