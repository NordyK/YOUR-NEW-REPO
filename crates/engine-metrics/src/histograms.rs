//! Latency histograms. Records `u64` nanosecond timings into `hdrhistogram` on the
//! background core; hot threads ship samples through a SPSC ring (provided by the engine).

use hdrhistogram::Histogram;
use parking_lot::Mutex;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Global runtime switch for latency recording. Default ON. When OFF, [`LatencyHistograms::record`]
/// is a single relaxed atomic load + early return — no mutex acquisition, no histogram write — so
/// observability adds no measurable hot-path cost. Callers on the hot path should also gate any
/// expensive log formatting behind [`metrics_enabled`]. Set once at startup from `METRICS_ENABLED`.
static METRICS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Enable or disable latency recording at runtime (operator toggle, env `METRICS_ENABLED`).
#[inline]
pub fn set_metrics_enabled(on: bool) {
    METRICS_ENABLED.store(on, Ordering::Relaxed);
}

/// Whether latency recording is currently enabled. Cheap (relaxed load); safe to call on the hot
/// path to gate expensive log/format work so disabled metrics cost effectively nothing.
#[inline(always)]
pub fn metrics_enabled() -> bool {
    METRICS_ENABLED.load(Ordering::Relaxed)
}

/// Stage identifier used to address one of the histograms.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageLatency {
    /// Frame parse + base64 decode.
    FrameParse = 0,
    /// Classifier.
    Classify = 1,
    /// State write + dirty lookup.
    DirtyLookup = 2,
    /// Confidence chain.
    Confidence = 3,
    /// Negative-filter cascade.
    Filters = 4,
    /// Sim.
    Sim = 5,
    /// REVM.
    Revm = 6,
    /// Sign + serialise + write.
    Submit = 7,
    /// End-to-end frame-recv to submit-ack.
    EndToEnd = 8,
    /// Whole-second sequencer event age at local receive. This is not sub-ms one-way latency; the
    /// histogram values for this one legacy stage are seconds, despite the generic latency store.
    FeedLatency = 9,
    /// Outbound RPC request to response (Alchemy round-trip).
    RpcLatency = 10,
    /// Fine sub-stage of [`StageLatency::Classify`] (opt-in, `LATENCY_FINE=1`): intent decode +
    /// state prediction (`classify_and_predict`). Splits the coarse Classify block so a tail
    /// spike is attributable to decode/predict vs commit vs the sniff fallback.
    Predict = 11,
    /// Fine sub-stage of Classify (opt-in): committing predicted state to the resident stores +
    /// generation bumps + checkpoint recording.
    StateCommit = 12,
    /// Fine sub-stage of Classify (opt-in): the undecodable-flow fallback (calldata pool-address
    /// sniff, optional access-list sniff, receipt replay scheduling, dirty emit).
    SniffFallback = 13,
    /// Fine sub-stage of [`StageLatency::Submit`] (opt-in, `LATENCY_FINE=1`): nonce claim →
    /// signed raw tx ready. ~0 on a speculative pre-sign hit; the full RLP+keccak+ECDSA
    /// (~30 µs) on a miss — the split is what shows whether the FCFS leg actually pays for
    /// signing or the speculative path absorbed it.
    SubmitSign = 14,
    /// Fine sub-stage of Submit (opt-in): JSON-RPC `eth_sendRawTransaction` envelope build
    /// (hex-encode + framing into the reusable buffer).
    SubmitEnvelope = 15,
    /// Fine sub-stage of Submit (opt-in): `conn.submit()` — HTTP request build + TLS write +
    /// sequencer ack read. Includes the network RTT, so compare against SubmitSign/SubmitEnvelope
    /// to attribute SEND overhead to our code vs the wire.
    SubmitWire = 16,
    /// Fine sub-stage of [`StageLatency::Sim`] (opt-in, `LATENCY_FINE=1`): dirty-event route
    /// collection (CSR slice walk + shape-budget passes + dedup). 2026-07-10 box post-mortem:
    /// sim_ns hit a needle-flat 1.01ms that the coarse stage could not attribute — these three
    /// sub-stages make any future sim inflation attributable in one heartbeat (and a sub-stage
    /// sum ≪ total proves external preemption of the sim core, not engine work).
    SimCollect = 17,
    /// Fine sub-stage of Sim (opt-in): per-pool spot refresh + spot-product score scan over the
    /// collected routes + top-K select + lane split.
    SimScore = 18,
    /// Fine sub-stage of Sim (opt-in): lane evaluation (grid/sizer) + candidate drain; includes
    /// the fork-join barrier when the broad lane dispatches to helper shards.
    SimEval = 19,
    /// A3 service-class split of the coarse [`StageLatency::Revm`] stage — always recorded IN
    /// ADDITION to `Revm` (which stays for continuity). The single `revm_ns` histogram mixed three
    /// cost classes that differ by ~6 orders of magnitude (median 334 ns vs p90 106 ms), making its
    /// p50 meaningless and the true full-REVM distribution invisible. These three split it apart.
    ///
    /// `RevmSkip`: a candidate short-circuited BEFORE any pricing work — the reject-cooldown gate
    /// (same route+state generation re-rejected within the TTL). ~ns.
    RevmSkip = 20,
    /// `RevmFastlane`: a candidate priced by the zero-RPC authoritative fast lane (the engine's own
    /// AMM kernels re-walked over event-mirrored state) or declined by the strict zero-RPC mode —
    /// no REVM, no RPC. ~µs.
    RevmFastlane = 21,
    /// `RevmFull`: a candidate validated by a real REVM fork execution (`call_ro_with_changes` /
    /// `call_ro_cache_only` against the MantisExecutor). ~ms–s (cold RPC fork misses dominate).
    RevmFull = 22,
    /// `NETWORK_FINE=1`, sampled: complete feed-frame receive → successful parsed-ring publish.
    FeedRxToParsedPush = 23,
    /// `NETWORK_FINE=1`, sampled: parsed-ring publish → state-writer pop (queue residence/wakeup).
    FeedParsedPushToStatePop = 24,
    /// `NETWORK_FINE=1`, sampled: state-writer pop → all state effects/derived events ready.
    FeedStatePopToReady = 25,
    /// `NETWORK_FINE=1`, sampled: correlated complete feed-frame receive → state ready.
    FeedRxToStateReady = 26,
    /// `NETWORK_FINE=1`, sampled submit: JSON-RPC body ready → byte-exact HTTP frame complete.
    SubmitHttpFrame = 27,
    /// `NETWORK_FINE=1`, sampled submit: HTTP frame complete → rustls `write_all` + `flush`
    /// returned. This is a LOCAL-KERNEL write-return boundary, NOT proof of NIC transmission.
    SubmitTlsWriteLocalKernel = 28,
    /// `NETWORK_FINE=1`, sampled submit: local-kernel TLS write return → first
    /// application-visible response byte (includes outbound/inbound network and server time).
    SubmitWriteReturnToResponseFirstByte = 29,
    /// `NETWORK_FINE=1`, sampled submit: first application-visible response byte → complete
    /// Content-Length-framed response body copied for the caller.
    SubmitResponseFirstByteToDone = 30,
}

