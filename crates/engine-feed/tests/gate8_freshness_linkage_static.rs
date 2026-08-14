use std::{fs, path::PathBuf};

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

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start_idx = source
        .find(start)
        .unwrap_or_else(|| panic!("source missing section start {start}"));
    let tail = &source[start_idx..];
    let end_idx = tail
        .find(end)
        .unwrap_or_else(|| panic!("source missing section end {end}"));
    &tail[..end_idx]
}

fn writer_record_block(source: &str) -> &str {
    let writer = source
        .find("fn write_candidate_record_jsonl(")
        .expect("candidate record writer");
    let tail = &source[writer..];
    let start = tail
        .find("let record = serde_json::json!({")
        .expect("candidate record json block");
    let record_tail = &tail[start..];
    let end = record_tail.find("});").expect("candidate record json end");
    &record_tail[..end]
}

#[test]
fn relay_sidecar_persists_raw_capture_and_joinable_freshness_sidecar() {
    let sidecar = read_repo("engine/crates/engine/src/bin/relay_sidecar.rs");
    let capture_freshness = section(
        &sidecar,
        "fn write_freshness_row",
        "#[derive(Debug)]\nstruct CaptureSlot",
    );
    let pump_feed = section(&sidecar, "fn pump_feed", "#[cfg(test)]");

    assert!(
        sidecar.contains("format!(\"{path}.freshness.jsonl\")"),
        "Gate8 freshness must be a sibling artifact to --capture-jsonl"
    );
    assert!(
        sidecar.contains("capture.capture_raw_frame(cb.frame);"),
        "raw capture must remain byte-identical relay JSON for replay"
    );
    assert!(
        pump_feed.contains("let recv_ns = cb.recv_ns;"),
        "live pump must use RelayCallback recv_ns, not a later timestamp"
    );
    assert!(
        pump_feed.contains("capture.capture_freshness(seq, env.timestamp, recv_ns);"),
        "live pump must join sequenceNumber, sequencer timestamp, and receive timestamp"
    );
    for field in [
        "\\\"seq\\\"",
        "\\\"sequencer_timestamp\\\"",
        "\\\"capture_recv_ns\\\"",
        "\\\"event_age_ms\\\"",
    ] {
        assert!(
            capture_freshness.contains(field),
            "freshness sidecar JSON is missing {field}"
        );
    }
}

#[test]
fn binary_v3_path_records_feed_event_age_and_summary_percentiles() {
    let frame = read_repo("engine/crates/engine-feed/src/feed_frame.rs");
    let pipeline = read_repo("engine/crates/engine/src/pipeline.rs");
    let frame_view = section(&frame, "pub struct FeedFrameView", "/// V1 encoder");
    let binary_rx = section(
        &pipeline,
        "fn rx_loop_unix_binary",
        "fn enqueue_revm_state_replay",
    );
    let summary = section(
        &pipeline,
        "pub fn log_latency_percentiles",
        "pub fn log_filter_reasons",
    );

    assert!(frame.contains("FEED_FRAME_MAGIC_V3"));
    assert!(frame.contains("encode_frame_v3"));
    assert!(
        frame_view.contains("pub header_timestamp: u64"),
        "V3 frame view must expose sequencer header timestamp"
    );
    assert!(
        binary_rx.contains("v.header_timestamp > 0")
            && binary_rx.contains("StageLatency::FeedLatency"),
        "binary RX must convert V3 header timestamp + recv_ns into FeedLatency samples"
    );
    for marker in [
        "gate8_feed_freshness",
        "p95_event_age_seconds",
        "p99_event_age_seconds",
        "max_event_age_seconds",
    ] {
        assert!(
            summary.contains(marker),
            "Gate8 summary missing freshness marker {marker}"
        );
    }
}

#[test]
fn candidate_records_carry_keys_to_join_freshness_to_money_records() {
    let pipeline = read_repo("engine/crates/engine/src/pipeline.rs");
    let record = writer_record_block(&pipeline);

    for marker in [
        "\"feed_seq\": candidate.seq",
        "\"recv_ns\": candidate.recv_ns",
        "\"trigger_recv_ns\": candidate.recv_ns",
        "\"trigger_seq\": candidate.seq",
    ] {
        assert!(
            record.contains(marker),
            "candidate record writer missing freshness join marker {marker}"
        );
    }
}

#[test]
fn current_report_keeps_gate8_live_freshness_aws_blocked_until_measured() {
    let report: Value = serde_json::from_str(&read_repo(
        "Codex/CURRENT_REMAINING_GAPS_CERTIFICATION_REPORT_2026-06-18.json",
    ))
    .expect("current report json");
    let blockers = report
        .get("verified_red_or_blocked")
        .and_then(Value::as_array)
        .expect("verified_red_or_blocked array");
    let gate8 = blockers
        .iter()
        .find(|row| {
            row.get("id").and_then(Value::as_str) == Some("live_feed_freshness_candidate_linkage")
        })
        .expect("Gate8 live freshness blocker");

    assert_eq!(
        gate8.get("classification").and_then(Value::as_str),
        Some("aws_live_measurement_blocker")
    );
    assert_eq!(
        gate8.get("production_logic_issue").and_then(Value::as_bool),
        Some(false)
    );
    assert!(
        {
            let evidence = gate8
                .get("evidence")
                .and_then(Value::as_str)
                .unwrap_or_default();
            evidence.contains("still missing is the AWS live run artifact")
                && evidence.contains("stale-rejection proof")
        },
        "report must not convert static linkage into live freshness certification"
    );
}
