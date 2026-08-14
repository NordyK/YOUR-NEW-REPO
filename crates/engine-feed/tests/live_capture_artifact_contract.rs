#![allow(clippy::double_ended_iterator_last, clippy::bool_assert_comparison)] // test code; cosmetic

use serde_json::Value;
use std::{collections::BTreeSet, fs, path::PathBuf};

const REQUIREMENTS: &str =
    "../../../Codex/GATE8_LIVE_CAPTURE_ARTIFACT_REQUIREMENTS_2026-06-14.json";
const LIVE_ATTEMPT_STATUS: &str = "../../../Codex/GATE8_LIVE_ATTEMPT_STATUS_2026-06-14.json";
const EVENT_AGE_BLOCKER: &str = "../../../Codex/GATE8_EVENT_AGE_FRESHNESS_BLOCKER_2026-06-14.json";

fn read_required(path: PathBuf, label: &str) -> String {
    fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("failed to read {label} at {}: {err}", path.display());
    })
}

fn count_nonempty_lines(path: PathBuf, label: &str) -> usize {
    let content = read_required(path, label);
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn read_json(path: PathBuf, label: &str) -> Value {
    let raw = read_required(path, label);
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("{label} is invalid JSON: {err}"))
}

fn string_array(value: &Value, key: &str) -> BTreeSet<String> {
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

fn nested_string_array(value: &Value, outer: &str, inner: &str) -> BTreeSet<String> {
    let object = value
        .get(outer)
        .unwrap_or_else(|| panic!("missing object field {outer} in {value}"));
    string_array(object, inner)
}

fn repo_root_path(relative_to_repo: &str) -> PathBuf {
    PathBuf::from("../../../").join(relative_to_repo)
}

fn jsonl_values(path: PathBuf, label: &str) -> Vec<Value> {
    let raw = read_required(path, label);
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{label} contains invalid JSONL row: {err}"))
        })
        .collect()
}

fn required_u64(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing u64 field {key} in {value}"))
}

fn required_bool(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("missing bool field {key} in {value}"))
}

fn non_null_field_count(rows: &[Value], key: &str) -> usize {
    rows.iter()
        .filter(|row| row.get(key).is_some_and(|value| !value.is_null()))
        .count()
}

fn sidecar_stderr_satisfies_fragment(stderr: &str, fragment: &str) -> bool {
    stderr.contains(fragment)
        || (fragment.starts_with("listening on ")
            && stderr.contains("relay_sidecar: listening on "))
}

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_idx = source
        .find(start)
        .unwrap_or_else(|| panic!("source missing section start: {start}"));
    let tail = &source[start_idx..];
    let end_idx = tail
        .find(end)
        .unwrap_or_else(|| panic!("source missing section end after {start}: {end}"));
    &tail[..end_idx]
}

fn latest_latency_line<'a>(log: &'a str, stage: &str) -> &'a str {
    let needle = format!("stage=\"{stage}\"");
    log.lines()
        .filter(|line| line.contains("latency elapsed_s=300") && line.contains(&needle))
        .last()
        .unwrap_or_else(|| panic!("missing elapsed_s=300 latency line for stage {stage}"))
}

fn latency_field(line: &str, key: &str) -> u64 {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("latency line missing numeric {key}: {line}"))
}