const STAGE_COUNT: usize = 31;

/// One stage's cumulative distribution, read from a SINGLE merge across shards. Produced by
/// [`LatencyHistograms::snapshot_all`] so the periodic reporter reads every percentile/max/count
/// from one merged histogram instead of re-locking + re-merging the live shards per percentile.
#[derive(Debug, Clone, Copy, Default)]
pub struct StageSnapshot {
    /// Total samples recorded for this stage (all-time).
    pub count: u64,
    /// 20th percentile (ns).
    pub p20: u64,
    /// Median (ns).
    pub p50: u64,
    /// 90th percentile (ns).
    pub p90: u64,
    /// 95th percentile (ns).
    pub p95: u64,
    /// 99th percentile (ns).
    pub p99: u64,
    /// 99.9th percentile (ns) — only meaningful with ≥1000 samples, else 0.
    pub p999: u64,
    /// Maximum recorded value (ns).
    pub max: u64,
}

impl StageSnapshot {
    fn from_hist(h: Histogram<u64>) -> Self {
        let count = h.len();
        let q = |p: f64| h.value_at_quantile(p / 100.0);
        Self {
            count,
            p20: q(20.0),
            p50: q(50.0),
            p90: q(90.0),
            p95: q(95.0),
            p99: q(99.0),
            // p99.9 needs ≥1000 samples to mean more than "max with extra steps".
            p999: if count >= 1000 { q(99.9) } else { 0 },
            max: h.max(),
        }
    }
}

