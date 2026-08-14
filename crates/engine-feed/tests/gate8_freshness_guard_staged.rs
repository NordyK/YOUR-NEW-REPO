//! WIRED (Codex #3) - Gate8 binary-feed freshness closure. Was staged; the production changes below
//! are now implemented (frame V3 header_timestamp, sidecar freshness join, binary-RX FeedLatency,
//! event-age summary, candidate-record join keys) and these tests ENFORCE them.
//!
//! The current Gate8 binary live run proves feed/candidate activity, but not release-grade
//! freshness. The raw capture is the unmodified Arbitrum relay payload, so it contains sequencer
//! sequence/header timestamp but no host receive timestamp. The binary frame sent to the engine
//! carries sidecar `recv_ns`, but not the sequencer header timestamp. The binary RX loop therefore
//! records frame-parse/recv-to-classify latency, but not `feed_seq_to_recv_ns` event age.
//!
//! Target production closure, intentionally deferred:
//! 1. Persist a joined raw/live artifact containing sequenceNumber, sequencer header timestamp,
//!    and host receive timestamp in the same run as candidate_records.jsonl.
//! 2. Or carry the sequencer header timestamp through the binary frame and record
//!    StageLatency::FeedLatency in `rx_loop_unix_binary`.
//! 3. Emit p95/p99/max event-age metrics in the Gate8 run summary.
//! 4. Keep local >1s sequencer/local-network spikes acceptable only when measured and classified.

use std::{fs, path::Path};

const RELAY_SIDECAR_RS: &str = "../engine/src/bin/relay_sidecar.rs";
const PIPELINE_RS: &str = "../engine/src/pipeline.rs";
const FEED_FRAME_RS: &str = "src/feed_frame.rs";

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
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

/// (Codex #3) Raw capture stays a byte-identical replay corpus, AND a sibling freshness artifact
/// joins seq → sequencer_timestamp → host receive → event age in the same run.
#[test]
fn raw_capture_persists_joined_freshness_fields() {
    let relay = read(RELAY_SIDECAR_RS);
    assert!(
        relay.contains("capture.capture_raw_frame(cb.frame)"),
        "raw capture must stay byte-identical (it doubles as a replay corpus)"
    );
    assert!(
        relay.contains("capture_recv_ns") && relay.contains("event_age_ms"),
        "freshness join must persist host receive time + derived event age"
    );
    assert!(
        relay.contains("sequencer_timestamp") && relay.contains("freshness.jsonl"),
        "freshness join must persist the sequencer header timestamp to a sidecar artifact"
    );
}

/// (Codex #3) The binary V3 frame carries the sequencer header timestamp and the binary RX records
/// feed event age — so the zero-JSON fast path proves freshness, not just the JSON path.
#[test]
fn binary_path_records_feed_event_age() {
    let pipeline = read(PIPELINE_RS);
    let frame = read(FEED_FRAME_RS);
    let frame_view = section(&frame, "pub struct FeedFrameView", "/// V1 encoder");
    let binary_rx = section(
        &pipeline,
        "fn rx_loop_unix_binary",
        "fn enqueue_revm_state_replay",
    );

    assert!(
        frame_view.contains("pub header_timestamp"),
        "binary frame must carry the sequencer header timestamp"
    );
    assert!(
        frame.contains("encode_frame_v3") && frame.contains("FEED_FRAME_MAGIC_V3"),
        "a V3 frame encoder carrying the header timestamp must exist"
    );
    assert!(
        binary_rx.contains("StageLatency::FeedLatency"),
        "binary RX must record whole-second feed event age"
    );
}

/// (Codex #3) The run summary persists whole-second event age (p95/p99/max) so a run can ACCEPT
/// or REJECT local >1s feed staleness rather than infer it.
#[test]
fn gate8_summary_persists_event_age_percentiles() {
    let pipeline = read(PIPELINE_RS);
    assert!(pipeline.contains("max_event_age_seconds"));
    assert!(pipeline.contains("p95_event_age_seconds"));
    assert!(pipeline.contains("p99_event_age_seconds"));
    assert!(
        pipeline.contains("gate8_feed_freshness"),
        "the freshness summary must be emitted under a stable log name"
    );
}

/// (Codex #3) Candidate records carry the feed-seq join key + engine receive time, so each candidate
/// can be joined to the freshness sidecar for its per-candidate event age.
#[test]
fn candidate_records_carry_feed_join_keys() {
    let pipeline = read(PIPELINE_RS);
    assert!(
        pipeline.contains("\"feed_seq\"") && pipeline.contains("\"recv_ns\""),
        "candidate records must carry feed_seq + recv_ns to join the freshness sidecar"
    );
}