fn named_attempt<'a>(status: &'a Value, name: &str) -> &'a Value {
    status["attempts"]
        .as_array()
        .unwrap_or_else(|| panic!("Gate 8 status missing attempts array: {status}"))
        .iter()
        .find(|attempt| attempt["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("Gate 8 status missing attempt {name}"))
}

#[test]
fn gate8_event_age_freshness_blocker_matches_binary_run_and_source() {
    let blocker = read_json(PathBuf::from(EVENT_AGE_BLOCKER), "Gate 8 event-age blocker");
    assert_eq!(
        blocker["classification"].as_str(),
        Some("live_freshness_metrics_missing"),
        "Gate 8 event-age blocker classification drifted"
    );
    assert_eq!(
        blocker["production_logic_changed"].as_bool(),
        Some(false),
        "event-age blocker must not claim production logic edits"
    );
    assert_eq!(
        blocker["verdict"].as_str(),
        Some("Gate8 remains red for live freshness: the completed 300s binary run proves feed/candidate activity, but it does not persist p95/p99/max sequencer-to-engine event age."),
        "event-age blocker verdict changed; update the verifier with evidence"
    );

    let raw = &blocker["raw_capture"];
    // Worktree-portable: the raw feed capture is a gitignored (.gitignore:51
    // **/raw_feed_capture.jsonl), regenerable live-run artifact. This test cross-checks its row
    // count, top-level schema, message count, and sequence/timestamp bounds against the committed
    // blocker JSON; it does NOT bind the raw sha256 or byte size (the blocker JSON records none), so
    // the skip below is skip-on-absence for portability — NOT because a byte-exact artifact is
    // required. When it is absent (fresh checkout / clean-environment gate) skip LOUDLY so a gate can
    // detect it — never a silent pass.
    let raw_capture_path = repo_root_path(
        raw["file"]
            .as_str()
            .expect("event-age blocker missing raw_capture.file"),
    );
    if !raw_capture_path.exists() {
        eprintln!(
            "SKIPPED: artifact absent: {} (gitignored ~10MB live-run raw feed capture) — Gate 8 \
             event-age blocker cross-check requires the recorded certification-run raw capture",
            raw_capture_path.display()
        );
        return;
    }
    let raw_rows = jsonl_values(raw_capture_path, "Gate 8 raw feed capture");
    assert_eq!(
        raw_rows.len() as u64,
        required_u64(raw, "rows"),
        "raw feed capture row count drifted"
    );
    let top_level_keys = raw_rows
        .first()
        .and_then(Value::as_object)
        .map(|object| object.keys().cloned().collect::<BTreeSet<_>>())
        .expect("raw feed capture first row must be an object");
    assert_eq!(
        top_level_keys,
        string_array(raw, "top_level_keys"),
        "raw feed capture top-level schema drifted"
    );
    assert_eq!(
        raw_rows.iter().any(|row| row.get("recv_ns").is_some()),
        required_bool(raw, "top_level_has_recv_ns"),
        "raw capture recv_ns presence changed"
    );
    assert_eq!(
        raw_rows
            .iter()
            .any(|row| row.get("capture_recv_ns").is_some()),
        required_bool(raw, "top_level_has_capture_recv_ns"),
        "raw capture capture_recv_ns presence changed"
    );
    assert_eq!(
        raw_rows.iter().any(|row| row.get("event_age_ms").is_some()),
        required_bool(raw, "top_level_has_event_age_ms"),
        "raw capture event_age_ms presence changed"
    );

    let mut sequences = Vec::new();
    let mut timestamps = Vec::new();
    for row in &raw_rows {
        if let Some(messages) = row.get("messages").and_then(Value::as_array) {
            for message in messages {
                if let Some(seq) = message.get("sequenceNumber").and_then(Value::as_u64) {
                    sequences.push(seq);
                }
                if let Some(ts) = message
                    .pointer("/message/message/header/timestamp")
                    .and_then(Value::as_u64)
                {
                    timestamps.push(ts);
                }
            }
        }
    }
    assert_eq!(
        sequences.len() as u64,
        required_u64(raw, "message_count"),
        "raw feed message count drifted"
    );
    assert_eq!(
        sequences.iter().copied().min(),
        Some(required_u64(raw, "sequence_min")),
        "raw feed sequence_min drifted"
    );
    assert_eq!(
        sequences.iter().copied().max(),
        Some(required_u64(raw, "sequence_max")),
        "raw feed sequence_max drifted"
    );
    assert_eq!(
        timestamps.iter().copied().min(),
        Some(required_u64(raw, "timestamp_min")),
        "raw feed timestamp_min drifted"
    );
    assert_eq!(
        timestamps.iter().copied().max(),
        Some(required_u64(raw, "timestamp_max")),
        "raw feed timestamp_max drifted"
    );
    assert_eq!(
        !sequences.is_empty(),
        required_bool(raw, "contains_sequence_number"),
        "raw feed sequence-number evidence drifted"
    );
    assert_eq!(
        !timestamps.is_empty(),
        required_bool(raw, "contains_sequencer_header_timestamp"),
        "raw feed header timestamp evidence drifted"
    );
    assert!(
        !required_bool(raw, "contains_capture_recv_ns"),
        "current blocker must stay red until raw capture persists host receive time"
    );
    assert!(
        !required_bool(raw, "contains_event_age_metrics"),
        "current blocker must stay red until raw capture or logs persist event-age metrics"
    );

    let log = &blocker["engine_log_latency"];
    let engine_log = read_required(
        repo_root_path(
            log["file"]
                .as_str()
                .expect("event-age blocker missing engine_log_latency.file"),
        ),
        "Gate 8 engine live stdout",
    );
    assert_eq!(
        engine_log.contains("stage=\"feed_seq_to_recv_ns\""),
        required_bool(log, "contains_feed_seq_to_recv_ns"),
        "feed_seq_to_recv_ns log presence drifted"
    );
    for (stage, count_key, p99_key, max_key) in [
        (
            "frame_parse_ns",
            "latest_frame_parse_count",
            "latest_frame_parse_p99_ns",
            "latest_frame_parse_max_ns",
        ),
        (
            "recv_to_classify_ns",
            "latest_recv_to_classify_count",
            "latest_recv_to_classify_p99_ns",
            "latest_recv_to_classify_max_ns",
        ),
        (
            "revm_ns",
            "latest_revm_count",
            "latest_revm_p99_ns",
            "latest_revm_max_ns",
        ),
        (
            "alchemy_rpc_rtt_ns",
            "latest_alchemy_rpc_rtt_count",
            "latest_alchemy_rpc_rtt_p99_ns",
            "latest_alchemy_rpc_rtt_max_ns",
        ),
    ] {
        let line = latest_latency_line(&engine_log, stage);
        assert_eq!(
            latency_field(line, "elapsed_s"),
            required_u64(log, "latest_elapsed_s"),
            "latest elapsed_s drifted for stage {stage}"
        );
        assert_eq!(
            latency_field(line, "count"),
            required_u64(log, count_key),
            "latest count drifted for stage {stage}"
        );
        assert_eq!(
            latency_field(line, "p99"),
            required_u64(log, p99_key),
            "latest p99 drifted for stage {stage}"
        );
        assert_eq!(
            latency_field(line, "max"),
            required_u64(log, max_key),
            "latest max drifted for stage {stage}"
        );
    }

    let status_trace = &blocker["status_artifact_trace"];
    let status = read_json(
        repo_root_path(
            status_trace["file"]
                .as_str()
                .expect("event-age blocker missing status_artifact_trace.file"),
        ),
        "Gate 8 live attempt status",
    );
    assert_eq!(
        status["gate_result"].as_str(),
        status_trace["gate_result"].as_str(),
        "Gate 8 status gate_result drifted"
    );
    assert_eq!(
        status["latest_attempt_result"].as_str(),
        status_trace["latest_attempt_result"].as_str(),
        "Gate 8 status latest_attempt_result drifted"
    );
    let classifications = string_array(&status, "classification");
    assert_eq!(
        classifications.contains("event_age_metrics_missing_latest"),
        required_bool(
            status_trace,
            "classification_contains_event_age_metrics_missing_latest"
        ),
        "Gate 8 status event-age blocker classification drifted"
    );
    let latest_attempt = named_attempt(
        &status,
        status_trace["latest_attempt"]
            .as_str()
            .expect("event-age blocker missing latest_attempt"),
    );
    assert_eq!(
        latest_attempt["result"].as_str(),
        status_trace["latest_attempt_result"].as_str(),
        "Gate 8 latest attempt result drifted"
    );

    let source = &blocker["source_trace"];
    let relay_client = read_required(
        repo_root_path("engine/crates/engine-feed/src/relay_client.rs"),
        "relay client source",
    );
    assert_eq!(
        relay_client.contains("pub recv_ns: u64"),
        required_bool(source, "relay_callback_recv_ns_available"),
        "RelayCallback recv_ns availability drifted"
    );

    let relay_sidecar = read_required(
        repo_root_path("engine/crates/engine/src/bin/relay_sidecar.rs"),
        "relay sidecar source",
    );
    assert!(
        relay_sidecar.contains("c.raw.write_all(frame)")
            && relay_sidecar.contains("c.raw.write_all(b\"\\n\")")
            && relay_sidecar.contains("raw.write_all(&slot.raw)")
            && relay_sidecar.contains("raw.write_all(b\"\\n\")"),
        "sync and async capture modes must both persist the unmodified frame plus one newline"
    );
    assert!(
        relay_sidecar.contains("capture.capture_raw_frame(cb.frame)")
            && !relay_sidecar.contains("capture.capture_raw_frame(cb.recv_ns"),
        "relay_sidecar raw capture must not prefix callback recv_ns"
    );
    assert_eq!(
        relay_sidecar.contains("let recv_ns = now_ns();")
            && relay_sidecar.contains("encode_frame_v2("),
        required_bool(source, "sidecar_binary_emit_stamps_recv_ns_with_now_ns"),
        "relay_sidecar binary emit recv_ns stamping drifted"
    );

    let feed_frame = read_required(
        repo_root_path("engine/crates/engine-feed/src/feed_frame.rs"),
        "feed frame source",
    );
    let frame_view = source_section(&feed_frame, "pub struct FeedFrameView", "/// V1 encoder");
    assert_eq!(
        frame_view.contains("pub seq: u64"),
        required_bool(source, "binary_frame_carries_sequence_number"),
        "FeedFrameView sequence field drifted"
    );
    assert_eq!(
        frame_view.contains("pub recv_ns: u64"),
        required_bool(source, "binary_frame_carries_recv_ns"),
        "FeedFrameView recv_ns field drifted"
    );
    assert_eq!(
        frame_view.contains("pub timestamp") || frame_view.contains("pub header_timestamp"),
        required_bool(source, "binary_frame_carries_sequencer_header_timestamp"),
        "FeedFrameView timestamp field drifted"
    );

    let pipeline = read_required(
        repo_root_path("engine/crates/engine/src/pipeline.rs"),
        "pipeline source",
    );
    assert_eq!(
        pipeline.contains("(StageLatency::FeedLatency, \"feed_event_age_seconds\")"),
        required_bool(
            source,
            "engine_latency_stage_name_feed_seq_to_recv_ns_defined"
        ),
        "pipeline feed latency stage name drifted"
    );
    assert_eq!(
        pipeline.contains("latency.record(StageLatency::FeedLatency"),
        required_bool(
            source,
            "json_ws_parser_records_feed_latency_from_recv_ns_and_header_timestamp"
        ),
        "JSON parser feed-latency recording evidence drifted"
    );
    let binary_rx = source_section(
        &pipeline,
        "fn rx_loop_unix_binary",
        "fn parsed_frame_from_signed_envelope",
    );
    assert_eq!(
        binary_rx.contains("latency.record(StageLatency::FrameParse"),
        required_bool(source, "binary_rx_records_frame_parse_latency"),
        "binary rx frame-parse latency evidence drifted"
    );
    assert_eq!(
        binary_rx.contains("StageLatency::FeedLatency"),
        required_bool(source, "binary_rx_records_feed_latency"),
        "binary rx feed-latency recording evidence drifted"
    );
    assert_eq!(
        engine_log.contains("stage=\"feed_seq_to_recv_ns\""),
        required_bool(source, "engine_300s_log_emitted_feed_seq_to_recv_ns"),
        "300s Gate 8 log feed-latency evidence drifted"
    );

    let missing = string_array(&blocker["certification_meaning"], "what_is_not_proven");
    for metric in ["max_event_age_ms", "p95_event_age_ms", "p99_event_age_ms"] {
        assert!(
            missing.contains(metric),
            "event-age blocker missing not-proven metric {metric}"
        );
    }
}

#[test]
fn gate8_live_capture_requirements_are_latency_aware_and_match_harness() {
    let requirements = read_json(
        PathBuf::from(REQUIREMENTS),
        "Gate 8 live capture requirements",
    );
    let script = read_required(
        PathBuf::from("../../scripts/gate8_binary_live.sh"),
        "Gate 8 binary-live harness",
    );

    assert_eq!(
        requirements["environment"].as_str(),
        Some("local laptop local network"),
        "Gate 8 requirements must preserve the local laptop latency boundary"
    );

    let required_files = string_array(&requirements, "required_files");
    assert_eq!(
        required_files,
        BTreeSet::from([
            "candidate_records.jsonl".to_string(),
            "engine_live_stderr.log".to_string(),
            "engine_live_stdout.log".to_string(),
            "raw_feed_capture.jsonl".to_string(),
            "relay_sidecar_stderr.log".to_string(),
            "relay_sidecar_stdout.log".to_string(),
        ]),
        "Gate 8 required artifact file set drifted"
    );

    for fragment in string_array(&requirements, "required_env") {
        assert!(
            script.contains(&fragment),
            "Gate 8 harness missing env fragment from requirements: {fragment}"
        );
    }

    for marker in string_array(&requirements, "required_markers") {
        assert!(
            script.contains(&marker) || marker == "REVM route validation ACTIVE",
            "Gate 8 harness missing live marker from requirements: {marker}"
        );
    }

    assert!(
        script.contains("--capture-jsonl \"$GATE8_RAW_CAPTURE_JSONL\""),
        "Gate 8 harness must persist raw relay feed frames"
    );
    assert!(
        script.contains("CANDIDATE_RECORD_JSONL=\"$GATE8_CANDIDATES_JSONL\""),
        "Gate 8 harness must persist candidate records"
    );
    assert!(
        script.contains("LIVE_WINDOW_SECS=$LIVE_WINDOW"),
        "Gate 8 harness must report the measured live ingestion window"
    );
    assert!(
        script.contains("STARTUP_MAX"),
        "Gate 8 harness must keep startup wait separate from live ingestion time"
    );
    assert!(
        script.contains("ENG_PID=\"\"") && script.contains("SIDE_PID=\"\""),
        "Gate 8 harness must initialize child PIDs before cleanup can run"
    );
    assert!(
        script.contains("if [ -n \"${ENG_PID:-}\" ]")
            && script.contains("if [ -n \"${SIDE_PID:-}\" ]"),
        "Gate 8 cleanup must guard unset child PIDs so startup failures preserve the real blocker"
    );

    let latency_metrics = nested_string_array(&requirements, "latency_policy", "required_metrics");
    for metric in [
        "raw_frame_count",
        "candidate_count",
        "live_window_secs",
        "startup_wait_secs",
        "max_event_age_ms",
        "p95_event_age_ms",
        "p99_event_age_ms",
    ] {
        assert!(
            latency_metrics.contains(metric),
            "Gate 8 latency policy missing metric {metric}"
        );
    }
    assert_eq!(
        requirements["latency_policy"]["sequencer_latency_spikes_over_1s"].as_str(),
        Some("allowed_if_measured"),
        "local >1s sequencer/feed spikes must be allowed only when measured"
    );
    assert_eq!(
        requirements["latency_policy"]["classification_when_artifacts_missing"].as_str(),
        Some("live_artifact_missing"),
        "missing Gate8 artifacts must stay a red proof blocker"
    );
}

#[test]
fn gate8_live_attempt_status_records_socket_bind_blocker() {
    let status = read_json(
        PathBuf::from(LIVE_ATTEMPT_STATUS),
        "Gate 8 live attempt status",
    );
    let requirements = read_json(
        PathBuf::from(REQUIREMENTS),
        "Gate 8 live capture requirements",
    );

    // Worktree-portable: several attempts reference gitignored (.gitignore:51
    // **/raw_feed_capture.jsonl), regenerable live-run raw captures. This test cross-checks their
    // non-empty row counts against the committed status JSON; it does NOT bind the raw sha256 or byte
    // size, so the skip below is skip-on-absence for portability — NOT because a byte-exact artifact
    // is required. When any referenced raw capture is absent (fresh checkout / clean-environment
    // gate) skip LOUDLY so a gate can detect it — never a silent pass.
    if let Some(attempts) = status["attempts"].as_array() {
        for attempt in attempts {
            let (Some(run_dir), Some(raw_file)) = (
                attempt["run_dir"].as_str(),
                attempt
                    .get("raw_capture")
                    .and_then(|c| c.get("file"))
                    .and_then(Value::as_str),
            ) else {
                continue;
            };
            let raw_capture_path = repo_root_path(run_dir).join(raw_file);
            if !raw_capture_path.exists() {
                eprintln!(
                    "SKIPPED: artifact absent: {} (gitignored ~10MB live-run raw feed capture) — \
                     Gate 8 live-attempt status cross-check requires the recorded certification-run \
                     raw captures",
                    raw_capture_path.display()
                );
                return;
            }
        }
    }

    assert_eq!(
        requirements["current_status"]["latest_live_attempt_status"].as_str(),
        Some("Codex/GATE8_LIVE_ATTEMPT_STATUS_2026-06-14.json"),
        "Gate 8 requirements must point at the latest live-attempt status artifact"
    );
    assert_eq!(
        requirements["current_status"]["latest_live_attempt_result"].as_str(),
        Some("live_window_completed_candidate_records_present_no_accepts"),
        "Gate 8 requirements must track the latest 300s run result"
    );
    assert_eq!(
        requirements["current_status"]["latest_money_path_result"].as_str(),
        Some("not_green_validated_zero_submitted_zero_no_positive_candidate_records"),
        "Gate 8 requirements must keep the money-path verdict red"
    );

    assert_eq!(
        status["gate_result"].as_str(),
        Some("not_green"),
        "Gate 8 must stay red until live artifacts prove accepted profitable realization"
    );
    assert_eq!(
        status["exit_code"].as_i64(),
        Some(0),
        "Gate 8 status must preserve the latest long-run harness exit code"
    );
    assert_eq!(
        status["production_logic_changed"].as_bool(),
        Some(false),
        "Gate 8 status must not claim production-logic edits"
    );

    let classifications = string_array(&status, "classification");
    for expected in [
        "sandbox_unix_socket_bind_blocked",
        "startup_live_marker_timeout_after_socket_bind",
        "raw_feed_capture_nonempty_latest",
        "pipeline_live_marker_seen_latest",
        "candidate_records_nonempty_latest",
        "revm_validation_active_latest",
        "no_positive_candidate_records_latest",
        "validated_zero_latest",
        "submitted_zero_latest",
        "revm_fork_suspect_latest",
        "event_age_metrics_missing_latest",
        "artifact_run_dir_reused_latest",
        "short_run_artifact_overwritten_by_later_300s_run",
    ] {
        assert!(
            classifications.contains(expected),
            "Gate 8 status missing blocker classification {expected}"
        );
    }

    let sandbox_attempt = named_attempt(&status, "sandboxed_short_run");
    assert_eq!(
        sandbox_attempt["exit_code"].as_i64(),
        Some(1),
        "sandbox Gate 8 attempt must preserve failed exit code"
    );
    let sandbox_run_dir = sandbox_attempt["run_dir"]
        .as_str()
        .expect("Gate 8 sandbox attempt missing run_dir");
    let sandbox_run_root = repo_root_path(sandbox_run_dir);
    let relay_stderr = read_required(
        sandbox_run_root.join(
            sandbox_attempt["sidecar"]["stderr_file"]
                .as_str()
                .expect("Gate 8 sandbox attempt missing sidecar stderr_file"),
        ),
        "Gate 8 sandbox relay sidecar stderr",
    );
    let expected_bind_error = sandbox_attempt["sidecar"]["stderr_required_fragment"]
        .as_str()
        .expect("Gate 8 sandbox attempt missing sidecar stderr_required_fragment");
    assert!(
        relay_stderr.contains(expected_bind_error),
        "Gate 8 sidecar stderr does not contain the recorded bind blocker: {expected_bind_error}"
    );

    let produced_files = string_array(sandbox_attempt, "produced_files");
    assert_eq!(
        produced_files,
        BTreeSet::from([
            "relay_sidecar_stderr.log".to_string(),
            "relay_sidecar_stdout.log".to_string(),
        ]),
        "Gate 8 failed attempt should only have sidecar logs"
    );
    for file in &produced_files {
        assert!(
            sandbox_run_root.join(file).exists(),
            "Gate 8 status lists produced file that is absent: {file}"
        );
    }

    for missing in string_array(sandbox_attempt, "missing_required_files") {
        assert!(
            !sandbox_run_root.join(&missing).exists(),
            "Gate 8 status says required file is missing but it exists: {missing}"
        );
    }

    let terminal_fragments = string_array(sandbox_attempt, "terminal_stdout_fragments");
    assert!(
        terminal_fragments
            .contains("scripts/gate8_binary_live.sh: line 18: ENG_PID: unbound variable"),
        "Gate 8 status must preserve the harness cleanup failure"
    );

    let escalated_attempt = named_attempt(&status, "escalated_short_run");
    assert_eq!(
        escalated_attempt["exit_code"].as_i64(),
        Some(1),
        "short escalated Gate 8 attempt must preserve failed exit code"
    );
    assert_eq!(
        escalated_attempt["result"].as_str(),
        Some("failed_before_live_marker"),
        "escalated Gate 8 attempt must stay red until the live marker appears"
    );
    let escalated_run_dir = escalated_attempt["run_dir"]
        .as_str()
        .expect("Gate 8 escalated attempt missing run_dir");
    let escalated_run_root = repo_root_path(escalated_run_dir);
    let escalated_relay_stderr = read_required(
        escalated_run_root.join(
            escalated_attempt["sidecar"]["stderr_file"]
                .as_str()
                .expect("Gate 8 escalated attempt missing sidecar stderr_file"),
        ),
        "Gate 8 escalated relay sidecar stderr",
    );
    if let Some(stability) = escalated_attempt.get("artifact_stability") {
        assert_eq!(
            required_bool(
                stability,
                "current_artifacts_do_not_match_recorded_short_attempt"
            ),
            true,
            "Gate 8 status must classify the reused run directory before checking overwritten logs"
        );
        let escalated_terminal_fragments =
            string_array(escalated_attempt, "terminal_stdout_fragments");
        assert!(
            escalated_terminal_fragments.contains("[gate8] sidecar socket up"),
            "historical escalated Gate 8 terminal text must preserve successful socket bind"
        );
        assert!(
            escalated_relay_stderr.contains("relay_sidecar: listening on"),
            "overwritten Gate 8 sidecar stderr should still be a valid later sidecar log"
        );
    } else {
        assert!(
            sidecar_stderr_satisfies_fragment(
                &escalated_relay_stderr,
                escalated_attempt["sidecar"]["stderr_required_fragment"]
                    .as_str()
                    .expect("Gate 8 escalated attempt missing sidecar stderr fragment"),
            ),
            "escalated Gate 8 sidecar did not record successful socket bind"
        );
    }

    let engine_stdout = read_required(
        escalated_run_root.join(
            escalated_attempt["engine"]["stdout_file"]
                .as_str()
                .expect("Gate 8 escalated attempt missing engine stdout file"),
        ),
        "Gate 8 escalated engine stdout",
    );
    let live_marker = escalated_attempt["engine"]["required_live_marker"]
        .as_str()
        .expect("Gate 8 escalated attempt missing live marker");
    assert!(
        engine_stdout.contains("snapshot phase progress"),
        "escalated Gate 8 attempt should preserve startup/snapshot evidence"
    );

    if let Some(stability) = escalated_attempt.get("artifact_stability") {
        assert_eq!(
            required_bool(
                stability,
                "current_artifacts_do_not_match_recorded_short_attempt"
            ),
            true,
            "Gate 8 status must classify the reused run directory"
        );
        assert_eq!(
            engine_stdout.contains(live_marker),
            required_bool(stability, "current_engine_stdout_contains_live_marker"),
            "Gate 8 reused run directory live-marker evidence drifted"
        );
        let driver_log = read_required(
            repo_root_path(
                stability["driver_log"]
                    .as_str()
                    .expect("Gate 8 artifact stability missing driver_log"),
            ),
            "Gate 8 overwritten driver log",
        );
        assert!(
            driver_log.contains(&format!(
                "GATE8_EXIT={}",
                required_u64(stability, "driver_exit_code")
            )),
            "Gate 8 overwritten driver log missing recorded exit code"
        );
        assert!(
            driver_log.contains(&format!(
                "LIVE_WINDOW_SECS={}",
                required_u64(stability, "driver_live_window_secs")
            )),
            "Gate 8 overwritten driver log missing recorded live window"
        );
        assert_eq!(
            count_nonempty_lines(
                escalated_run_root.join(
                    escalated_attempt["raw_capture"]["file"]
                        .as_str()
                        .expect("Gate 8 escalated attempt missing raw capture file")
                ),
                "Gate 8 overwritten raw feed capture",
            ) as u64,
            required_u64(stability, "current_raw_capture_rows"),
            "Gate 8 overwritten raw capture row count drifted"
        );
        let overwritten_candidates_path = escalated_run_root.join(
            escalated_attempt["candidate_capture"]["file"]
                .as_str()
                .expect("Gate 8 escalated attempt missing candidate capture file"),
        );
        let overwritten_candidates = jsonl_values(
            overwritten_candidates_path,
            "Gate 8 overwritten candidate records",
        );
        assert_eq!(
            overwritten_candidates.len() as u64,
            required_u64(stability, "current_candidate_rows"),
            "Gate 8 overwritten candidate row count drifted"
        );
        assert_eq!(
            overwritten_candidates
                .iter()
                .filter(|row| row["revm_result"].as_str() == Some("loss"))
                .count() as u64,
            required_u64(stability, "current_candidate_loss_rows"),
            "Gate 8 overwritten loss-row count drifted"
        );
        assert_eq!(
            overwritten_candidates
                .iter()
                .filter(|row| row["revm_result"].as_str() == Some("revert"))
                .count() as u64,
            required_u64(stability, "current_candidate_revert_rows"),
            "Gate 8 overwritten revert-row count drifted"
        );
        assert_eq!(
            non_null_field_count(&overwritten_candidates, "net_profit_wei") as u64,
            required_u64(stability, "current_candidate_rows_with_net_profit"),
            "Gate 8 overwritten net_profit_wei presence drifted"
        );
        assert_eq!(
            non_null_field_count(&overwritten_candidates, "estimated_gas_wei") as u64,
            required_u64(stability, "current_candidate_rows_with_estimated_gas"),
            "Gate 8 overwritten estimated_gas_wei presence drifted"
        );
        assert_eq!(
            non_null_field_count(&overwritten_candidates, "balance_delta_wei") as u64,
            required_u64(stability, "current_candidate_rows_with_balance_delta"),
            "Gate 8 overwritten balance_delta_wei presence drifted"
        );
        assert_eq!(
            non_null_field_count(&overwritten_candidates, "block") as u64,
            required_u64(stability, "current_candidate_rows_with_block"),
            "Gate 8 overwritten block presence drifted"
        );
    } else {
        assert!(
            !engine_stdout.contains(live_marker),
            "escalated Gate 8 attempt should not claim live marker proof"
        );
        assert_eq!(
            count_nonempty_lines(
                escalated_run_root.join(
                    escalated_attempt["raw_capture"]["file"]
                        .as_str()
                        .expect("Gate 8 escalated attempt missing raw capture file")
                ),
                "Gate 8 escalated raw feed capture",
            ),
            0,
            "escalated Gate 8 raw capture must remain empty in this failed attempt"
        );
        assert!(
            !escalated_run_root
                .join(
                    escalated_attempt["candidate_capture"]["file"]
                        .as_str()
                        .expect("Gate 8 escalated attempt missing candidate capture file")
                )
                .exists(),
            "escalated Gate 8 should not claim candidate capture"
        );
    }

    let long_attempt = named_attempt(&status, "escalated_60s_run");
    assert_eq!(
        long_attempt["exit_code"].as_i64(),
        Some(0),
        "long Gate 8 attempt must preserve successful harness exit code"
    );
    assert_eq!(
        long_attempt["result"].as_str(),
        Some("live_window_completed_candidate_records_missing"),
        "long Gate 8 attempt must keep the current candidate-record blocker red"
    );
    let long_run_dir = long_attempt["run_dir"]
        .as_str()
        .expect("Gate 8 long attempt missing run_dir");
    let long_run_root = repo_root_path(long_run_dir);
    let long_relay_stderr = read_required(
        long_run_root.join(
            long_attempt["sidecar"]["stderr_file"]
                .as_str()
                .expect("Gate 8 long attempt missing sidecar stderr_file"),
        ),
        "Gate 8 long relay sidecar stderr",
    );
    for fragment in long_attempt["sidecar"]["stderr_required_fragments"]
        .as_array()
        .expect("Gate 8 long attempt missing sidecar stderr fragments")
    {
        let fragment = fragment
            .as_str()
            .expect("Gate 8 long sidecar stderr fragment must be string");
        assert!(
            sidecar_stderr_satisfies_fragment(&long_relay_stderr, fragment),
            "long Gate 8 sidecar stderr missing fragment: {fragment}"
        );
    }

    let long_engine_stdout = read_required(
        long_run_root.join(
            long_attempt["engine"]["stdout_file"]
                .as_str()
                .expect("Gate 8 long attempt missing engine stdout file"),
        ),
        "Gate 8 long engine stdout",
    );
    assert!(
        long_engine_stdout.contains(
            long_attempt["engine"]["required_live_marker"]
                .as_str()
                .expect("Gate 8 long attempt missing live marker")
        ),
        "long Gate 8 attempt must prove the pipeline live marker"
    );
    for fragment in [
        "pipeline heartbeat elapsed_s=60 frames=8731",
        "candidates=7709",
        "validated=0 submitted=0",
    ] {
        assert!(
            long_engine_stdout.contains(fragment),
            "long Gate 8 engine stdout missing heartbeat fragment: {fragment}"
        );
    }
    assert_eq!(
        count_nonempty_lines(
            long_run_root.join(
                long_attempt["raw_capture"]["file"]
                    .as_str()
                    .expect("Gate 8 long attempt missing raw capture file")
            ),
            "Gate 8 long raw feed capture",
        ),
        long_attempt["raw_capture"]["nonempty_rows"]
            .as_u64()
            .expect("Gate 8 long attempt missing raw row count") as usize,
        "long Gate 8 raw capture row count drifted"
    );
    assert!(
        !long_run_root
            .join(
                long_attempt["candidate_capture"]["file"]
                    .as_str()
                    .expect("Gate 8 long attempt missing candidate capture file")
            )
            .exists(),
        "long Gate 8 should not claim candidate capture until candidate_records.jsonl exists"
    );

    let latest_attempt = named_attempt(&status, "escalated_300s_run");
    assert_eq!(
        latest_attempt["exit_code"].as_i64(),
        Some(0),
        "300s Gate 8 attempt must preserve successful harness exit code"
    );
    assert_eq!(
        latest_attempt["result"].as_str(),
        Some("live_window_completed_candidate_records_present_no_accepts"),
        "latest Gate 8 attempt must preserve the no-accepted-profit blocker"
    );
    let latest_run_dir = latest_attempt["run_dir"]
        .as_str()
        .expect("Gate 8 latest attempt missing run_dir");
    let latest_run_root = repo_root_path(latest_run_dir);
    let latest_engine_stdout = read_required(
        latest_run_root.join(
            latest_attempt["engine"]["stdout_file"]
                .as_str()
                .expect("Gate 8 latest attempt missing engine stdout file"),
        ),
        "Gate 8 latest engine stdout",
    );
    for fragment in [
        "dry-run pipeline live",
        "REVM route validation ACTIVE",
        "candidate record JSONL enabled",
        "pipeline heartbeat elapsed_s=300 frames=12710",
        "candidates=12541 validated=0 submitted=0",
        "revm_fork_suspect=588",
    ] {
        assert!(
            latest_engine_stdout.contains(fragment),
            "latest Gate 8 engine stdout missing fragment: {fragment}"
        );
    }
    assert_eq!(
        count_nonempty_lines(
            latest_run_root.join(
                latest_attempt["raw_capture"]["file"]
                    .as_str()
                    .expect("Gate 8 latest attempt missing raw capture file")
            ),
            "Gate 8 latest raw feed capture",
        ),
        latest_attempt["raw_capture"]["nonempty_rows"]
            .as_u64()
            .expect("Gate 8 latest attempt missing raw row count") as usize,
        "latest Gate 8 raw capture row count drifted"
    );
    assert_eq!(
        count_nonempty_lines(
            latest_run_root.join(
                latest_attempt["candidate_capture"]["file"]
                    .as_str()
                    .expect("Gate 8 latest attempt missing candidate capture file")
            ),
            "Gate 8 latest candidate records",
        ),
        latest_attempt["candidate_capture"]["nonempty_rows"]
            .as_u64()
            .expect("Gate 8 latest attempt missing candidate row count") as usize,
        "latest Gate 8 candidate row count drifted"
    );
    assert_eq!(
        latest_attempt["candidate_capture"]["summary"]["rows"].as_u64(),
        Some(2658),
        "Gate 8 latest candidate summary row count drifted"
    );
    assert_eq!(
        latest_attempt["candidate_capture"]["summary"]["revm_result_loss"].as_u64(),
        Some(625),
        "Gate 8 latest loss count drifted"
    );
    assert_eq!(
        latest_attempt["candidate_capture"]["summary"]["revm_result_revert"].as_u64(),
        Some(2033),
        "Gate 8 latest revert count drifted"
    );
    for field in [
        "rows_with_net_profit",
        "rows_with_estimated_gas",
        "rows_with_balance_delta",
        "rows_with_block",
    ] {
        assert_eq!(
            latest_attempt["candidate_capture"]["summary"][field].as_u64(),
            Some(0),
            "Gate 8 latest candidate summary unexpectedly has {field}"
        );
    }

    assert!(
        status["root_cause_assessment"]["not_proven_by_these_attempts"]
            .as_array()
            .expect("Gate 8 status missing not_proven_by_these_attempts")
            .iter()
            .any(|entry| entry.as_str() == Some("max/p95/p99 event-age metrics")),
        "failed Gate 8 attempts must state that live freshness metrics are not proven"
    );
}