/// Number of histogram shards (lock-striping), split into two DISJOINT regions:
///
/// * `[0, CORE_SHARDS)` — **core-reserved**. A thread pinned to core `c` deterministically owns
///   shard `c % CORE_SHARDS` (bound via [`bind_thread_shard_to_core`], called from the engine's
///   `pin_to_core`). Because each pinned hot thread sits on its OWN core, two threads on different
///   cores NEVER address the same shard mutex in [`record`](LatencyHistograms::record). This is the
///   lock-disjointness that keeps the microsecond fast lane (rx@1, parser@2, sim@3, state-writer@5,
///   submit@7) isolated from the REVM validator lane (advancer@4, revm@6): a REVM `record()`
///   lock-hold — even if the validator is preempted mid-record — can never stall a fast-lane
///   `record()`, because the two lanes lock different mutexes by construction.
/// * `[CORE_SHARDS, N_SHARDS)` — round-robin pool for **unpinned** threads (test harnesses, async
///   pool workers). They never land on a core-reserved shard, so they also cannot collide with a
///   pinned lane's shard.
///
/// Readers (`percentile`/`max`/`count`, background only) merge every shard, so no sample is lost
/// regardless of which shard recorded it. `CORE_SHARDS = 8` covers the c8g 8-core layout; 16 total
/// leaves 8 shards for unpinned threads.
const CORE_SHARDS: usize = 8;
const N_SHARDS: usize = 16;

/// Process-global round-robin assigner for UNPINNED threads only: hands out shards in
/// `[CORE_SHARDS, N_SHARDS)`, so an unpinned thread can never collide with a core-reserved shard.
static NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// This thread's shard index. `usize::MAX` = unassigned — filled lazily on first record for
    /// unpinned threads, or eagerly by [`bind_thread_shard_to_core`] for pinned threads.
    static MY_SHARD: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// The core-reserved shard a thread pinned to `core` records into. Pure + deterministic: distinct
/// cores in `[0, CORE_SHARDS)` map to distinct shards, so pinned lanes never share a record mutex.
/// Cores `≥ CORE_SHARDS` wrap (only cores 0–7 exist on the target box).
#[inline]
pub fn shard_for_core(core: usize) -> usize {
    core % CORE_SHARDS
}

/// Bind THIS thread's histogram shard to its pinned `core`, so its `record()` calls take a shard
/// mutex that no thread on any OTHER core ever locks. Called from `pin_to_core` right after the CPU
/// affinity is set — idempotent, and independent of whether the affinity syscall actually succeeded
/// (so the deterministic core→shard mapping also holds in unit tests on a non-isolcpus box).
#[inline]
pub fn bind_thread_shard_to_core(core: usize) {
    MY_SHARD.with(|c| c.set(shard_for_core(core)));
}

/// This thread's current shard index (test / introspection accessor). Assigns lazily exactly like
/// [`record`](LatencyHistograms::record) would.
#[inline]
pub fn current_thread_shard() -> usize {
    my_shard()
}

