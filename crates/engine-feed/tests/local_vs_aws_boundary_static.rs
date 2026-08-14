use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

const MATRIX: &str = "Codex/LOCAL_VS_AWS_CERTIFICATION_BOUNDARY_2026-06-15.json";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

fn workspace_path(path: &str) -> PathBuf {
    repo_root().join(path)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn matrix() -> Value {
    serde_json::from_str(&read(&workspace_path(MATRIX))).expect("matrix json")
}

fn array_len(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array {key}"))
        .len()
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string {key}"))
}

#[test]
fn local_boundary_keeps_correctness_work_on_laptop() {
    let matrix = matrix();
    assert_eq!(
        str_field(&matrix["assumptions"], "local_environment"),
        "developer laptop on local network; sequencer/local-network latency can exceed 1s"
    );
    assert!(
        str_field(&matrix["assumptions"], "local_latency_rule").contains("correctness proof only")
    );
    assert!(
        array_len(&matrix, "local_can_certify_now") >= 8,
        "local certification list should cover kernel, route, gas, replay, schema, submit, and Dune work"
    );
    assert!(
        array_len(&matrix, "local_can_fix_if_production_edits_are_authorized") >= 5,
        "local-fix list should cover candidate schema/economics and replay regeneration"
    );
}

#[test]
fn aws_boundary_is_limited_to_runtime_latency_submit_and_deploy_proof() {
    let matrix = matrix();
    assert!(
        array_len(&matrix, "aws_or_colocated_required_for_final_certification") >= 5,
        "AWS boundary should cover feed freshness, latency, live submit, runtime privileges, and deploy"
    );

    let aws = matrix["aws_or_colocated_required_for_final_certification"]
        .as_array()
        .expect("aws array");
    let text = serde_json::to_string(aws).expect("aws text");
    for required in [
        "production feed freshness",
        "production end-to-end latency",
        "live sequencer submit path",
        "kernel/network/runtime privileges",
        "AWS deploy scripts",
    ] {
        assert!(
            text.contains(required),
            "missing AWS boundary area {required}"
        );
    }
}

#[test]
fn aws_is_not_next_for_money_path_until_local_correctness_is_green() {
    let matrix = matrix();
    let verdict = &matrix["current_boundary_verdict"];
    assert!(
        str_field(verdict, "aws_not_next_for_money_path").contains("AWS should wait"),
        "money-path correctness must remain local-first"
    );
    assert!(
        str_field(verdict, "aws_next_for_latency_only").contains("latency/feed/submit/deploy"),
        "AWS should be framed as latency/runtime proof only while correctness remains red"
    );
}
