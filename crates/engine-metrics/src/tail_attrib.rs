//! Tail-latency attribution (opt-in via `TAIL_ATTRIB=1`; zero hot-path cost when off).
//!
//! When a stage sample exceeds `TAIL_ATTRIB_US` (default 2000 µs), log *why* it was slow:
//! the per-thread context-switch + page-fault deltas and the running core. Without this, every
//! AWS p99/p999 spike is unattributable (scheduler preemption vs. major page fault vs. cold cache
//! all look identical in a histogram). Dependency-free: reads `/proc/thread-self/{stat,status}`
//! (the kernel's per-current-thread view, Linux ≥3.17), so no `libc`/`gettid`.
//!
//! Pure observability — no effect on detection, AMM math, sizing, or submission.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use crate::histograms::StageLatency;

static ENABLED: OnceLock<bool> = OnceLock::new();
static THRESHOLD_NS: OnceLock<u64> = OnceLock::new();
/// Rate-limit: at most one attribution line per ~this many ns of wall clock, process-wide, so a
/// pathological burst can't flood the log (the line itself must never become a tail source).
static LAST_LOG_NS: AtomicU64 = AtomicU64::new(0);
const MIN_LOG_GAP_NS: u64 = 1_000_000; // 1 ms

#[inline]
fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        matches!(
            std::env::var("TAIL_ATTRIB").ok().as_deref(),
            Some("1") | Some("true") | Some("on") | Some("yes")
        )
    })
}

#[inline]
fn threshold_ns() -> u64 {
    *THRESHOLD_NS.get_or_init(|| {
        std::env::var("TAIL_ATTRIB_US")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(2000)
            .saturating_mul(1_000)
    })
}

#[derive(Clone, Copy, Default)]
struct ThreadCounters {
    vol_ctxsw: u64,
    nonvol_ctxsw: u64,
    minflt: u64,
    majflt: u64,
    initialized: bool,
}

thread_local! {
    static LAST: Cell<ThreadCounters> = const { Cell::new(ThreadCounters {
        vol_ctxsw: 0, nonvol_ctxsw: 0, minflt: 0, majflt: 0, initialized: false,
    }) };
}

/// (minflt, majflt, processor) from `/proc/thread-self/stat`. Field indices (1-based, after the
/// comm field which is parenthesised): minflt=10, majflt=12, processor=39.
fn read_stat() -> Option<(u64, u64, i64)> {
    let s = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    // comm (field 2) can contain spaces/parens — split on the last ')'.
    let rest = &s[s.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After ')' the next token is field 3 (state). minflt is field 10 → index 10-3=7, etc.
    let minflt = f.get(7)?.parse().ok()?;
    let majflt = f.get(9)?.parse().ok()?;
    let processor = f.get(36)?.parse().ok()?;
    Some((minflt, majflt, processor))
}

/// (voluntary_ctxt_switches, nonvoluntary_ctxt_switches) from `/proc/thread-self/status`.
fn read_ctxsw() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/thread-self/status").ok()?;
    let mut vol = None;
    let mut nonvol = None;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("voluntary_ctxt_switches:") {
            vol = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
            nonvol = v.trim().parse().ok();
        }
    }
    Some((vol?, nonvol?))
}