#[inline]
fn my_shard() -> usize {
    MY_SHARD.with(|c| {
        let mut v = c.get();
        if v == usize::MAX {
            // Unpinned thread: round-robin over the NON-core-reserved shards so it can never
            // collide with a pinned lane's core-reserved shard.
            v = CORE_SHARDS + NEXT_SHARD.fetch_add(1, Ordering::Relaxed) % (N_SHARDS - CORE_SHARDS);
            c.set(v);
        }
        v
    })
}

fn make_stage_array() -> [Histogram<u64>; STAGE_COUNT] {
    // 1 ns to 60 s, 3 sig figs — feed latency from far-side hosts can hit multi-second values.
    std::array::from_fn(|_| Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap())
}

/// Per-shard histogram pair: the all-time `cumulative` set (for reporting) and a `window` set
/// that the breach monitor drains+resets every sample interval. The window exists so the latency
/// breach check sees a DECAYING p99 — a stale all-time tail can't keep the kill-switch latched
/// after the hot path has recovered (audit finding: cumulative HDR never decays).
struct StageHistos {
    cumulative: [Histogram<u64>; STAGE_COUNT],
    window: [Histogram<u64>; STAGE_COUNT],
}

/// Sharded stage-latency histograms. `record()` is uncontended (per-thread shard); reads merge.
pub struct LatencyHistograms {
    shards: [Mutex<StageHistos>; N_SHARDS],
}

impl LatencyHistograms {
    /// New collection. See [`N_SHARDS`] for the lock-striping rationale.
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| {
                Mutex::new(StageHistos {
                    cumulative: make_stage_array(),
                    window: make_stage_array(),
                })
            }),
        }
    }

    /// Record a sample. No-op (single relaxed atomic load) when metrics are disabled, so the
    /// hot path pays nothing once observability is switched off via `METRICS_ENABLED=0`. When
    /// enabled, takes only THIS thread's shard lock (uncontended) — no cross-thread stall.
    pub fn record(&self, stage: StageLatency, ns: u64) {
        if !metrics_enabled() {
            return;
        }
        {
            let mut g = self.shards[my_shard()].lock();
            let _ = g.cumulative[stage as usize].record(ns);
            let _ = g.window[stage as usize].record(ns);
        }
        // Opt-in tail attribution (TAIL_ATTRIB=1). Fast no-op otherwise; on an outlier it logs the
        // per-thread ctxsw/page-fault deltas + core so AWS p99/p999 spikes are attributable.
        crate::tail_attrib::maybe_attribute(stage, ns);
    }

    /// Merge one stage's CUMULATIVE histogram across all shards (background reader path).
    fn merged(&self, stage: StageLatency) -> Histogram<u64> {
        let mut acc = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
        for shard in &self.shards {
            let g = shard.lock();
            // `add` only fails on incompatible bounds; ours are identical, so this never errors.
            let _ = acc.add(&g.cumulative[stage as usize]);
        }
        acc
    }

    /// Drain the WINDOW histogram for `stage` (merge across shards, then clear it) and return its
    /// p-th percentile and sample count. Resets the window so the next call covers only samples
    /// recorded since this one — the decaying p99 the latency-breach monitor consumes. Returns
    /// `(value_ns, count)`; `count==0` means no samples in the window (don't act on the value).
    pub fn windowed_percentile_reset(&self, stage: StageLatency, percent: f64) -> (u64, u64) {
        let mut acc = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
        for shard in &self.shards {
            // `try_lock`, NOT `lock`: this reader runs on the fast-lane submit thread (core 7) every
            // ~7 s. A blocking lock here could stall the submit thread on a shard mutex momentarily
            // held by another recorder — including the REVM validator's shard — coupling fast-lane
            // submit latency to REVM activity, which the whole lane-disjoint sharding exists to
            // prevent. A shard being written at this instant is simply skipped: its samples roll into
            // the next window, and the windowed p99 is a statistical breach signal an occasional
            // skipped shard cannot mislead. This guarantees the reader never blocks a recorder.
            if let Some(mut g) = shard.try_lock() {
                let _ = acc.add(&g.window[stage as usize]);
                g.window[stage as usize].clear();
            }
        }
        (acc.value_at_quantile(percent / 100.0), acc.len())
    }

    /// Snapshot p-th percentile for a stage (merged across shards, all-time).
    pub fn percentile(&self, stage: StageLatency, percent: f64) -> u64 {
        self.merged(stage).value_at_quantile(percent / 100.0)
    }

    /// Once-per-interval snapshot of EVERY stage's cumulative distribution, taken by locking each
    /// shard EXACTLY ONCE and merging all stages in that single pass. The reporter then reads
    /// count/percentiles/max from the merged accumulators without re-locking any shard.
    ///
    /// This closes the metric-reader residual: the old 60 s reporter called
    /// `percentile()`/`max()`/`count()` separately for each stage, and each of those re-locked and
    /// re-merged EVERY shard — ~9 full shard-merges per stage (~200 per interval) all touching the
    /// live recorder mutexes. `snapshot_all` is `N_SHARDS` locks total for the whole report.
    /// Returned `Vec` is indexed by `stage as usize` (length `STAGE_COUNT`).
    pub fn snapshot_all(&self) -> Vec<StageSnapshot> {
        let mut acc: Vec<Histogram<u64>> = (0..STAGE_COUNT)
            .map(|_| Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap())
            .collect();
        for shard in &self.shards {
            let g = shard.lock();
            for (s, a) in acc.iter_mut().enumerate() {
                // Identical bounds ⇒ `add` never errors.
                let _ = a.add(&g.cumulative[s]);
            }
        }
        acc.into_iter().map(StageSnapshot::from_hist).collect()
    }

    /// Snapshot maximum recorded value (merged across shards).
    pub fn max(&self, stage: StageLatency) -> u64 {
        self.merged(stage).max()
    }

    /// Count of recorded samples (summed across shards, all-time).
    pub fn count(&self, stage: StageLatency) -> u64 {
        self.shards
            .iter()
            .map(|s| s.lock().cumulative[stage as usize].len())
            .sum()
    }
}

