use serde_json::Value;
use std::{collections::BTreeSet, fs, path::Path};

const REQUIREMENTS: &str =
    "../../../Codex/DRYRUN_WOULD_SUBMIT_ARTIFACT_REQUIREMENTS_2026-06-14.json";

fn read_required(path: impl AsRef<Path>, label: &str) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("failed to read {label} at {}: {err}", path.display());
    })
}

fn read_json(path: impl AsRef<Path>, label: &str) -> Value {
    let raw = read_required(path, label);
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("{label} is invalid JSON: {err}"))
}

fn string_set(value: &Value, key: &str) -> BTreeSet<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array field {key} in {value}"))
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .unwrap_or_else(|| panic!("non-string entry in {key}: {entry}"))
                .to_string()
        })
        .collect()
}

fn nested_string_set(value: &Value, outer: &str, inner: &str) -> BTreeSet<String> {
    let object = value
        .get(outer)
        .unwrap_or_else(|| panic!("missing object field {outer} in {value}"));
    string_set(object, inner)
}

fn read_jsonl(path: impl AsRef<Path>, label: &str) -> Vec<Value> {
    let path = path.as_ref();
    let raw = read_required(path, label);
    let rows = raw
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(idx, line)| {
            serde_json::from_str::<Value>(line).unwrap_or_else(|err| {
                panic!(
                    "{label} line {} at {} is not valid JSON: {err}",
                    idx + 1,
                    path.display()
                );
            })
        })
        .collect::<Vec<_>>();
    assert!(!rows.is_empty(), "{label} must not be empty");
    rows
}

fn decimal_string(value: &Value, field: &str) -> bool {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(|s| !s.is_empty() && s.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or(false)
}

fn route_id(value: &Value) -> Option<u64> {
    value.get("route_id").and_then(Value::as_u64)
}

#[test]
fn dryrun_harness_keeps_submit_disabled_and_artifacts_enabled() {
    let script = read_required("../../scripts/dryrun_3h.sh", "3h dry-run harness");

    for required in [
        "DRY_RUN=true",
        "SUBMIT_TX=false",
        "REVM_VALIDATE=1",
        "PAPER_MODE_LOG=$PAPER",
        "CANDIDATE_RECORD_JSONL=$CANDREC",
        "LOG_DIR=$RUNDIR",
        "\"$ENGINE_BIN\" dry-run",
        "dry-run pipeline live",
    ] {
        assert!(
            script.contains(required),
            "dry-run harness missing required contract fragment: {required}"
        );
    }

    assert!(
        !script.contains("SUBMIT_TX=true"),
        "dry-run harness must not enable live submit"
    );
}

#[test]
fn dryrun_would_submit_requirements_match_harness_and_forbid_broadcast() {
    let requirements = read_json(REQUIREMENTS, "dry-run would-submit requirements");
    let script = read_required("../../scripts/dryrun_3h.sh", "3h dry-run harness");

    assert_eq!(
        requirements["required_invocation"]["script"].as_str(),
        Some("scripts/dryrun_3h.sh"),
        "dry-run requirements should point at the local harness"
    );
    assert_eq!(
        requirements["required_invocation"]["default_live_window_secs"].as_u64(),
        Some(10_800),
        "dry-run default live window drifted"
    );
    assert_eq!(
        requirements["required_invocation"]["startup_wait_separate_from_live_window"].as_bool(),
        Some(true),
        "startup wait must remain separate from live run duration"
    );
    assert!(
        script.contains(": \"${RUNDIR:?"),
        "dry-run harness should require an explicit RUNDIR"
    );

    let required_files = string_set(&requirements, "required_files");
    assert_eq!(
        required_files,
        BTreeSet::from([
            "candidate_records.jsonl".to_string(),
            "engine.stderr.log".to_string(),
            "engine.stdout.log".to_string(),
            "live_start".to_string(),
            "paper_candidates.jsonl".to_string(),
            "sidecar.stderr.log".to_string(),
            "sidecar.stdout.log".to_string(),
            "status".to_string(),
        ]),
        "dry-run required artifact file set drifted"
    );

    for fragment in string_set(&requirements, "required_env") {
        let current_fragment = match fragment.as_str() {
            "PAPER_MODE_LOG=\"$PAPER\"" => "PAPER_MODE_LOG=$PAPER",
            "CANDIDATE_RECORD_JSONL=\"$CANDREC\"" => "CANDIDATE_RECORD_JSONL=$CANDREC",
            "LOG_DIR=\"$RUNDIR\"" => "LOG_DIR=$RUNDIR",
            "RELAY_ENDPOINT=\"binunix:$SOCK\"" => "RELAY_ENDPOINT=binunix:$SOCKET_PATH",
            current => current,
        };
        assert!(
            script.contains(current_fragment),
            "dry-run harness missing current semantic env fragment for requirement: {fragment}"
        );
    }
    for marker in string_set(&requirements, "required_markers") {
        match marker.as_str() {
            "dry-run pipeline live" => assert!(
                script.contains(&marker),
                "dry-run harness missing required marker contract: {marker}"
            ),
            "REVM route validation ACTIVE" => assert!(
                script.contains("REVM_VALIDATE=1"),
                "dry-run harness must enable REVM validation to produce marker: {marker}"
            ),
            "OPP_LATENCY" => assert!(
                script.contains("PAPER_MODE_LOG=$PAPER")
                    && script.contains("CANDIDATE_RECORD_JSONL=$CANDREC"),
                "dry-run harness must enable paper and candidate artifacts to produce marker: {marker}"
            ),
            other => panic!("unexpected dry-run required marker {other}"),
        }
    }

    assert!(
        script.contains("PAPER_MODE_LOG=$PAPER"),
        "paper-mode artifact must stay enabled"
    );
    assert!(
        script.contains("CANDIDATE_RECORD_JSONL=$CANDREC"),
        "candidate record artifact must stay enabled"
    );
    assert!(
        script.contains("REVM_VALIDATE=1"),
        "REVM validation must stay enabled for would-submit proof"
    );
    assert!(
        script.contains("SUBMIT_TX=false"),
        "dry-run harness must keep live transaction submission disabled"
    );

    for forbidden in string_set(&requirements, "forbidden_broadcast_markers") {
        if forbidden == "SUBMIT_TX=true" {
            assert!(
                !script.contains(&forbidden),
                "dry-run harness must never enable {forbidden}"
            );
        }
    }

    let candidate_fields = nested_string_set(
        &requirements,
        "candidate_record_requirements",
        "required_fields",
    );
    for field in [
        "route_id",
        "revm_enabled",
        "revm_result",
        "input_amount",
        "quote_output",
        "gross_profit_wei",
        "tokens",
        "pools",
    ] {
        assert!(
            candidate_fields.contains(field),
            "candidate requirements missing field {field}"
        );
    }
    assert_eq!(
        requirements["candidate_record_requirements"]["must_include_success_or_loss"].as_bool(),
        Some(true),
        "candidate artifact must include at least one success/loss REVM classification"
    );
    let paper_fields = nested_string_set(
        &requirements,
        "paper_record_requirements",
        "required_fields",
    );
    for field in [
        "timestamp_ns",
        "block_number_seen",
        "route_id",
        "amount_in",
        "gross_profit_wei",
        "pools",
    ] {
        assert!(
            paper_fields.contains(field),
            "paper requirements missing field {field}"
        );
    }
    assert_eq!(
        requirements["join_requirements"]
            ["paper_and_candidate_records_share_at_least_one_route_id"]
            .as_bool(),
        Some(true),
        "paper and candidate artifacts must join on route_id"
    );
}