fn stage_name(stage: StageLatency) -> &'static str {
    match stage {
        StageLatency::FrameParse => "frame_parse",
        StageLatency::Classify => "classify",
        StageLatency::DirtyLookup => "recv_to_classify",
        StageLatency::Confidence => "confidence",
        StageLatency::Filters => "filters",
        StageLatency::Sim => "sim",
        StageLatency::Revm => "revm",
        StageLatency::Submit => "submit",
        StageLatency::EndToEnd => "end_to_end",
        StageLatency::FeedLatency => "feed_latency",
        StageLatency::RpcLatency => "rpc_latency",
        StageLatency::Predict => "predict",
        StageLatency::StateCommit => "state_commit",
        StageLatency::SniffFallback => "sniff_fallback",
        StageLatency::SubmitSign => "submit_sign",
        StageLatency::SubmitEnvelope => "submit_envelope",
        StageLatency::SubmitWire => "submit_wire",
        StageLatency::SimCollect => "sim_collect",
        StageLatency::SimScore => "sim_score",
        StageLatency::SimEval => "sim_eval",
        // A3 revm service-class split (recorded alongside the coarse `revm` stage).
        StageLatency::RevmSkip => "revm_skip",
        StageLatency::RevmFastlane => "revm_fastlane",
        StageLatency::RevmFull => "revm_full",
        StageLatency::FeedRxToParsedPush => "feed_rx_to_parsed_push",
        StageLatency::FeedParsedPushToStatePop => "feed_parsed_push_to_state_pop",
        StageLatency::FeedStatePopToReady => "feed_state_pop_to_ready",
        StageLatency::FeedRxToStateReady => "feed_rx_to_state_ready",
        StageLatency::SubmitHttpFrame => "submit_http_frame",
        StageLatency::SubmitTlsWriteLocalKernel => "submit_tls_write_local_kernel",
        StageLatency::SubmitWriteReturnToResponseFirstByte => {
            "submit_write_return_to_response_first_byte"
        }
        StageLatency::SubmitResponseFirstByteToDone => "submit_response_first_byte_to_done",
    }
}

/// Called from the metrics `record()` path. Fast no-op unless `TAIL_ATTRIB=1` AND the sample is a
/// tail outlier. On an outlier, logs per-thread ctxsw/page-fault deltas + core — the attribution.
#[inline]
pub fn maybe_attribute(stage: StageLatency, ns: u64) {
    if !enabled() || ns < threshold_ns() {
        return;
    }
    // Per-process rate-limit (relaxed CAS-free check; a little slop is fine for diagnostics).
    let now = now_ns();
    let last = LAST_LOG_NS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < MIN_LOG_GAP_NS {
        return;
    }
    LAST_LOG_NS.store(now, Ordering::Relaxed);

    let (minflt, majflt, core) = read_stat().unwrap_or((0, 0, -1));
    let (vol, nonvol) = read_ctxsw().unwrap_or((0, 0));
    let prev = LAST.with(|c| c.get());
    LAST.with(|c| {
        c.set(ThreadCounters {
            vol_ctxsw: vol,
            nonvol_ctxsw: nonvol,
            minflt,
            majflt,
            initialized: true,
        })
    });
    if !prev.initialized {
        return; // first sample on this thread establishes the baseline; no delta yet.
    }
    tracing::warn!(
        target: "engine::tail_attrib",
        stage = stage_name(stage),
        us = ns / 1_000,
        core,
        d_vol_ctxsw = vol.saturating_sub(prev.vol_ctxsw),
        d_nonvol_ctxsw = nonvol.saturating_sub(prev.nonvol_ctxsw),
        d_minflt = minflt.saturating_sub(prev.minflt),
        d_majflt = majflt.saturating_sub(prev.majflt),
        "TAIL_ATTRIB: stage exceeded threshold"
    );
}

fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default_is_noop() {
        // With TAIL_ATTRIB unset, this must not panic and must be a cheap no-op.
        maybe_attribute(StageLatency::Sim, 999_999_999);
    }

    #[test]
    fn revm_split_stages_map_to_distinct_names() {
        // A3: the three service-class split stages must serialize to distinct, stable names so the
        // latency dump / tail-attrib lines are attributable. Also confirms they differ from the
        // coarse `revm` name (they are recorded alongside it, not instead of it).
        assert_eq!(stage_name(StageLatency::RevmSkip), "revm_skip");
        assert_eq!(stage_name(StageLatency::RevmFastlane), "revm_fastlane");
        assert_eq!(stage_name(StageLatency::RevmFull), "revm_full");
        assert_eq!(stage_name(StageLatency::Revm), "revm");
        let names = [
            stage_name(StageLatency::RevmSkip),
            stage_name(StageLatency::RevmFastlane),
            stage_name(StageLatency::RevmFull),
            stage_name(StageLatency::Revm),
        ];
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                assert_ne!(names[i], names[j], "split stage names must be distinct");
            }
        }
    }

    #[test]
    fn proc_readers_parse_current_thread() {
        // On Linux these should succeed; on non-Linux they return None and the caller falls back.
        if std::path::Path::new("/proc/thread-self/stat").exists() {
            assert!(read_stat().is_some());
            assert!(read_ctxsw().is_some());
        }
    }
}