impl Default for LatencyHistograms {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both tests mutate the process-global `METRICS_ENABLED`, so they must not interleave.
    // Serialise them with a shared lock (standard pattern for tests sharing global state).
    static SERIAL: Mutex<()> = Mutex::new(());

    // Single test for both the percentile path and the enable/disable toggle: the toggle is a
    // process-global atomic, so exercising it from one test avoids a parallel-run race with a
    // second test mutating the same global.
    #[test]
    fn record_percentile_and_toggle() {
        let _g = SERIAL.lock();
        set_metrics_enabled(true);
        let h = LatencyHistograms::new();
        for i in 0..1000 {
            h.record(StageLatency::Submit, 100 + i);
        }
        let p95 = h.percentile(StageLatency::Submit, 95.0);
        assert!((1000..=1100).contains(&p95));

        // Disabled → record is a no-op.
        set_metrics_enabled(false);
        for _ in 0..1000 {
            h.record(StageLatency::Classify, 500);
        }
        assert_eq!(
            h.count(StageLatency::Classify),
            0,
            "disabled record must be a no-op"
        );

        // Re-enabled → recording resumes; leave the global ON (the crate default).
        set_metrics_enabled(true);
        h.record(StageLatency::Classify, 500);
        assert_eq!(h.count(StageLatency::Classify), 1);

        // Fine LATENCY_FINE/NETWORK_FINE sub-stages must index in-bounds (STAGE_COUNT guard).
        for st in [
            StageLatency::Predict,
            StageLatency::StateCommit,
            StageLatency::SniffFallback,
            StageLatency::SubmitHttpFrame,
            StageLatency::SubmitTlsWriteLocalKernel,
            StageLatency::SubmitWriteReturnToResponseFirstByte,
            StageLatency::SubmitResponseFirstByteToDone,
        ] {
            h.record(st, 250);
            assert_eq!(h.count(st), 1);
        }

        // A3: the three revm service-class split stages must index in-bounds (STAGE_COUNT=23)
        // and record independently of one another and of the legacy Revm stage.
        for st in [
            StageLatency::RevmSkip,
            StageLatency::RevmFastlane,
            StageLatency::RevmFull,
        ] {
            assert_eq!(h.count(st), 0, "split stage starts empty");
            h.record(st, 42);
            assert_eq!(h.count(st), 1, "split stage records in-bounds");
        }
        // Recording a split class must NOT bleed into the coarse Revm bucket (they are separate).
        assert_eq!(h.count(StageLatency::Revm), 0, "split stages are distinct from Revm");
    }

    #[test]
    fn snapshot_all_matches_per_stage_reads() {
        // P1: the once-per-interval `snapshot_all` must agree with the per-stage `count/percentile/max`
        // reads it replaces (it is the SAME merge, just done once). Guards against the reporter
        // switching to a snapshot that silently disagrees with the live histograms.
        let _g = SERIAL.lock();
        set_metrics_enabled(true);
        let h = LatencyHistograms::new();
        for i in 0..2000u64 {
            h.record(StageLatency::Submit, 100 + (i % 500));
            h.record(StageLatency::SubmitWire, 50 + (i % 50));
        }
        let snaps = h.snapshot_all();
        assert_eq!(snaps.len(), STAGE_COUNT);
        for stage in [StageLatency::Submit, StageLatency::SubmitWire] {
            let s = snaps[stage as usize];
            assert_eq!(s.count, h.count(stage), "snapshot count matches");
            assert_eq!(s.p50, h.percentile(stage, 50.0), "snapshot p50 matches");
            assert_eq!(s.p99, h.percentile(stage, 99.0), "snapshot p99 matches");
            assert_eq!(s.max, h.max(stage), "snapshot max matches");
            assert!(s.p999 > 0, "p999 populated with >=1000 samples");
        }
        // An unrecorded stage snapshots to a clean zero (count 0 ⇒ reporter skips it).
        let empty = snaps[StageLatency::RpcLatency as usize];
        assert_eq!(empty.count, 0);
        assert_eq!(empty.p999, 0, "p999 is 0 below 1000 samples");
    }

    // B1: per-thread sharding must lose ZERO samples under concurrency and merge correctly across
    // shards for count/percentile/max. Spawn more threads than there are shards to also exercise the
    // round-robin shard-reuse path (two threads sharing a shard → the per-shard lock serialises them
    // without data loss).
    #[test]
    fn sharded_record_is_lossless_under_concurrency() {
        use std::sync::Arc;
        let _g = SERIAL.lock();
        set_metrics_enabled(true);
        let h = Arc::new(LatencyHistograms::new());
        let threads = 32usize; // > N_SHARDS to force shard reuse
        let per = 5000u64;
        let mut handles = Vec::new();
        for _ in 0..threads {
            let h = Arc::clone(&h);
            handles.push(std::thread::spawn(move || {
                for i in 0..per {
                    // value range [100, 100+per) so the percentile is predictable
                    h.record(StageLatency::Sim, 100 + i);
                }
            }));
        }
        for t in handles {
            t.join().unwrap();
        }
        // Exact sample count preserved across all shards (no drops, no double-count).
        assert_eq!(
            h.count(StageLatency::Sim),
            threads as u64 * per,
            "sharded record must preserve every sample"
        );
        // Merged percentile is within the recorded value range.
        let p99 = h.percentile(StageLatency::Sim, 99.0);
        assert!((100..100 + per).contains(&p99), "p99 {p99} out of range");
        // Max is the largest value any thread recorded.
        assert!(h.max(StageLatency::Sim) >= 100 + per - 1);
    }

    // FAST-LANE ↔ REVM ISOLATION: `record()` must be lock-disjoint across the pinned lanes, so a
    // REVM `record()` lock-hold (even if the validator is preempted mid-record) can never stall a
    // fast-lane `record()`. Each pinned hot thread owns a private core-reserved shard.
    #[test]
    fn core_shard_binding_is_lane_disjoint_and_injective() {
        use std::collections::HashSet;
        // The microsecond fast lane pins these cores; the REVM validator lane pins these.
        let fast_lane_cores = [1usize, 2, 3, 5, 7]; // rx, parser, sim, state-writer, submit
        let revm_lane_cores = [4usize, 6]; // advancer, revm validator
        let fast: HashSet<usize> = fast_lane_cores.iter().map(|&c| shard_for_core(c)).collect();
        let revm: HashSet<usize> = revm_lane_cores.iter().map(|&c| shard_for_core(c)).collect();
        // Injective within the fast lane: each hot thread owns a distinct shard → no intra-lane
        // record contention either.
        assert_eq!(
            fast.len(),
            fast_lane_cores.len(),
            "fast-lane cores must map to distinct record shards"
        );
        // The isolation invariant: no fast-lane thread shares a record shard with a REVM-lane thread.
        assert!(
            fast.is_disjoint(&revm),
            "fast-lane shards {fast:?} must be disjoint from REVM-lane shards {revm:?} — a shared \
             histogram shard mutex would let a REVM record() lock-hold add latency to a fast-lane \
             record()"
        );
    }

    // A thread bound to a fast-lane core and a thread bound to the REVM core record concurrently
    // into their PRIVATE shards; every sample survives the merge (no loss, and — by disjoint shards
    // — no cross-lane blocking on the record path).
    #[test]
    fn bound_core_shards_keep_record_lossless() {
        use std::sync::Arc;
        let _g = SERIAL.lock();
        set_metrics_enabled(true);
        let h = Arc::new(LatencyHistograms::new());
        let per = 5000u64;
        let mut handles = Vec::new();
        for core in [3usize, 6] {
            // sim@3 (fast lane) vs revm@6 (REVM lane) → shards 3 and 6, disjoint.
            let h = Arc::clone(&h);
            handles.push(std::thread::spawn(move || {
                bind_thread_shard_to_core(core);
                assert_eq!(current_thread_shard(), core, "pinned thread owns its core shard");
                for i in 0..per {
                    h.record(StageLatency::Sim, 100 + i);
                }
            }));
        }
        for t in handles {
            t.join().unwrap();
        }
        assert_eq!(
            h.count(StageLatency::Sim),
            2 * per,
            "bound-shard concurrent records must lose no samples"
        );
    }

    // The fast-lane submit thread samples `windowed_percentile_reset` every ~7 s. It must NOT block
    // on a shard mutex held by a recorder (e.g. the REVM validator mid-record). Hold one shard on a
    // separate thread and assert the reader returns promptly (try_lock skip), never waiting it out.
    #[test]
    fn windowed_reset_does_not_block_on_a_held_shard() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let _g = SERIAL.lock();
        set_metrics_enabled(true);
        let h = Arc::new(LatencyHistograms::new());
        let hold = Duration::from_millis(400);
        let h2 = Arc::clone(&h);
        // Simulate a recorder (REVM lane) holding its shard mutex across a preemption window.
        let holder = std::thread::spawn(move || {
            let _g = h2.shards[0].lock();
            std::thread::sleep(hold);
        });
        // Ensure the holder has acquired the shard lock before we read.
        std::thread::sleep(Duration::from_millis(50));
        let t = Instant::now();
        let _ = h.windowed_percentile_reset(StageLatency::Submit, 99.0);
        let elapsed = t.elapsed();
        holder.join().unwrap();
        assert!(
            elapsed < Duration::from_millis(150),
            "windowed_percentile_reset must not block on a held shard (took {elapsed:?}); a blocking \
             lock would couple the fast-lane submit thread to a REVM-held shard mutex"
        );
    }
}
