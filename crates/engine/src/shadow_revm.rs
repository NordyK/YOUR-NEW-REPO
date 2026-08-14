//! Shadow-mode REVM worker (off the hot path, env-gated by `SHADOW_REVM=1`).
//!
//! The hot detector loop decodes the ~6% of feed traffic whose calldata maps to a known
//! pool/router intent. The other ~94% (proprietary aggregators / MEV routers) it can only
//! *sniff* for embedded pool addresses. This worker closes that gap in a **non-intrusive,
//! observable** way: for a sampled stream of raw feed txs it recovers the real sender,
//! replays the tx through REVM over live forked state ([`ForkExecutor`]), and reports which
//! *resident* pools the tx actually mutated — the pools the hot path should treat as dirty.
//!
//! It runs on a background core, reads through an [`ArrayQueue`] that drops on overflow (so it never
//! back-pressures the feed). For a touched resident pool it DRIVES detection: [row 7] V2/V3/DODO/
//! Algebra/Kyber are reconstructed from the LOCAL replay's post-state (via slot-reconstruction or the
//! getter-overlay); `SHADOW_STATE` (default on) arms that reconstruction + divergence diagnostics, but
//! the resident-store PROMOTION (committing the reconstructed post-state + waking at FEED latency with
//! NO RPC) is now EXPERIMENTAL and DEFAULT-OFF behind `SHADOW_PROMOTION` (round-9 N1 — codex proved the
//! producer can stale-BLESS at this scope, so the resident write is deferred to WI-09; see
//! `submit_shadow_promotion_if_enabled` and `codex/PHASE6_DEFERRAL_LEDGER_2026-07-17.md`). With
//! promotion OFF (default) and for families it does not support, touched pools fall back to a
//! parity-gated targeted resync (`SHADOW_EMIT`, default on). A stale-fork reconstruction is a wake/rank
//! signal only — re-validated by the sampled REVM validator + the on-chain `minNetProfit` revert, so it
//! is a wasted check, never lost capital.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ahash::AHashMap;
use alloy_primitives::{Address, U256};
use arrayvec::ArrayVec;
use crossbeam_queue::ArrayQueue;
use engine_detect::PoolPrediction;
use engine_feed::{decode_signed_tx, recover_tx_sender};
use engine_revm::{ChangedSlot, ForkExecutor};
use engine_rpc::{ArbitrumRpcClient, BlockTag};
use engine_state::{PoolId, PoolKind, V2Reserves256, V2State, V3State};
use tracing::{info, warn};

use crate::mirror_journal::{ApplyResult, MirrorJournal, ReconcileMode, RollbackTo};
use crate::pipeline::{
    prediction_family_write, submit_offthread_family_write, transitional_placeholder_point,
    AdvancerMetrics, DetectionContext,
};
use crate::revm_fork::{ProofVerifyingFetcher, RpcStateFetcher};
use crate::shadow_state::{reconstruct_v2_reserves, reconstruct_v3_slot0, reconstruct_v3_slot0_pre};
use crate::state_mirror::{health_reason, StateMirror};

/// Gas ceiling for a shadow replay — Arbitrum's per-tx cap is far below this, so a real
/// swap never hits it; we just want headroom so replay never halts on gas.
const SHADOW_GAS_LIMIT: u64 = 30_000_000;
/// Max resident pools recorded per tx (a single aggregator route rarely touches more).
const MAX_SHADOW_HITS: usize = 16;
/// Max changed storage slots retained PER pool for reconstruction (V2 needs ~1 reserves slot;
/// V3 needs slot0 + liquidity; the rest — price cumulatives, kLast — are decoys we tolerate).
const MAX_CHANGED_PER_POOL: usize = 24;
/// `getReserves()` selector — reads a UniV2-family pool's PRE-swap reserves off the not-yet-
/// committed fork, to anchor the [`reconstruct_v2_reserves`] packed-slot match.
const GET_RESERVES_SELECTOR: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];
/// `liquidity()` selector (UniswapV3/Sushi/Pancake `uint128 liquidity`, canonical storage slot 4).
/// WI-06 T3 fix round 6 (K1): the V3 reconstruction/pre-image reads the FORK's ACTUAL current
/// liquidity through this getter over the NON-overlaid (pre-swap) fork — never the resident base,
/// which made the CAS liquidity term a resident==resident tautology (both round-6 verifiers' blocker).
/// Mirrors `engine_rpc::v3_snapshot`'s `LIQUIDITY_SELECTOR` (asserted equal in a unit test below).
const V3_LIQUIDITY_SELECTOR: [u8; 4] = [0x1a, 0x68, 0x65, 0x02];
/// Canonical UniswapV3/Sushi/Pancake `uint128 liquidity` storage slot. A swap writes it ONLY on a
/// tick cross (`shadow_state::reconstruct_v3_slot0`), so its presence in a replay's changed slots is
/// the tick-cross signal WI-06 T3 round 6 (K2) uses to DECLINE the promotion.
const V3_LIQUIDITY_SLOT: u64 = 4;

/// A raw feed tx queued for shadow replay. `envelope` is the bytes *after* the L2 kind byte
/// (the [`recover_tx_sender`] / [`decode_signed_tx`] input).
pub struct ShadowJob {
    /// Raw signed-tx envelope (post kind byte).
    pub envelope: Vec<u8>,
    /// Wall-clock receive time (ns) stamped at enqueue, same clock as the hot path.
    pub recv_ns: u64,
    /// Sequencer message sequence number.
    pub seq: u64,
    /// Optional fixed-pool owner used by FEED_SLAB. Dropping/finishing the job returns its Vec;
    /// default JSON and non-slab binary paths leave this None and retain their prior ownership.
    pub(crate) recycle: Option<Arc<ArrayQueue<Vec<u8>>>>,
}

impl ShadowJob {
    pub(crate) fn owned(envelope: Vec<u8>, recv_ns: u64, seq: u64) -> Self {
        Self {
            envelope,
            recv_ns,
            seq,
            recycle: None,
        }
    }

    /// Copy into an already allocated envelope buffer. `None` is an explicit pool-exhaustion
    /// signal; callers must apply their existing shadow-drop or advancer-gap policy and must not
    /// fall back to a hot-path allocation.
    pub(crate) fn from_pool(
        pool: &Arc<ArrayQueue<Vec<u8>>>,
        envelope: &[u8],
        recv_ns: u64,
        seq: u64,
    ) -> Option<Self> {
        let mut bytes = pool.pop()?;
        bytes.clear();
        if envelope.len() > bytes.capacity() {
            let _ = pool.push(bytes);
            return None;
        }
        bytes.extend_from_slice(envelope);
        Some(Self {
            envelope: bytes,
            recv_ns,
            seq,
            recycle: Some(pool.clone()),
        })
    }
}

impl Drop for ShadowJob {
    fn drop(&mut self) {
        let Some(pool) = self.recycle.take() else {
            return;
        };
        let mut bytes = std::mem::take(&mut self.envelope);
        bytes.clear();
        // A full return pool would violate exclusive ownership accounting. Retiring the buffer is
        // safer than overwriting; normal construction makes this branch unreachable.
        let _ = pool.push(bytes);
    }
}

/// Cumulative shadow-worker telemetry (read by the background stats logger).
#[derive(Default)]
pub struct ShadowStats {
    /// Jobs dequeued and processed.
    pub jobs: AtomicU64,
    /// Jobs whose sender was recovered and envelope decoded.
    pub recovered: AtomicU64,
    /// Replays that executed without revert/halt.
    pub replayed_ok: AtomicU64,
    /// Replays that reverted/halted/errored.
    pub reverts: AtomicU64,
    /// Txs that mutated at least one resident pool.
    pub txs_touching_resident: AtomicU64,
    /// Total (tx, resident-pool) touches.
    pub resident_hits: AtomicU64,
    /// Jobs dropped at enqueue because the queue was full (feed never back-pressures).
    pub dropped: AtomicU64,
    /// Cumulative wall-clock spent inside `replay_tx` (ns) — divided by `jobs` gives the
    /// average replay cost, which drops sharply once the working set is warm.
    pub replay_ns_total: AtomicU64,
    /// Resident-pool moves whose REVM pre-state matched canonical `eth_getStorageAt` (the
    /// parity gate passed → a targeted resync was emitted).
    pub parity_pass: AtomicU64,
    /// Moves where REVM's pre-state diverged from canonical (stale fork) → emission skipped.
    pub parity_fail: AtomicU64,
    /// Targeted-resync requests emitted into the pipeline (REVM driving detection).
    pub emitted: AtomicU64,
    /// [row-7] DirtyEvents emitted with REVM-RECONSTRUCTED post-swap STATE — the undecodable-router
    /// pool refreshed from the LOCAL replay (no parity `eth_getStorageAt`, no resync RPC). This is
    /// the promotion; `emitted` above is the RPC-resync fallback for unsupported families. WI-06 T3
    /// fix round 9 (N1): a Shadow-origin write is submitted here ONLY when the EXPERIMENTAL, default-OFF
    /// `SHADOW_PROMOTION` gate is explicitly enabled — otherwise the would-be promotion is counted in
    /// `promotion_suppressed` and routed to the parity+resync diagnostic sink instead.
    pub state_emitted: AtomicU64,
    /// WI-06 T3 fix round 9 (N1): would-be shadow POST-STATE promotions that were NOT submitted to the
    /// publisher because the EXPERIMENTAL `SHADOW_PROMOTION` gate is OFF (the default). The prediction +
    /// pre-image were still built and the divergence still routed to the parity+resync diagnostic sink;
    /// only the resident-store WRITE was suppressed. Non-zero here on a default build is EXPECTED — it
    /// measures how many promotions the disarmed experimental lane WOULD have made.
    pub promotion_suppressed: AtomicU64,
    /// Jobs dequeued but SKIPPED because they aged past [`SHADOW_MAX_JOB_AGE_MS`] in the queue —
    /// a stale wake is worthless (the poller already resynced) and its replay would still burn
    /// 1-3 RPC round-trips. With evict-oldest intake this stays near zero except under sustained
    /// overload.
    pub stale_dropped: AtomicU64,
    /// Parity-fallback `eth_getStorageAt` reads that ERRORED (previously skipped silently —
    /// indistinguishable from "no verdict"). The pool falls to the normal poller (fail-closed).
    pub parity_err: AtomicU64,
}

/// Max queue age for a shadow job before the worker skips it instead of replaying: past this the
/// wake has lost its latency edge (feed → poller resync has already covered the pool) and the
/// replay would only burn RPC. ~8 Arbitrum blocks.
const SHADOW_MAX_JOB_AGE_MS: u64 = 2_000;

/// [WP5 · WP4 carry-over] Minimum seconds between in-run full-resync recovery attempts (each is a
/// whole-mirror re-proof at a current checked anchor — expensive, so a sustained latch retries on this
/// cadence rather than every block close).
const RESYNC_ATTEMPT_SECS: u64 = 30;

/// Whether a dequeued job is still worth replaying (see [`SHADOW_MAX_JOB_AGE_MS`]).
fn shadow_job_is_fresh(recv_ns: u64, now_ns: u64, max_age_ms: u64) -> bool {
    now_ns.saturating_sub(recv_ns) <= max_age_ms.saturating_mul(1_000_000)
}

/// Verdict of the pre-state parity read in the resync fallback.
#[derive(Debug, PartialEq, Eq)]
enum ParityOutcome {
    /// Canonical pre-state matches REVM's — the "pool moved" signal is trustworthy.
    Pass,
    /// Canonical diverges — stale fork; skip (fail-closed).
    Fail,
    /// The read itself errored — no verdict either way; counted, pool left to the poller.
    Err,
}

/// Classify a parity `eth_getStorageAt` read against the replay's pre-value.
fn classify_parity(read: Result<[u8; 32], engine_rpc::RpcError>, original: U256) -> ParityOutcome {
    match read {
        Ok(raw) if U256::from_be_bytes(raw) == original => ParityOutcome::Pass,
        Ok(_) => ParityOutcome::Fail,
        Err(_) => ParityOutcome::Err,
    }
}

/// Enqueue keeping the NEWEST jobs: on overflow evict the oldest (a stale wake is worthless —
/// the worker's freshness cutoff would skip it anyway). Eviction counts as `dropped`.
pub(crate) fn shadow_enqueue(queue: &ArrayQueue<ShadowJob>, stats: &ShadowStats, job: ShadowJob) {
    if queue.force_push(job).is_some() {
        stats.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// [Stage 1 · Mirror v2] The mirror-advancer's LOSSLESS-intent feed queue. Unlike the sampled
/// shadow queue (evict-oldest — it wants the newest wakes), the advancer must apply EVERY tx in
/// seq order, so on overflow it does the OPPOSITE: it drops the NEWEST (rejects the push) and
/// records a **gap** rather than silently evicting a tx already in flight. The feed still never
/// back-pressures (a full queue is the honesty valve, not a stall); a gap marks the current block
/// for forced canonical reconcile + trips the mirror unhealthy (degraded-mode honesty).
pub struct AdvanceFeed {
    /// Bounded FIFO — pop order is feed/seq order for the applied prefix.
    pub queue: ArrayQueue<ShadowJob>,
    /// Cumulative jobs rejected on overflow (each is a lossless-stream gap the advancer must repair).
    pub gaps: AtomicU64,
    /// Highest feed sequence number teed in (for the `mirror_lag_seq` = head − last-applied metric),
    /// advanced even on a dropped push so lag reflects the true feed head.
    pub head_seq: AtomicU64,
    /// FEED_SLAB-only preallocated envelope buffers for asynchronous ownership.
    pub(crate) envelope_pool: Option<Arc<ArrayQueue<Vec<u8>>>>,
}

impl AdvanceFeed {
    /// New lossless feed queue of `capacity` slots.
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: ArrayQueue::new(capacity),
            gaps: AtomicU64::new(0),
            head_seq: AtomicU64::new(0),
            envelope_pool: None,
        }
    }

    pub(crate) fn with_envelope_pool(
        capacity: usize,
        pool: Arc<ArrayQueue<Vec<u8>>>,
    ) -> Self {
        Self {
            queue: ArrayQueue::new(capacity),
            gaps: AtomicU64::new(0),
            head_seq: AtomicU64::new(0),
            envelope_pool: Some(pool),
        }
    }
}

fn advance_head(feed: &AdvanceFeed, seq: u64) {
    let mut cur = feed.head_seq.load(Ordering::Relaxed);
    while seq > cur {
        match feed.head_seq.compare_exchange_weak(
            cur,
            seq,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => cur = observed,
        }
    }
}

/// Record a feed item that could not acquire its preallocated advancer envelope. This has the same
/// correctness meaning as a full queue: advance the observed head and force canonical reconcile.
pub(crate) fn advance_pool_gap(feed: &AdvanceFeed, seq: u64) {
    advance_head(feed, seq);
    feed.gaps.fetch_add(1, Ordering::Relaxed);
}

/// Tee a signed-tx envelope into the mirror-advancer's lossless queue. Advances `head_seq`
/// unconditionally (the feed moved); on overflow the newest job is dropped and `gaps` bumped so the
/// advancer flags the block for reconcile — the tx is NEVER silently swallowed without a repair path.
pub(crate) fn advance_enqueue(feed: &AdvanceFeed, job: ShadowJob) {
    // Monotonic max of the observed feed sequence (relaxed CAS loop — off the hot detection path).
    let seq = job.seq;
    advance_head(feed, seq);
    if feed.queue.push(job).is_err() {
        feed.gaps.fetch_add(1, Ordering::Relaxed);
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn hex20(a: &[u8; 20]) -> String {
    let mut s = String::with_capacity(42);
    s.push_str("0x");
    for b in a {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// WI-06 T3 fix round 9 (N1): parse the EXPERIMENTAL `SHADOW_PROMOTION` gate. DEFAULT-OFF — a MISSING
/// (or empty / any non-true) value keeps the shadow POST-STATE promotion (the Shadow-origin
/// resident-store write) DISARMED. Pure over its input (mirrors `pipeline::parse_safety_switch`) so
/// the default and the truthy tokens are unit-tested WITHOUT mutating process env.
fn parse_shadow_promotion(raw: Option<&str>) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) => matches!(v.as_str(), "1" | "true" | "on" | "yes"),
        None => false,
    }
}

/// WI-06 T3 fix round 9 (N1): whether the EXPERIMENTAL shadow POST-STATE promotion is enabled.
/// DEFAULT-OFF. codex round-9 proved the shadow PRODUCER cannot be made fail-closed at this scope (a
/// whole-transaction repeated-pool round-trip restores tick + liquidity while retaining a stale
/// interior sqrt; Algebra's hidden timepoint/adaptive-fee state is outside `V3State` entirely), so the
/// resident-store write is disarmed by default and deferred to WI-09. When OFF the shadow lane stays
/// fully alive as diagnostics — it still replays, builds predictions + pre-images, and measures
/// divergence (the parity+resync sink); ONLY the resident-store submit is suppressed.
fn shadow_promotion_enabled() -> bool {
    parse_shadow_promotion(std::env::var("SHADOW_PROMOTION").ok().as_deref())
}

/// WI-06 T3 fix round 9 (N1): emit the LOUD one-shot operator warning when the EXPERIMENTAL gate is
/// explicitly enabled. Fires at most once per process (both worker loops share the latch) so an
/// operator sees it at boot without per-job log spam.
fn warn_experimental_shadow_promotion_once() {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        warn!(
            "EXPERIMENTAL shadow promotion ENABLED (SHADOW_PROMOTION) — the shadow resident-store write \
             is a WI-09 deferral with KNOWN-OPEN producer constructions (whole-transaction \
             repeated-pool round-trip restores tick+L with a stale interior sqrt; Algebra hidden \
             timepoint/adaptive-fee state is outside V3State). The publisher-side scalar-CAS / \
             tick-unchanged / interior-landing / shape / seq guards are the experimental-path defense; \
             see the WI-09 deferral ledger row before relying on it."
        );
    }
}

/// WI-06 T3 fix round 9 (N1): the SOLE shadow-promotion submit gate. The shadow POST-STATE promotion
/// (the Shadow-origin resident-store write into the sole publisher) is EXPERIMENTAL and DEFAULT-OFF
/// (see [`shadow_promotion_enabled`]). Returns `true` iff the write was SUBMITTED.
///
/// When `promotion_enabled` is false (the default) this NEVER reaches [`submit_offthread_family_write`]
/// — it records the suppressed promotion in `promotion_suppressed` and returns false, so the caller
/// routes the would-be promotion to the parity+resync diagnostic sink instead of writing the resident
/// store. When enabled, the publisher-side guards (scalar CAS, pre-image shape, tick-unchanged +
/// strictly-interior landing, seq) remain the fail-closed defense for the experimental path — this
/// gate only decides whether the write is offered at all.
#[allow(clippy::too_many_arguments)]
fn submit_shadow_promotion_if_enabled(
    promotion_enabled: bool,
    sender: &engine_state::publication::StateCommandSender,
    stats: &ShadowStats,
    pool: PoolId,
    snapshot_generation: engine_state::Generation,
    pre_replay_seq: u64,
    pre_image: engine_state::publication::FamilyWrite,
    write: engine_state::publication::FamilyWrite,
) -> bool {
    if !promotion_enabled {
        // EXPERIMENTAL gate OFF (default): do NOT submit the resident-store write. Record the
        // suppressed promotion as a diagnostic and let the caller fall through to the parity+resync
        // sink — the shadow lane's divergence signal is preserved, only the write is disarmed.
        stats.promotion_suppressed.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    if submit_offthread_family_write(
        sender,
        engine_state::publication::BarrierOriginClass::Shadow,
        pool,
        snapshot_generation,
        pre_replay_seq,
        Some(pre_image),
        transitional_placeholder_point(),
        write,
    )
    .is_ok()
    {
        stats.state_emitted.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// Run the shadow worker until `shutdown`. Pins to a fork snapshot (refreshed every
/// `refresh_secs`) and replays each queued tx — **committing** each successful replay's storage
/// deltas so consecutive swaps COMPOSE within the refresh window (see `commit_changed_slots`) —
/// logging resident-pool movements. `clock` supplies the feed-driven (number, timestamp) block
/// env so replays execute under real time (Algebra adaptive fee / V3 oracle writes are
/// timestamp-dependent; a ts≈0 replay computes post-state the real chain never had).
#[allow(clippy::too_many_arguments)]
pub(crate) fn shadow_loop(
    shutdown: &AtomicBool,
    queue: &ArrayQueue<ShadowJob>,
    rpc_url: String,
    needles: AHashMap<[u8; 20], u32>,
    stats: &ShadowStats,
    refresh_secs: u64,
    targeted_q: &ArrayQueue<PoolId>,
    emit: bool,
    detection: Option<&Arc<DetectionContext>>,
    state_sender: Option<&engine_state::publication::StateCommandSender>,
    state_emit: bool,
    clock: Option<&engine_gas::BatchClock>,
) {
    let client = ArbitrumRpcClient::new(rpc_url);
    // Offline replay: pin the fork to the snapshot block so REVM replays historical feed txs
    // against historical state (not the live head, which would contaminate the replay).
    let pinned = std::env::var("ENGINE_SNAPSHOT_BLOCK")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    let mut block = match pinned {
        Some(n) => BlockTag::Number(n),
        None => match client.block_number() {
            Ok(n) => BlockTag::Number(n),
            Err(_) => BlockTag::Latest,
        },
    };
    let mut exec = ForkExecutor::new(RpcStateFetcher::new(&client, block));
    // [Stage 3 · revmc advancer] The mirror-advancer opts into BACKGROUND-JIT independently of the
    // validator's REVM_BACKEND, via REVMC_ADVANCER=jit (default off). AOT is load-bearing here: it
    // makes full-feed re-execution affordable at burst. The wrap is non-blocking — every replay runs
    // interpreted until its background compile lands, so core 0 is never stalled on a compile. With
    // `--features revmc` off, or REVMC_ADVANCER unset, this stays the plain interpreter (byte-
    // identical to today). Shares the process-global backend + persisted store with the validator,
    // so a contract compiled for one is instantly available to the other.
    #[cfg(feature = "revmc")]
    {
        let advancer_mode = match std::env::var("REVMC_ADVANCER").ok().as_deref() {
            Some(v) if v.eq_ignore_ascii_case("jit") => {
                engine_revm::compiled::RevmBackendMode::Jit
            }
            _ => engine_revm::compiled::RevmBackendMode::Interpreter,
        };
        exec.set_backend_mode(advancer_mode);
        if advancer_mode != engine_revm::compiled::RevmBackendMode::Interpreter {
            info!(mode = ?advancer_mode, "SHADOW_REVM mirror-advancer: revmc background-JIT ENABLED (REVMC_ADVANCER)");
        }
    }
    // Pre-warm every resident pool's bytecode once: turns the first (and every) replay that
    // touches a pool into a cache hit for its code (the large, block-invariant part), so the
    // worker isn't paying a cold get_code per pool per replay.
    let warm_t0 = Instant::now();
    for addr in needles.keys() {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        exec.prewarm(*addr);
    }
    // WI-06 T3 fix round 9 (N1): read the EXPERIMENTAL, DEFAULT-OFF shadow-promotion gate ONCE (read
    // here rather than threaded from pipeline.rs, matching the advancer's `MIRROR_REORG_UNWIND`
    // precedent). When enabled, emit the loud one-shot operator warning at boot.
    let promotion_enabled = shadow_promotion_enabled();
    if promotion_enabled {
        warn_experimental_shadow_promotion_once();
    }
    info!(
        resident_pools = needles.len(),
        warm_accounts = exec.warm_accounts(),
        prewarm_ms = warm_t0.elapsed().as_millis() as u64,
        ?block,
        refresh_secs,
        emit,
        state_emit,
        promotion_enabled,
        "SHADOW_REVM worker online — resident code pre-warmed, replaying live feed txs"
    );
    let mut last_refresh = Instant::now();
    // Contracts whose storage was COMMITTED into the fork this refresh window (composed local
    // pre-state). Cleared at repin: the fork re-anchors to canonical head, so the next replay's
    // pre-state is RPC-fresh again. Used by the parity fallback — a composed pre-state can no
    // longer be compared against canonical-at-pin, and a composed pool is KNOWN to have moved,
    // so the resync is emitted directly (a resync fetches canonical state; over-emission is
    // safe, under-emission loses freshness).
    let mut composed: ahash::AHashSet<[u8; 20]> = ahash::AHashSet::new();

    while !shutdown.load(Ordering::Acquire) {
        if last_refresh.elapsed().as_secs() >= refresh_secs {
            // When pinned (offline replay) keep the historical fork block; only advance to the
            // live head in normal operation.
            if pinned.is_none() {
                if let Ok(n) = client.block_number() {
                    block = BlockTag::Number(n);
                    // Advance the snapshot but KEEP warmed bytecode — only storage (reserves,
                    // balances) re-fetches fresh. Bounds staleness without re-paying for code.
                    exec.repin(RpcStateFetcher::new(&client, block));
                    // Fresh anchor ⇒ the composed-state window starts over.
                    composed.clear();
                }
            }
            let jobs = stats.jobs.load(Ordering::Relaxed).max(1);
            info!(
                jobs = stats.jobs.load(Ordering::Relaxed),
                recovered = stats.recovered.load(Ordering::Relaxed),
                replayed_ok = stats.replayed_ok.load(Ordering::Relaxed),
                reverts = stats.reverts.load(Ordering::Relaxed),
                txs_touching_resident = stats.txs_touching_resident.load(Ordering::Relaxed),
                resident_hits = stats.resident_hits.load(Ordering::Relaxed),
                parity_pass = stats.parity_pass.load(Ordering::Relaxed),
                parity_fail = stats.parity_fail.load(Ordering::Relaxed),
                emitted = stats.emitted.load(Ordering::Relaxed),
                state_emitted = stats.state_emitted.load(Ordering::Relaxed),
                promotion_suppressed = stats.promotion_suppressed.load(Ordering::Relaxed),
                dropped = stats.dropped.load(Ordering::Relaxed),
                stale_dropped = stats.stale_dropped.load(Ordering::Relaxed),
                parity_err = stats.parity_err.load(Ordering::Relaxed),
                avg_replay_us = stats.replay_ns_total.load(Ordering::Relaxed) / jobs / 1_000,
                warm_accounts = exec.warm_accounts(),
                ?block,
                "SHADOW_REVM summary"
            );
            // [Stage 3] Surface the shared revmc backend's runtime ledger — `revmc_fallback_execs`
            // (lookups that ran interpreted: novel / not-yet-compiled bytecode) is the loud counter;
            // compiled_hits + resident show the compile working; compile_failed is the only red flag.
            #[cfg(feature = "revmc")]
            if let Some(s) = engine_revm::compiled::backend_stats() {
                info!(
                    revmc_fallback_execs = s.lookup_misses,
                    revmc_compiled_hits = s.lookup_hits,
                    revmc_resident = s.resident_entries,
                    revmc_compiled_ok = s.compilations_succeeded,
                    revmc_compiled_failed = s.compilations_failed,
                    revmc_pending_jobs = s.pending_jobs,
                    "SHADOW_REVM revmc backend"
                );
            }
            last_refresh = Instant::now();
        }
        let Some(job) = queue.pop() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        // Freshness cutoff: a job that aged out in the queue is skipped BEFORE paying the
        // recover/replay (and its cold-fetch RPC) cost — its wake value is already gone.
        if !shadow_job_is_fresh(job.recv_ns, now_ns(), SHADOW_MAX_JOB_AGE_MS) {
            stats.stale_dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        stats.jobs.fetch_add(1, Ordering::Relaxed);
        // Replays execute under the feed-driven block env (the same source the validator's A3
        // repin uses) so time-dependent pool state (Algebra timepoints/fee, V3 observations,
        // V2 cumulatives) is computed as the real chain computes it. Cheap: two atomic loads,
        // re-set only when the head advanced.
        if let Some(clock) = clock {
            let (n, ts) = (clock.last_block_number(), clock.last_block_timestamp_secs());
            if n > 0 && ts > 0 && exec.block_env() != Some((n, ts)) {
                exec.set_block_env(n, ts);
            }
        }
        let tq = if emit { Some(targeted_q) } else { None };
        process_job(
            &job,
            &mut exec,
            &needles,
            stats,
            &client,
            block,
            tq,
            detection,
            state_sender,
            state_emit,
            promotion_enabled,
            &mut composed,
            None,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn process_job(
    job: &ShadowJob,
    exec: &mut ForkExecutor<RpcStateFetcher<'_>>,
    needles: &AHashMap<[u8; 20], u32>,
    stats: &ShadowStats,
    client: &ArbitrumRpcClient,
    block: BlockTag,
    targeted_q: Option<&ArrayQueue<PoolId>>,
    detection: Option<&Arc<DetectionContext>>,
    state_sender: Option<&engine_state::publication::StateCommandSender>,
    state_emit: bool,
    // WI-06 T3 fix round 9 (N1): the EXPERIMENTAL, DEFAULT-OFF `SHADOW_PROMOTION` gate, read ONCE per
    // worker (never re-sampled per job). When false (default) the Shadow-origin resident-store write is
    // NOT submitted — the would-be promotion is routed to the parity+resync diagnostic sink.
    promotion_enabled: bool,
    composed: &mut ahash::AHashSet<[u8; 20]>,
    // [Stage 1] When `Some`, this replay's changed slots are journaled + applied to the standing
    // advancer mirror (ordered, idempotent) INSTEAD of the legacy `local_mirror_store`. `None` on
    // the sampled shadow path → byte-identical to before.
    adv: Option<&mut AdvanceCtx>,
) {
    let Some(from) = recover_tx_sender(&job.envelope) else {
        return;
    };
    let Some(tx) = decode_signed_tx(&job.envelope) else {
        return;
    };
    // Contract creation (to == 0) can't move a pool.
    if tx.to == [0u8; 20] {
        return;
    }
    stats.recovered.fetch_add(1, Ordering::Relaxed);

    // WI-06 T3 fix round 4 (codex round-4 item 5): capture the publisher's GLOBAL commit sequence
    // BEFORE the replay (and before ANY pool identity is known — a touched pool is a replay OUTPUT).
    // The shadow promotion below carries this `pre_replay_seq`; the publisher REFUSES the full-slot
    // write if the pool's `last_commit_seq` advanced past it (the pool committed since before the
    // replay ⇒ the stale-fork reconstruction can no longer be blessed over the fresher committed
    // value). This closes the interleaving the post-replay `expected_prior_generation` stamp cannot:
    // a commit landing DURING the replay is absorbed by that later stamp, so the F3 generation check
    // passes, but the pre-replay seq predates the commit so `last_commit_seq > pre_replay_seq` fires.
    // Captured unconditionally so it strictly precedes the fork work; only used when a promotion is
    // actually submitted (detection + state_sender present).
    let pre_replay_seq = detection
        .map(|d| d.universe.pools.commit_seq())
        .unwrap_or(engine_state::publication::UNVALIDATED_PRE_REPLAY_SEQ);

    let replay_t0 = Instant::now();
    let out = exec.replay_tx(from, tx.to, tx.value, tx.data.to_vec(), SHADOW_GAS_LIMIT);
    stats
        .replay_ns_total
        .fetch_add(replay_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    if out.success {
        stats.replayed_ok.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.reverts.fetch_add(1, Ordering::Relaxed);
    }

    // Group the mutated resident pools with ALL of their changed slots — reconstruction needs the
    // full per-pool set (V2's packed reserves slot; V3's slot0 + liquidity slot; the rest decoys).
    let mut groups: Vec<(u32, [u8; 20], ArrayVec<ChangedSlot, MAX_CHANGED_PER_POOL>)> = Vec::new();
    for cs in &out.changed_slots {
        let Some(&pid) = needles.get(&cs.address) else {
            continue;
        };
        if let Some(g) = groups.iter_mut().find(|(p, _, _)| *p == pid) {
            let _ = g.2.try_push(*cs);
        } else if groups.len() < MAX_SHADOW_HITS {
            let mut slots = ArrayVec::new();
            let _ = slots.try_push(*cs);
            groups.push((pid, cs.address, slots));
        }
    }

    let mut resync_ids: ArrayVec<u32, MAX_SHADOW_HITS> = ArrayVec::new();
    let mut state_ids: ArrayVec<u32, MAX_SHADOW_HITS> = ArrayVec::new();
    for (pid, addr, pool_slots) in &groups {
        // [fresh-overlay · family-agnostic] Publish this pool's raw post-swap changed slots so the
        // REVM validator can overlay them onto its stale-snapshot fork before pricing a route that
        // touches this pool (freshens ANY family — no per-family slot encoding). Cheap; the validator
        // only reads it when REVM_FRESH_OVERLAY is on.
        crate::pipeline::fresh_slots_store(*addr, pool_slots.to_vec());
        // [row-7] REVM POST-STATE promotion (EXPERIMENTAL, DEFAULT-OFF — `SHADOW_PROMOTION`, round-9 N1):
        // reconstruct the touched pool's typed post-swap state from the LOCAL replay and — ONLY when the
        // promotion is explicitly enabled — commit it + emit a DirtyEvent with no parity
        // `eth_getStorageAt` and no targeted resync RPC. Falls through to the parity+resync path when the
        // family is unsupported, reconstruction is implausible, or the promotion is OFF (default). The
        // "re-validated on the decision path (REVM validator + on-chain minNetProfit revert)" defense
        // below is publisher/decision-path hardening, NOT a producer soundness proof: round 9 showed the
        // shadow PRODUCER can stale-BLESS at this scope (whole-transaction repeated-pool round-trip;
        // Algebra hidden timepoint state), which is WHY the resident-store write is deferred to WI-09.
        if state_emit {
            if let (Some(detection), Some(sender)) = (detection, state_sender) {
                // WI-06 round 3 (codex item 5, shadow): EARLY-STAMP — capture the generation the
                // instant this pool is identified, BEFORE `build_shadow_prediction` reads any
                // resident state to compose the promotion. A commit interleaving between this stamp
                // and the apply advances the pool generation past it, so the stale-fork
                // reconstruction is REFUSED at apply rather than clobbering fresher committed state.
                //
                // NOTE (architectural, reported to verifiers): a pool's identity is a replay OUTPUT
                // — the touched pools are discovered from `out.changed_slots` — so the per-pool stamp
                // cannot be sampled before the replay itself. The replay reads the local REVM FORK,
                // not resident state, so this stamp still brands the ONLY resident read
                // (`build_shadow_prediction`), which it precedes. The residual commit-during-replay
                // window is the single-tx CPU replay (µs); a stale-fork value landing in it is
                // re-validated on the decision path (REVM validator + on-chain minNetProfit revert). This
                // decision-path defense is NOT a producer soundness proof (round 9 — see the [row-7] and
                // round-9 N1 notes); the promotion is an EXPERIMENTAL, default-OFF RPC-saving optimization
                // whose fallback is the parity+resync path below.
                let snapshot_generation = detection.universe.pools.generation(PoolId(*pid));
                let pred = build_shadow_prediction(
                    exec,
                    detection,
                    PoolId(*pid),
                    *addr,
                    &out.changed_slots,
                );
                // WI-06 T3 fix round 5 (codex round-5 item 5): the fork PRE-STATE value-parity (CAS)
                // pre-image — the resident state the prediction was computed FROM. Carried with the
                // promotion so the publisher applies it ONLY IF the pre-image still matches resident
                // (closes the lagging-fork / captured-mid-window stale-BLESS the `pre_replay_seq` guard
                // cannot). Derived from the SAME not-yet-committed fork (sequential `&mut exec` reads,
                // both non-committing). If EITHER the prediction OR the pre-image is unavailable, do NOT
                // submit the promotion — fall through to the parity+resync fallback below (fail-closed:
                // an unguarded Shadow write would be refused at apply anyway).
                let pre_image = build_shadow_pre_image(
                    exec,
                    detection,
                    PoolId(*pid),
                    *addr,
                    &out.changed_slots,
                );
                if let (Some(pred), Some(pre_image)) = (pred, pre_image) {
                    // WI-06 §3.3 T3: route the shadow POST-STATE promotion to the SOLE publisher as a
                    // `StateMutationBatch` (Shadow origin) instead of writing the store + waking
                    // directly. The publisher applies + bumps the generation, and the state-thread
                    // drain emits the DirtyEvent AFTER the commit (commit-before-wake) — so the shadow
                    // worker must NOT bump or emit here (no double-wake). A rejected batch has ALREADY
                    // disarmed authority + scheduled reconciliation; fall through to the parity+resync
                    // fallback below (never a direct write).
                    //
                    // WI-06 T3 fix round 9 (N1): the submit is DEFAULT-OFF (EXPERIMENTAL). The
                    // prediction + pre-image above are STILL built regardless (diagnostics unchanged);
                    // `submit_shadow_promotion_if_enabled` submits ONLY when `SHADOW_PROMOTION` is
                    // explicitly enabled, and otherwise routes the would-be promotion to the
                    // parity+resync diagnostic sink below (counted in `promotion_suppressed`). The
                    // pre-replay seq (defense-in-depth behind the CAS) and the fork pre-image (the
                    // publisher refuses the write unless it still byte-matches resident) ride the submit.
                    if submit_shadow_promotion_if_enabled(
                        promotion_enabled,
                        sender,
                        stats,
                        PoolId(*pid),
                        snapshot_generation,
                        pre_replay_seq,
                        pre_image,
                        prediction_family_write(&pred),
                    ) {
                        let _ = state_ids.try_push(*pid);
                        continue;
                    }
                }
            }
        }

        // Fallback: the pre-promotion parity gate + targeted resync. We never trust REVM's VALUE
        // here, only its "this pool moved" SIGNAL, and only if REVM read correct pre-state — the
        // slot's pre-value must equal canonical eth_getStorageAt at the fork block (fail-closed).
        if let Some(tq) = targeted_q {
            if composed.contains(addr) {
                // A locally-COMPOSED pre-state (a prior replay this window committed this pool's
                // slots) can no longer be compared against canonical-at-pin, and a composed pool
                // is KNOWN to have moved — emit the resync directly. A resync only triggers a
                // canonical re-fetch, so over-emission is safe; skipping the parity read also
                // saves its RPC round-trip.
                if tq.push(PoolId(*pid)).is_ok() {
                    stats.emitted.fetch_add(1, Ordering::Relaxed);
                    let _ = resync_ids.try_push(*pid);
                }
            } else if let Some(cs) = pool_slots.first() {
                let slot_bytes: [u8; 32] = cs.slot.to_be_bytes();
                match classify_parity(client.storage_at(addr, &slot_bytes, block), cs.original) {
                    ParityOutcome::Pass => {
                        stats.parity_pass.fetch_add(1, Ordering::Relaxed);
                        if tq.push(PoolId(*pid)).is_ok() {
                            stats.emitted.fetch_add(1, Ordering::Relaxed);
                            let _ = resync_ids.try_push(*pid);
                        }
                    }
                    ParityOutcome::Fail => {
                        stats.parity_fail.fetch_add(1, Ordering::Relaxed);
                    }
                    ParityOutcome::Err => {
                        // No verdict — counted (was a silent skip); the pool falls to the poller.
                        stats.parity_err.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    // [local-state · Stage 1 + D2] AFTER all pre-state work (reconstruction anchors and the
    // getter-overlay need the fork's PRE-tx state): commit this replay's storage deltas so the
    // NEXT replay composes on top of them, and publish the FULL changed-slot set (every touched
    // contract, ALL families, UNCAPPED — including the pools' token-balance slots) into the
    // standing state mirror. The mirror publish is a no-op unless REVM_LOCAL_STATE is on;
    // distinct from the per-pool `fresh_slots_store` overlay above (capped, per-candidate path).
    if record_composition(exec, &out, composed) {
        match adv {
            // [Stage 1] Ordered, journaled application to the standing advancer mirror. The FULL
            // changed-slot set (every touched contract — pool + token-balance slots, uncapped) is
            // committed to the fork above (composition) and here recorded+applied to the mirror,
            // keeping fork and mirror byte-identical so the journal's `original`s are valid inverses.
            Some(ctx) => ctx.journal_apply(&job.envelope, &out.changed_slots),
            // Legacy sampled path: no-op unless REVM_LOCAL_STATE is on (unchanged).
            None => crate::pipeline::local_mirror_store(&out.changed_slots),
        }
    }
    if groups.is_empty() {
        return;
    }

    stats.txs_touching_resident.fetch_add(1, Ordering::Relaxed);
    stats
        .resident_hits
        .fetch_add(groups.len() as u64, Ordering::Relaxed);
    let shadow_age_us = now_ns().saturating_sub(job.recv_ns) / 1_000;
    info!(
        seq = job.seq,
        from = %hex20(&from),
        to = %hex20(&tx.to),
        success = out.success,
        resident_pools = groups.len(),
        state_emitted = ?state_ids.as_slice(),
        resync_emitted = ?resync_ids.as_slice(),
        shadow_age_us,
        "SHADOW_REVM resident pools moved"
    );
}

/// [local-state · D2] Commit a successful replay's storage deltas into the shadow fork and record
/// its touched contracts in the composed-window set, so consecutive swaps on the same pool COMPOSE
/// (each next replay starts from the previous post-state instead of the pinned snapshot — the
/// uncommitted behaviour made the mirror end at f(pin, last_tx), silently missing every
/// intermediate swap; corruption that scaled with churn). Success-gated: a reverted feed tx leaves
/// no state on the real chain either. Returns whether the deltas were committed (the caller then
/// publishes the SAME set to the standing mirror, keeping fork and mirror byte-identical).
fn record_composition<F: engine_revm::StateFetcher>(
    exec: &mut ForkExecutor<F>,
    out: &engine_revm::ReplayOutcome,
    composed: &mut ahash::AHashSet<[u8; 20]>,
) -> bool {
    if !out.success || out.changed_slots.is_empty() {
        return false;
    }
    exec.commit_changed_slots(&out.changed_slots);
    composed.extend(out.changed.iter().copied());
    true
}

/// WI-06 T3 fix round 6 (K1): fork-read a UniswapV3/Sushi/Pancake pool's ACTUAL current `liquidity`
/// (`liquidity()` → storage slot 4) over the NON-overlaid fork. In `build_shadow_prediction` /
/// `build_shadow_pre_image` the `exec` fork is at PRE-swap state (the replay is non-committing and
/// composition has not run), so this returns the fork's PRE-swap active liquidity — the exact value the
/// reconstruction was computed FROM. Returns `None` on a reverted/short getter return (K4 totality: a
/// failed derivation DECLINES the promotion — never a resident-seeded fallback).
fn fork_v3_liquidity<F: engine_revm::StateFetcher>(
    exec: &mut ForkExecutor<F>,
    pool_addr: [u8; 20],
) -> Option<u128> {
    let (ok, out, _) = exec.call_ro(
        [0x11; 20],
        pool_addr,
        U256::ZERO,
        V3_LIQUIDITY_SELECTOR.to_vec(),
        200_000,
    );
    if !ok || out.len() < 32 {
        return None;
    }
    Some(u128::from_be_bytes(out[16..32].try_into().ok()?))
}

/// WI-06 T3 fix round 6 (K1/K2): derive a UniswapV3/Sushi/Pancake pool's `(PRE, POST)` typed swap
/// states from the replay's changed slots, with the active `liquidity` FORK-READ (never resident).
///
/// The `PRE` state is the shadow value-parity (CAS) pre-image — the resident state the reconstruction
/// was computed FROM; the publisher refuses the promotion unless it still byte-matches resident on the
/// swap-load-bearing fields (`sqrt_price_x96`/`tick`/`liquidity`, plus `fee_pips` for the dynamic-fee
/// Algebra arm). `base` supplies ONLY the swap-INVARIANT fields (`fee_pips`/`tick_spacing`/tick set) —
/// UniV3/Sushi/Pancake never move them within a swap — never the liquidity (K1).
///
/// K1: `liquidity` for BOTH states is the fork's actual liquidity — slot 4's changed value when the
/// swap wrote it (a tick cross), else the fork-read (`fork_v3_liquidity`). A missing fork read DECLINES.
fn derive_v3_shadow_states<F: engine_revm::StateFetcher>(
    exec: &mut ForkExecutor<F>,
    pool_addr: [u8; 20],
    base: &V3State,
    pool_slots: &[ChangedSlot],
) -> Option<(V3State, V3State)> {
    // K2a (cheap fork-side BELT — NOT a totality proof): a tick-crossing replay writes slot 4 (it
    // consumed the fork's tick set to compute the post-swap active liquidity via the crossed tick's
    // `liquidityNet`). If the fork's tick set LAGS a resident out-of-range Mint/Burn (which changes a
    // tick's net without moving the pre-swap scalars), that post-liquidity would be STALE, and a
    // PRE-value CAS — which only compares pre-swap fields — could not catch it. Declining on ANY slot-4
    // write cheaply sheds that class on the fork side. This is a BELT ONLY: it is NOT total (a two-cross
    // round trip that returns slot 4 to its pre-value leaves NO slot-4 change for this check to see), and
    // the reconstruction's tick set is itself fork-derived so it can lag. Correctness does NOT rest on
    // it — the publisher's tick-UNCHANGED + interior-landing gate (`publication.rs`, L1/M1) confines the
    // post tick representation-independently and refuses EVERY tick move (including the final-equal
    // two-cross) regardless of what this belt sees. A no-cross swap does not consume the tick set, so its
    // post scalars are faithful and its tick set is carried forward unchanged (preserved on apply, K2b).
    if pool_slots.iter().any(|c| c.slot == U256::from(V3_LIQUIDITY_SLOT)) {
        return None;
    }
    // K1: the fork's ACTUAL current liquidity over the non-overlaid (pre-swap) fork — the value the
    // no-cross reconstruction uses (the swap did not itself write slot 4). NEVER resident.
    let fork_liq = fork_v3_liquidity(exec, pool_addr)?;
    let (post_sqrt, post_tick, post_liq) = reconstruct_v3_slot0(pool_slots, fork_liq)?;
    let (pre_sqrt, pre_tick, pre_liq) = reconstruct_v3_slot0_pre(pool_slots, fork_liq)?;
    let mk = |sqrt, tick, liquidity| V3State {
        sqrt_price_x96: sqrt,
        tick,
        liquidity,
        fee_pips: base.fee_pips,
        tick_spacing: base.tick_spacing,
        initialised_ticks: base.initialised_ticks.clone(),
    };
    Some((
        mk(pre_sqrt, pre_tick, pre_liq),
        mk(post_sqrt, post_tick, post_liq),
    ))
}

/// WI-06 T3 fix round 6 (K4): decode an Algebra (Camelot V3) getter-overlay pool's typed V3 state.
/// `changes` is the post-swap overlay for the PREDICTION and `&[]` (the non-overlaid pre-swap fork)
/// for the PRE-IMAGE; `base` supplies ONLY the swap-invariant tick set / tick spacing. A `None`
/// `liquidity()` getter (reverted/absent) DECLINES the promotion (`liq?`) — NEVER a resident-seeded
/// fallback (`unwrap_or(base.liquidity)` would forge the CAS pre-image resident==resident).
fn derive_algebra_v3state<F: engine_revm::StateFetcher>(
    exec: &mut ForkExecutor<F>,
    pool_addr: [u8; 20],
    base: &V3State,
    changes: &[ChangedSlot],
) -> Option<V3State> {
    let raw = exec.read_getters_over_changes(
        changes,
        pool_addr,
        &engine_rpc::v3_snapshot::algebra_shadow_getters(),
    );
    let (sqrt, tick, fee, liq) = engine_rpc::v3_snapshot::decode_algebra_shadow(
        raw.first()?.as_deref()?,
        raw.get(1).and_then(|o| o.as_deref()),
    )?;
    Some(V3State {
        sqrt_price_x96: sqrt,
        tick,
        liquidity: liq?,
        fee_pips: fee,
        tick_spacing: base.tick_spacing,
        initialised_ticks: base.initialised_ticks.clone(),
    })
}

/// WI-06 T3 fix round 6 (K4): decode a Kyber Elastic getter-overlay pool's typed V3 state. Same
/// getter-None totality as [`derive_algebra_v3state`]: a `None` liquidity getter DECLINES (`liq?`),
/// never a resident-seeded fallback. Kyber's fee is static (from `base`), unlike Algebra's dynamic fee.
fn derive_kyber_v3state<F: engine_revm::StateFetcher>(
    exec: &mut ForkExecutor<F>,
    pool_addr: [u8; 20],
    base: &V3State,
    changes: &[ChangedSlot],
) -> Option<V3State> {
    let raw = exec.read_getters_over_changes(
        changes,
        pool_addr,
        &engine_rpc::kyber_snapshot::kyber_shadow_getters(),
    );
    let (sqrt, tick, liq) = engine_rpc::kyber_snapshot::decode_kyber_shadow(
        raw.first()?.as_deref()?,
        raw.get(1).and_then(|o| o.as_deref()),
    )?;
    Some(V3State {
        sqrt_price_x96: sqrt,
        tick,
        liquidity: liq?,
        fee_pips: base.fee_pips,
        tick_spacing: base.tick_spacing,
        initialised_ticks: base.initialised_ticks.clone(),
    })
}

/// [row-7] Reconstruct a touched pool's post-swap TYPED state from the shadow replay's changed
/// slots + the fork pre-state, as a `PoolPrediction` ready for `commit_prediction`. Zero RPC.
///
/// Supported families: UniswapV2/Sushi/Camelot V2 (post reserves via [`reconstruct_v2_reserves`],
/// anchored by a local `getReserves()` on the not-yet-committed fork) and UniswapV3/Sushi/Pancake
/// V3 (post sqrtPrice/tick/liquidity via [`reconstruct_v3_slot0`], preserving the mirror's fee /
/// tick-spacing / tick set). Returns `None` for any other family or an implausible reconstruction —
/// the caller then falls back to the existing parity + resync path (no regression).
fn build_shadow_prediction(
    exec: &mut ForkExecutor<RpcStateFetcher<'_>>,
    detection: &Arc<DetectionContext>,
    pool_id: PoolId,
    pool_addr: [u8; 20],
    all_changed: &[ChangedSlot],
) -> Option<PoolPrediction> {
    let meta = detection.universe.pools.meta(pool_id);
    // The pool's OWN changed slots (V2/V3 reconstruct from these — a value/key match must be scoped
    // to this pool). The getter-overlay families instead use the FULL changed set so a getter that
    // reads a co-mutated contract (e.g. a token balance) also sees post-swap values.
    let pool_slots: Vec<ChangedSlot> = all_changed
        .iter()
        .filter(|c| c.address == pool_addr)
        .copied()
        .collect();
    match meta.kind {
        PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => {
            // Pre-swap reserves off the NOT-yet-committed fork (replay_tx did not commit) — this is
            // the exact pre-state the changed slots' `original` came from, so the packed-slot match
            // is consistent.
            let (ok, out, _) = exec.call_ro(
                [0x11; 20],
                pool_addr,
                U256::ZERO,
                GET_RESERVES_SELECTOR.to_vec(),
                200_000,
            );
            if !ok || out.len() < 64 {
                return None;
            }
            let pre_r0 = u128::from_be_bytes(out[16..32].try_into().ok()?);
            let pre_r1 = u128::from_be_bytes(out[48..64].try_into().ok()?);
            let (r0, r1) = reconstruct_v2_reserves(pre_r0, pre_r1, &pool_slots)?;
            Some(PoolPrediction::V2 {
                pool_id,
                new_state: V2State {
                    reserves: V2Reserves256 {
                        reserve0: r0,
                        reserve1: r1,
                        block_timestamp_last: 0,
                    },
                },
            })
        }
        PoolKind::UniswapV3 | PoolKind::SushiV3 | PoolKind::PancakeV3 => {
            // WI-06 T3 fix round 6 (K1): base supplies ONLY the swap-invariant fields
            // (fee/tick-spacing/tick set) — the active liquidity is FORK-READ, never resident (that
            // resident copy made the CAS liquidity compare a tautology). See `derive_v3_shadow_states`.
            let base = detection.universe.v3_state.snapshot(meta.state_slot);
            let (_pre, post) = derive_v3_shadow_states(exec, pool_addr, &base, &pool_slots)?;
            Some(PoolPrediction::V3 {
                pool_id,
                new_state: post,
            })
        }
        // [row-7 / all families] Getter-overlay: run the family's EXISTING resync getters against the
        // REVM POST-swap state (overlay the full changed set — non-committing + restored) and decode
        // with the EXISTING engine-rpc decoder → byte-identical to the resync, but zero-RPC. Returns
        // None (→ parity+resync fallback) on any missing/short getter return.
        PoolKind::DodoV2 => {
            let cur = detection.universe.dodo_state.snapshot(meta.state_slot);
            let raw = exec.read_getters_over_changes(
                all_changed,
                pool_addr,
                &engine_rpc::dodo_shadow_getters(),
            );
            let state = engine_rpc::decode_dodo_state(
                &Address::from(pool_addr),
                cur.pool_class,
                raw.first()?.as_deref()?,
                raw.get(1)?.as_deref()?,
                raw.get(2)?.as_deref()?,
                raw.get(3).and_then(|o| o.as_deref()),
                raw.get(4).and_then(|o| o.as_deref()),
                0,
            )
            .ok()?;
            Some(PoolPrediction::Dodo {
                pool_id,
                new_state: Box::new(state),
            })
        }
        // Algebra (Camelot V3): V3State-backed, but globalState()/liquidity() layout differs from
        // UniV3 slot0 → getter-overlay. fee is the LIVE globalState fee; tick set preserved. K4: a
        // getter-None liquidity DECLINES (never a resident-seeded fallback) — see `derive_algebra_v3state`.
        PoolKind::AlgebraV19 => {
            let base = detection.universe.v3_state.snapshot(meta.state_slot);
            Some(PoolPrediction::V3 {
                pool_id,
                new_state: derive_algebra_v3state(exec, pool_addr, &base, all_changed)?,
            })
        }
        // Kyber Elastic: V3State-backed; getPoolState()/getLiquidityState() → getter-overlay. Uses
        // TOTAL liquidity (baseL + reinvestL). fee/tick-spacing/tick set preserved from the mirror.
        // K4: a getter-None liquidity DECLINES — see `derive_kyber_v3state`.
        PoolKind::KyberSwapElastic => {
            let base = detection.universe.v3_state.snapshot(meta.state_slot);
            Some(PoolPrediction::V3 {
                pool_id,
                new_state: derive_kyber_v3state(exec, pool_addr, &base, all_changed)?,
            })
        }
        _ => None,
    }
}

/// WI-06 T3 fix round 5 (codex round-5 item 5): reconstruct a touched pool's FORK PRE-SWAP typed state
/// — the resident state the [`build_shadow_prediction`] reconstruction was computed FROM — as the
/// shadow value-parity (CAS) pre-image the promotion carries to the publisher.
///
/// codex demonstrated that the `pre_replay_seq` seq guard cannot close a stale-BLESS by a fork that
/// LAGS a completed commit (its captured seq already equals the committed seq): the fork read
/// PRE-commit state while the seq looks current, so the strict-`>` compare passes and the stale
/// reconstruction clobbers the fresher resident. This pre-image ends the timing game: the publisher
/// applies the promotion ONLY IF this pre-image still byte-matches (swap-load-bearing fields) the
/// current resident — if the resident advanced at ANY point the pre-image cannot match and the write is
/// refused (resync-only). Reads the SAME fork pre-state the prediction used (V2 `getReserves`; V3 slot
/// `original`s; the getter-overlay families' getters over the NON-overlaid base fork). Returns `None`
/// (⇒ the promotion is NOT submitted — it falls to the parity+resync path) on any reconstruction the
/// prediction itself would also decline; a non-hot path (correctness over the extra local reads).
fn build_shadow_pre_image(
    exec: &mut ForkExecutor<RpcStateFetcher<'_>>,
    detection: &Arc<DetectionContext>,
    pool_id: PoolId,
    pool_addr: [u8; 20],
    all_changed: &[ChangedSlot],
) -> Option<engine_state::publication::FamilyWrite> {
    use engine_state::publication::FamilyWrite;
    let meta = detection.universe.pools.meta(pool_id);
    let pool_slots: Vec<ChangedSlot> = all_changed
        .iter()
        .filter(|c| c.address == pool_addr)
        .copied()
        .collect();
    match meta.kind {
        PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => {
            // The fork's PRE-swap reserves off the not-yet-committed fork — the exact pre-state the
            // reconstruction anchored its packed-slot match to.
            let (ok, out, _) = exec.call_ro(
                [0x11; 20],
                pool_addr,
                U256::ZERO,
                GET_RESERVES_SELECTOR.to_vec(),
                200_000,
            );
            if !ok || out.len() < 64 {
                return None;
            }
            let pre_r0 = u128::from_be_bytes(out[16..32].try_into().ok()?);
            let pre_r1 = u128::from_be_bytes(out[48..64].try_into().ok()?);
            Some(FamilyWrite::V2(V2State {
                reserves: V2Reserves256 {
                    reserve0: pre_r0,
                    reserve1: pre_r1,
                    block_timestamp_last: 0,
                },
            }))
        }
        PoolKind::UniswapV3 | PoolKind::SushiV3 | PoolKind::PancakeV3 => {
            // WI-06 T3 fix round 6 (K1): the PRE-image liquidity is FORK-READ (never the resident base
            // — that copy made the CAS liquidity compare `resident == resident`, blind to a lagging
            // in-range Mint/Burn that moved resident L but not the fork's). See `derive_v3_shadow_states`.
            let base = detection.universe.v3_state.snapshot(meta.state_slot);
            let (pre, _post) = derive_v3_shadow_states(exec, pool_addr, &base, &pool_slots)?;
            Some(FamilyWrite::V3(pre))
        }
        PoolKind::DodoV2 => {
            let cur = detection.universe.dodo_state.snapshot(meta.state_slot);
            // Getters over the NON-overlaid (pre-swap) base fork → the fork's pre-swap Dodo state.
            let raw = exec.read_getters_over_changes(&[], pool_addr, &engine_rpc::dodo_shadow_getters());
            let state = engine_rpc::decode_dodo_state(
                &Address::from(pool_addr),
                cur.pool_class,
                raw.first()?.as_deref()?,
                raw.get(1)?.as_deref()?,
                raw.get(2)?.as_deref()?,
                raw.get(3).and_then(|o| o.as_deref()),
                raw.get(4).and_then(|o| o.as_deref()),
                0,
            )
            .ok()?;
            Some(FamilyWrite::Dodo(Box::new(state)))
        }
        PoolKind::AlgebraV19 => {
            // K4: getters over the NON-overlaid (pre-swap) base fork; a getter-None liquidity DECLINES.
            let base = detection.universe.v3_state.snapshot(meta.state_slot);
            Some(FamilyWrite::V3(derive_algebra_v3state(
                exec, pool_addr, &base, &[],
            )?))
        }
        PoolKind::KyberSwapElastic => {
            let base = detection.universe.v3_state.snapshot(meta.state_slot);
            Some(FamilyWrite::V3(derive_kyber_v3state(
                exec, pool_addr, &base, &[],
            )?))
        }
        _ => None,
    }
}

/// [Stage 1] Per-job hook the advancer hands to [`process_job`]: after the replay's deltas are
/// committed to the fork (composition), record + apply the SAME changed-slot set to the standing
/// advancer mirror through the journal (ordered, idempotent, rollback-able). Books the apply-result
/// metric. Distinct object from the legacy `revm_local_mirror` — the Stage-1 validator never reads
/// this one (structural isolation: it wraps the legacy mirror, which stays block-tag 0 / untrusted).
pub(crate) struct AdvanceCtx<'a> {
    journal: &'a mut MirrorJournal,
    mirror: &'static StateMirror,
    block: u64,
    metrics: &'a AdvancerMetrics,
}

impl AdvanceCtx<'_> {
    fn journal_apply(&mut self, envelope: &[u8], slots: &[ChangedSlot]) {
        let tx_hash = engine_feed::address::keccak256(envelope);
        match self
            .journal
            .record_and_apply(self.mirror, self.block, tx_hash, slots)
        {
            ApplyResult::Applied => {
                self.metrics.applied.fetch_add(1, Ordering::Relaxed);
            }
            ApplyResult::DuplicateSkipped => {
                self.metrics.dup_skipped.fetch_add(1, Ordering::Relaxed);
            }
            ApplyResult::OutOfOrderRejected => {
                self.metrics.out_of_order.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// [Stage 5] The optional-hardening knobs the advancer honors — all default-off/neutral, so with the
/// flags unchanged this struct's effect is nil and the advancer is byte-identical to Stage 1.
#[derive(Clone, Debug, Default)]
pub struct Stage5Cfg {
    /// `MIRROR_COLD_VERIFY`: proof-verify each cold reconcile value against the block state root
    /// before it can repair the mirror.
    pub cold_verify: bool,
    /// `MIRROR_JOURNAL_PERSIST` + `MIRROR_JOURNAL_PATH`: persist the journal ring to this redb file
    /// (`None` ⇒ in-RAM-only).
    pub persist_path: Option<std::path::PathBuf>,
    /// `MIRROR_RECEIPT_SAMPLE_PCT`: deterministic per-tx receipt deep-compare rate — only consulted
    /// under [`ReconcileMode::Sampled`].
    pub receipt_sample_pct: u8,
}

/// [Stage 1] Close a completed feed block: reconcile its journal footprint against canonical state
/// and render a health verdict (Gate-C guarantees 3 + the health trip). The mirror ends every block
/// either PROVABLY head-consistent (clean, gapless, actually-checked ⇒ healthy) or PROVABLY unhealthy
/// (gap or gross divergence ⇒ tripped) — never silently wrong.
///
/// [Stage 5] Optionally proof-verifies the cold reconcile fetcher (`cold_verify`), runs a low-rate
/// canonical-receipt deep-compare (`receipt_sample_pct`, only under [`ReconcileMode::Sampled`]) that
/// folds into the same gap/trip logic, and persists the closed block if the journal is disk-backed.
/// [WP1 · Fix C] What [`close_block`] concluded — the caller consumes `publishable` to gate the
/// validator-visible snapshot, so an incomplete/failed close (proof/anchor failure, gap, outstanding
/// debt, or a full-resync escalation) never publishes trusted state. Minimal correct latch for WP1;
/// WP2 replaces it with reason-owned health bits without changing this call surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CloseBlockOutcome {
    /// The just-closed block's snapshot may be published (complete reconcile, no gap, no outstanding
    /// debt, no full-resync escalation).
    publishable: bool,
    /// The checked reconcile concluded with zero canonical failures.
    complete: bool,
    /// Canonical values that could not be resolved this close (proof/anchor/fetch failures).
    canonical_failures: usize,
    /// Debt exceeded its size/age cap ⇒ the journal was reset for a full resync and latched untrusted.
    full_resync: bool,
}

/// [WP1 · carry-over d] Per-close reconcile wall-clock budget in ms. `MIRROR_RECONCILE_BUDGET_MS`
/// (default 5000; `0` disables the deadline). Bounds total checked-fetch time across one close so a
/// slow/hung proof provider cannot stall the advancer; exhaustion fails closed (a canonical failure
/// into debt), never a silent skip. Read once per process.
fn reconcile_budget_ms() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("MIRROR_RECONCILE_BUDGET_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(5000)
    })
}

/// [WP4 · Fix E + F2] Reorg-path publish gate for the `MIRROR_COLD_VERIFY=0` compat path (the checked
/// `=1` path publishes only when `out.complete` + debt-empty + `is_healthy()`, inline). Never under
/// `cold_verify` (the =1 path owns its own certified publish), never while reconciliation debt is
/// outstanding or a full-resync latch is set, and only when the mirror is fully reason-free
/// (`is_healthy`) — symmetric with the forward `close_block` publish gate. Only when snapshots are on.
/// Under `=0` this is TRUST-EQUIVALENT to (strictly safer than) the original B5 publish: B5 published a
/// reorg unconditionally, whereas this adds the same `is_healthy()` + debt/latch gates the forward path
/// uses (an intentional `=0` delta — see the `DeepRegress` branch note); the outcomes coincide whenever
/// the mirror is healthy with no debt/latch, and only ever WITHHOLD a publish, never add one.
fn reorg_publishable(
    publish_snapshots: bool,
    cold_verify: bool,
    debt_empty: bool,
    resync_latched: bool,
    is_healthy: bool,
) -> bool {
    publish_snapshots && !cold_verify && debt_empty && !resync_latched && is_healthy
}

/// [WP4 · FIX-A] Latch the mirror UNTRUSTED after a deep reorg whose canonical-at-target footprint
/// could not be fully proven — either a checked-fetch/anchor failure, or a `target` below the journal
/// ring (beyond-journal). `REORG_RECOVERY` is a diagnostic marker, but the TRUST gate is the sticky
/// `FULL_RESYNC_REQUIRED` latch: unlike `REORG_RECOVERY` (which a later *successful* checked reorg
/// clears at the commit site), the sticky latch is heal-excluded and cleared ONLY by a real
/// re-hydration (`clear_full_resync_pending`, wired in WP5). So the "full resync" the caller logs is
/// honestly pending, and the degradation can NOT be laundered by an unrelated clean close or by a
/// successful reorg on a different footprint. `journal.reset()` drops the orphaned ring but the sticky
/// journal latch survives it (WP1 FIX-1), and the next `close` re-derives `FULL_RESYNC_REQUIRED` from
/// it. Applies only on the checked (`MIRROR_COLD_VERIFY=1`) path; the `=0` path keeps its
/// heal-on-clean-close QUEUE_GAP behavior.
fn latch_reorg_untrusted(mirror: &StateMirror, journal: &mut MirrorJournal) {
    mirror.set_reason(health_reason::REORG_RECOVERY);
    mirror.set_reason(health_reason::FULL_RESYNC_REQUIRED);
    journal.set_full_resync_pending();
    journal.reset();
}

/// [WP1 · FIX-3] The per-operation reconcile deadline. The budget is applied ONLY on the cold-verify
/// (checked-proof) path; with `MIRROR_COLD_VERIFY=0` the reconcile is byte-identical to the pre-WP1
/// legacy path (`None` deadline), so a slow `=0` reconcile can NEVER open debt where legacy would not.
/// `budget_ms == 0` also disables the deadline. Pure for unit testing.
fn reconcile_deadline(
    cold_verify: bool,
    budget_ms: u64,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    if cold_verify && budget_ms > 0 {
        Some(now + std::time::Duration::from_millis(budget_ms))
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn close_block(
    journal: &mut MirrorJournal,
    mirror: &StateMirror,
    block: u64,
    client: &ArbitrumRpcClient,
    mode: ReconcileMode,
    sample_n: usize,
    trip_pct: u64,
    gap_in_block: bool,
    metrics: &AdvancerMetrics,
    cold_verify: bool,
    receipt_sample_pct: u8,
) -> CloseBlockOutcome {
    // Canonical anchor: pin the reconcile fetcher to the block being reconciled (NOT `latest`).
    // `close_block(N)` fires at the N→N+1 boundary, so a caught-up advancer's mirror holds exactly
    // block-N's post-state — which `eth_getStorageAt(slot, N)` equals by definition. Reading `latest`
    // (the chain head, already at N+k while the advancer processed N) made EVERY footprint slot that
    // moved again in (N, N+k] falsely diverge, tripping the mirror unhealthy under benign forward-lag
    // (Stage-2 invariant #5). Pinning to `Number(block)` makes a clean advancer reconcile perfectly,
    // while a genuinely missed intervening tx (real drift) still diverges → repairs / trips. The block
    // is very recent (within the lag window), so any full node serves it — same archive assumption as
    // the validator fork's own pin. Per-candidate freshness still comes from Stage-3 invalidation.
    let canon = RpcStateFetcher::new(client, BlockTag::Number(block));
    // [WP1 · Fix B] CHECKED, transactional reconcile through the fail-closed seam
    // (`fetch_storage_checked`): a proof/anchor/fetch failure returns NO value, performs no mirror
    // mutation, and records reconciliation debt. `MIRROR_COLD_VERIFY=1` wraps the canonical fetcher in
    // a `ProofVerifyingFetcher` (proven value against an independently anchored root); OFF passes the
    // plain fetcher, whose default `fetch_storage_checked` is `Ok(fetch_storage)` — byte-identical to
    // the pre-WP1 reconcile and, crucially, ZERO proof/header RPC (no proof adapter is constructed).
    // [FIX-3] The deadline is applied only on the checked-proof path; =0 is byte-identical (None).
    let deadline = reconcile_deadline(cold_verify, reconcile_budget_ms(), std::time::Instant::now());
    let outcome = if cold_verify {
        let verifying = ProofVerifyingFetcher::new(canon, client, block, true);
        let o = journal.reconcile_block_checked(mirror, block, &verifying, mode, sample_n, deadline);
        if verifying.verify_failures() > 0 {
            metrics
                .cold_verify_failures
                .fetch_add(verifying.verify_failures(), Ordering::Relaxed);
        }
        o
    } else {
        journal.reconcile_block_checked(mirror, block, &canon, mode, sample_n, deadline)
    };
    if outcome.checked > 0 {
        metrics.reconcile_checked.fetch_add(outcome.checked as u64, Ordering::Relaxed);
    }
    if outcome.mismatched > 0 {
        metrics.reconcile_mismatches.fetch_add(outcome.mismatched as u64, Ordering::Relaxed);
    }
    if outcome.repaired > 0 {
        metrics.reconcile_repairs.fetch_add(outcome.repaired as u64, Ordering::Relaxed);
    }
    if outcome.canonical_failures > 0 {
        metrics.reconcile_incomplete.fetch_add(1, Ordering::Relaxed);
    }
    metrics
        .proof_debt_slots
        .store(journal.debt().len() as u64, Ordering::Relaxed);

    // [Stage 5] Optional low-rate canonical-receipt deep-compare (only under Sampled reconcile). A
    // sampled tx we journaled as state-changing but whose canonical receipt reverted / is absent is a
    // divergence — it folds into the SAME honesty net as a queue gap: flag the block for forced
    // reconcile and let the gap/trip verdict below trip the mirror unhealthy.
    let mut receipt_gap = false;
    if matches!(mode, ReconcileMode::Sampled) && receipt_sample_pct > 0 {
        let sampler = crate::receipt_sampler::ReceiptSampler::new(receipt_sample_pct);
        let mut checked = 0u64;
        let mut diverged = 0u64;
        for tx in journal.block_tx_hashes(block) {
            if !sampler.should_sample(&tx) {
                continue;
            }
            checked += 1;
            let status = crate::revm_fork::fetch_receipt_status(client, &tx);
            if crate::receipt_sampler::receipt_diverges(true, status) {
                diverged += 1;
            }
        }
        if checked > 0 {
            metrics.receipt_checked.fetch_add(checked, Ordering::Relaxed);
        }
        if diverged > 0 {
            metrics.receipt_divergences.fetch_add(diverged, Ordering::Relaxed);
            journal.flag_reconcile(block);
            receipt_gap = true;
        }
    }

    metrics
        .evicted_unreconciled
        .store(journal.evicted_unreconciled(), Ordering::Relaxed);

    // [WP1 · Fix B3] Full-resync escalation: carried debt exceeded its size/age cap. Reset the journal
    // (clears the debt/ring but KEEPS the sticky `full_resync_pending` latch — reset is NOT the resync).
    // The FULL_RESYNC_REQUIRED reason is set from that latch below. NEVER a silent eviction.
    let mut full_resync = false;
    if outcome.full_resync_required {
        journal.reset();
        metrics.full_resync_escalations.fetch_add(1, Ordering::Relaxed);
        full_resync = true;
    }

    // [WP2 · Fix C] Reason-owned health. The advancer owns every blocking reason on ITS mirror; each is
    // set/cleared independently so clearing one (a proved-gone gap) can never erase another (unresolved
    // proof debt or a sticky resync latch). A proof/canonical failure or outstanding debt DOMINATES
    // (PROOF_UNRESOLVED); `checked == 0` clears nothing (never a proof of health).
    let was_healthy = mirror.is_healthy();
    let gap = gap_in_block || receipt_gap;
    let debt_outstanding = !journal.debt().is_empty();
    // FULL_RESYNC_REQUIRED mirrors the WP1 sticky latch 1:1 (cleared only by real re-hydration).
    if journal.full_resync_pending() {
        mirror.set_reason(health_reason::FULL_RESYNC_REQUIRED);
    } else {
        mirror.clear_reason(health_reason::FULL_RESYNC_REQUIRED);
    }
    let rh = reconcile_health(
        outcome.complete,
        outcome.canonical_failures,
        debt_outstanding,
        outcome.checked,
        outcome.mismatched,
        gap,
        trip_pct,
    );
    mirror.set_reason(rh.set);
    if rh.heal {
        // A complete, gapless, debt-free, non-gross, actually-checked close clears the advancer's
        // HEALABLE reconcile reasons only. [WP4 · F1] REORG_RECOVERY is NO LONGER in this group: it is
        // owned by the checked reorg path (cleared only when a checked reorg reconcile proves the
        // footprint, or by a full resync), so a plain clean forward close can never launder it. The
        // sticky FULL_RESYNC_REQUIRED and the WP5 RESUME_PENDING are likewise excluded.
        mirror.clear_reason(
            health_reason::QUEUE_GAP
                | health_reason::GROSS_DIVERGENCE
                | health_reason::PROOF_UNRESOLVED,
        );
    }
    if was_healthy && !mirror.is_healthy() {
        metrics.unhealthy_trips.fetch_add(1, Ordering::Relaxed);
    }

    // [WP2 · Fix C] Publish authority: publish ONLY a complete reconcile with no queue/receipt gap and
    // a fully reason-free mirror (is_healthy() folds in every blocking reason, incl. a prior sticky
    // resync/reorg trip). `outcome.publishable` folds in complete + debt-empty at the reconcile layer.
    let publishable_now = outcome.publishable && !gap && mirror.is_healthy();

    // [Stage 5 · WP5 · F1] Persist the closed block WITH canonical provenance so a restart can validate
    // it against live headers (no-op unless MIRROR_JOURNAL_PERSIST). Provenance is only knowable on the
    // checked path (`cold_verify` resolves the canonical anchor); without it a record could never be
    // trusted on resume, so we skip persistence rather than store an unprovenanced (useless) record.
    // Gated on `is_persistent()` so persistence-off pays ZERO extra RPC.
    let provenance = if journal.is_persistent() && cold_verify {
        match crate::revm_fork::fetch_block_identity(client, block) {
            Ok(id) => Some(crate::mirror_journal_persist::BlockProvenance {
                block_hash: id.hash,
                parent_hash: id.parent_hash,
                state_root: id.state_root,
                close_complete: outcome.complete && !gap,
                reconciled: outcome.publishable,
                certified: publishable_now,
            }),
            Err(e) => {
                tracing::debug!(block, err = %e, "persist provenance fetch failed — skipping persist for this block");
                None
            }
        }
    } else {
        None
    };
    journal.persist_close(block, provenance.as_ref());

    CloseBlockOutcome {
        publishable: publishable_now,
        complete: outcome.complete,
        canonical_failures: outcome.canonical_failures,
        full_resync,
    }
}

/// [WP2 · Fix C] Pure reason decision for a closed block: which blocking [`health_reason`]s to SET and
/// whether to HEAL (clear the advancer's healable reconcile-reason group). A proof/canonical failure or
/// outstanding debt DOMINATES (always sets `PROOF_UNRESOLVED`, never heals); a gap sets `QUEUE_GAP`;
/// gross divergence (≥ `trip_pct`% of a non-empty checked sample) sets `GROSS_DIVERGENCE`. Heal only on
/// a complete, gapless, debt-free, non-gross, actually-checked block. `checked == 0` ⇒ no set, no heal.
struct ReconcileHealth {
    set: u32,
    heal: bool,
}

fn reconcile_health(
    complete: bool,
    canonical_failures: usize,
    debt_outstanding: bool,
    checked: usize,
    mismatched: usize,
    gap: bool,
    trip_pct: u64,
) -> ReconcileHealth {
    let proof = canonical_failures > 0 || !complete || debt_outstanding;
    let gross = checked > 0 && (mismatched as u64) * 100 >= (checked as u64) * trip_pct;
    let mut set = 0u32;
    if gap {
        set |= health_reason::QUEUE_GAP;
    }
    if gross {
        set |= health_reason::GROSS_DIVERGENCE;
    }
    if proof {
        set |= health_reason::PROOF_UNRESOLVED;
    }
    // Heal only when nothing is wrong AND we actually checked something (checked==0 never heals).
    let heal = !proof && !gap && !gross && checked > 0;
    ReconcileHealth { set, heal }
}

/// [B5] How a freshly observed feed head relates to the advancer's working block — the pure decision
/// behind the block-boundary handler, split out for unit testing. `cur_block == 0` (uninitialised)
/// is handled by the caller before this is consulted.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum FeedAdvance {
    /// No feed head yet (`head_block == 0`) or head == working block → nothing to do.
    Hold,
    /// Head advanced past the working block → close it and advance (the existing path).
    Forward,
    /// Head regressed by EXACTLY one block → the existing per-block reconcile + health path already
    /// covers it; no active multi-block unwind (preserves today's behavior).
    ShallowRegress,
    /// Head regressed DEEPER than one block (a reorg) → trigger a journal-driven multi-block unwind
    /// to `target` (the regressed-to block). Only acted on under `MIRROR_REORG_UNWIND`.
    DeepRegress { target: u64 },
}

/// Classify a feed head observation against the advancer's working block (see [`FeedAdvance`]).
fn classify_feed_advance(head_block: u64, cur_block: u64) -> FeedAdvance {
    if head_block == 0 || head_block == cur_block {
        FeedAdvance::Hold
    } else if head_block > cur_block {
        FeedAdvance::Forward
    } else if cur_block - head_block == 1 {
        FeedAdvance::ShallowRegress
    } else {
        FeedAdvance::DeepRegress { target: head_block }
    }
}

/// [Stage 1 · Mirror v2] The core-0 shadow worker PROMOTED to mirror-advancer (env
/// `MIRROR_FEED_ADVANCE=1`). It drains the LOSSLESS feed queue in seq order and, per tx: replays it
/// (composing on the fork), runs the same row-7 detection/wake as the sampled worker (which now
/// improves — it sees the full stream, no evict-oldest), and journals + applies the full changed-slot
/// set to the standing advancer mirror. At each feed block boundary it reconciles the closed block
/// against canonical state and renders a health verdict. On a lossless-queue gap it flags the block
/// for forced reconcile and trips the mirror unhealthy (degraded-mode honesty).
///
/// The Stage-1 validator does NOT read this mirror (it wraps the legacy `revm_local_mirror`, which
/// stays untrusted); this loop only builds + journals + reconciles + emits drift metrics. The fork is
/// repinned only at block boundaries (never mid-block), so intra-block `original`s stay valid inverses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn advancer_loop(
    shutdown: &AtomicBool,
    feed: &AdvanceFeed,
    rpc_url: String,
    needles: AHashMap<[u8; 20], u32>,
    stats: &ShadowStats,
    refresh_secs: u64,
    targeted_q: &ArrayQueue<PoolId>,
    emit: bool,
    detection: Option<&Arc<DetectionContext>>,
    state_sender: Option<&engine_state::publication::StateCommandSender>,
    state_emit: bool,
    clock: Option<&engine_gas::BatchClock>,
    mirror: &'static StateMirror,
    metrics: &AdvancerMetrics,
    reconcile_mode: ReconcileMode,
    journal_blocks: usize,
    sample_n: usize,
    trip_pct: u64,
    stage5: Stage5Cfg,
    // [B1] When true (validator trusts the mirror, i.e. REVM_STATE=mirror*), PUBLISH a frozen
    // block-boundary snapshot at each close so the validator reads a consistent end-of-block state
    // instead of the live map. False (REVM_STATE=rpc / S1 hot-but-untrusted) ⇒ never published,
    // zero clone cost, byte-identical to today's advancer.
    publish_snapshots: bool,
) {
    let client = ArbitrumRpcClient::new(rpc_url);
    let pinned = std::env::var("ENGINE_SNAPSHOT_BLOCK")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    let mut block = match pinned {
        Some(n) => BlockTag::Number(n),
        None => match client.block_number() {
            Ok(n) => BlockTag::Number(n),
            Err(_) => BlockTag::Latest,
        },
    };
    let mut exec = ForkExecutor::new(RpcStateFetcher::new(&client, block));
    // [Stage 3 · revmc advancer] The full-feed advancer opts into BACKGROUND-JIT independently of the
    // validator's REVM_BACKEND, via REVMC_ADVANCER=jit (default off). This is the SAME gating the
    // sampled shadow_loop ships with — but this `advancer_loop` SUPERSEDES shadow_loop whenever
    // MIRROR_FEED_ADVANCE=1 (our soak config), and until now the wiring lived ONLY in shadow_loop, so
    // the active advancer silently ran the plain interpreter no matter what REVMC_ADVANCER said. The
    // wrap is non-blocking: every replay runs interpreted until its background compile lands, so the
    // advancer core is never stalled on a compile. With `--features revmc` off, or REVMC_ADVANCER unset,
    // this stays the plain interpreter (byte-identical to today). Shares the process-global backend +
    // persisted store with the validator, so a contract compiled for one is instantly available to the
    // other. NOTE: JIT payoff on Graviton is unproven — size core 4 on the interpreted burst and A/B
    // REVMC_ADVANCER=jit against live compiled-hit counters before crediting any reduction.
    #[cfg(feature = "revmc")]
    {
        let advancer_mode = match std::env::var("REVMC_ADVANCER").ok().as_deref() {
            Some(v) if v.eq_ignore_ascii_case("jit") => {
                engine_revm::compiled::RevmBackendMode::Jit
            }
            _ => engine_revm::compiled::RevmBackendMode::Interpreter,
        };
        exec.set_backend_mode(advancer_mode);
        if advancer_mode != engine_revm::compiled::RevmBackendMode::Interpreter {
            info!(mode = ?advancer_mode, "MIRROR_FEED_ADVANCE advancer: revmc background-JIT ENABLED (REVMC_ADVANCER)");
        }
    }
    // [B5] MIRROR_REORG_UNWIND (default OFF): actively repair a feed regression DEEPER than one
    // block with a journal-driven multi-block unwind. OFF ⇒ deep regressions stay on the existing
    // per-block reconcile + health path (byte-identical to today's advancer). Read here rather than
    // threaded as a param so the pipeline.rs call site is untouched (matches ENGINE_SNAPSHOT_BLOCK,
    // which the advancer also reads directly from the environment).
    let reorg_unwind = std::env::var("MIRROR_REORG_UNWIND")
        .ok()
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false);
    // Pre-warm every resident pool's bytecode once (block-invariant, survives repins).
    for addr in needles.keys() {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        exec.prewarm(*addr);
    }
    // WI-06 T3 fix round 9 (N1): read the EXPERIMENTAL, DEFAULT-OFF shadow-promotion gate ONCE (same
    // read as the sampled `shadow_loop`; matches the `MIRROR_REORG_UNWIND` in-loop read above). Warn
    // loudly at boot on an explicit opt-in.
    let promotion_enabled = shadow_promotion_enabled();
    if promotion_enabled {
        warn_experimental_shadow_promotion_once();
    }
    info!(
        resident_pools = needles.len(),
        warm_accounts = exec.warm_accounts(),
        ?block,
        refresh_secs,
        reorg_unwind,
        ?reconcile_mode,
        journal_blocks,
        sample_n,
        emit,
        state_emit,
        promotion_enabled,
        "MIRROR_FEED_ADVANCE advancer online — full ordered feed → journaled mirror (hot-but-untrusted)"
    );
    // [Stage 5 · WP5] Disk-backed journal when MIRROR_JOURNAL_PERSIST is set; else in-RAM. Load is NOT
    // activation (Fix F3): `load_candidate` strictly DECODES the persisted state into an inactive
    // candidate but installs nothing — the journal is always attached FRESH (empty ring/seen/head), so a
    // persisted key can never DuplicateSkip and a persisted entry can never drive rollback. Only the
    // canonical-recovery protocol (Fix F4, below) turns a VALIDATED candidate into a warm mirror.
    use crate::mirror_journal_persist::ResumeCandidate;
    let resume_chain_id = engine_feed::constants::ARB_ONE_CHAIN_ID;
    let mut resume_blocks: Vec<crate::mirror_journal_persist::PersistedBlock> = Vec::new();
    let mut journal = match stage5.persist_path.as_deref() {
        Some(path) => {
            match crate::mirror_journal_persist::JournalPersist::open(
                path,
                journal_blocks,
                resume_chain_id,
            ) {
                Ok(p) => {
                    match p.load_candidate() {
                        Ok(ResumeCandidate::Empty) => {
                            info!(?path, "MIRROR_JOURNAL_PERSIST: fresh journal db — cold start")
                        }
                        Ok(ResumeCandidate::Unusable(reason)) => {
                            metrics.resume_discarded.fetch_add(1, Ordering::Relaxed);
                            info!(?path, %reason, "MIRROR_JOURNAL_PERSIST: resume candidate UNUSABLE — quarantined, cold start")
                        }
                        Ok(ResumeCandidate::Provenanced { blocks, head, .. }) => {
                            info!(?path, blocks = blocks.len(), head, "MIRROR_JOURNAL_PERSIST: provenanced resume candidate loaded (inactive) — validating");
                            resume_blocks = blocks;
                        }
                        Err(e) => {
                            info!(?path, err = %e, "MIRROR_JOURNAL_PERSIST: candidate load error — cold start")
                        }
                    }
                    MirrorJournal::with_persistence(journal_blocks, p)
                }
                Err(e) => {
                    info!(err = %e, "MIRROR_JOURNAL_PERSIST open failed — falling back to in-RAM journal");
                    MirrorJournal::new(journal_blocks)
                }
            }
        }
        None => MirrorJournal::new(journal_blocks),
    };
    // `cur_block` tracks the advancer's working block; a successful resume seeds it at the validated
    // anchor so the first forward close resumes from there (declared before recovery so it can set it).
    let mut cur_block: u64 = 0;

    // [Stage 5 · WP5 · F4] Canonical recovery. A provenanced candidate is validated against live headers
    // and hydrated at ONE checked anchor before ANY of its state affects trust. Warm-resume trust
    // requires the checked/proven path, so recovery only runs under MIRROR_COLD_VERIFY=1; otherwise the
    // candidate is discarded (safe cold start). While recovery runs the mirror is RESUME_PENDING
    // (untrusted); on success it activates + clears the reason, on any failure it discards to cold.
    if !resume_blocks.is_empty() {
        if stage5.cold_verify {
            mirror.set_reason(health_reason::RESUME_PENDING);
            let live_head = client.block_number().unwrap_or(0);
            let header = |n: u64| -> Result<[u8; 32], crate::mirror_journal::ResumeError> {
                crate::revm_fork::fetch_block_identity(&client, n)
                    .map(|id| id.hash)
                    .map_err(|_| crate::mirror_journal::ResumeError::HeaderUnavailable(n))
            };
            let make_fetcher = |anchor: u64| {
                ProofVerifyingFetcher::new(
                    RpcStateFetcher::new(&client, BlockTag::Number(anchor)),
                    &client,
                    anchor,
                    true,
                )
            };
            let deadline = reconcile_deadline(true, reconcile_budget_ms(), Instant::now());
            match crate::mirror_journal::recover_and_validate(
                &resume_blocks,
                live_head,
                header,
                make_fetcher,
                deadline,
            ) {
                Ok(activated) => {
                    let anchor = activated.anchor_block;
                    let prefix = activated.prefix_blocks;
                    let warmed = activated.patch.len();
                    journal.activate(mirror, activated);
                    cur_block = anchor;
                    metrics.resume_activated.fetch_add(1, Ordering::Relaxed);
                    info!(anchor, prefix, warmed, "MIRROR_JOURNAL_PERSIST: resume VALIDATED + activated — warm mirror at checked anchor");
                }
                Err(e) => {
                    mirror.clear_reason(health_reason::RESUME_PENDING);
                    metrics.resume_discarded.fetch_add(1, Ordering::Relaxed);
                    metrics.resume_validation_failures.fetch_add(1, Ordering::Relaxed);
                    info!(err = ?e, "MIRROR_JOURNAL_PERSIST: resume validation FAILED — discarded, cold start (RPC fallback + fresh journal)");
                }
            }
        } else {
            metrics.resume_discarded.fetch_add(1, Ordering::Relaxed);
            info!("MIRROR_JOURNAL_PERSIST: resume candidate present but MIRROR_COLD_VERIFY=0 (no checked path) — discarded, cold start");
        }
    }
    let mut composed: ahash::AHashSet<[u8; 20]> = ahash::AHashSet::new();
    let mut gap_in_block = false;
    let mut last_gaps: u64 = 0;
    let mut last_refresh = Instant::now();
    let mut repin_due = false;
    // [WP5 · WP4 carry-over] Throttle the in-run full-resync recovery (a whole-mirror re-proof) so a
    // sustained latch retries periodically, not every block close.
    let mut last_resync_attempt = Instant::now();

    while !shutdown.load(Ordering::Acquire) {
        let head_block = clock.map(|c| c.last_block_number()).unwrap_or(0);
        if last_refresh.elapsed().as_secs() >= refresh_secs {
            repin_due = true;
        }
        // [WP5 · WP4 carry-over] In-run full-resync recovery: when the sticky FULL_RESYNC_REQUIRED latch
        // is set (a WP1 debt-cap escalation or a WP4 reorg failure left the mirror untrusted), re-prove
        // the WHOLE resident footprint at the CURRENT checked head. On a complete re-proof it clears the
        // latch + FULL_RESYNC_REQUIRED and re-publishes — so a degraded mirror recovers in-run instead of
        // staying dead until restart. Only under the checked path (cold_verify); throttled.
        if stage5.cold_verify
            && head_block > 0
            && journal.full_resync_pending()
            && last_resync_attempt.elapsed().as_secs() >= RESYNC_ATTEMPT_SECS
        {
            last_resync_attempt = Instant::now();
            let verifying = ProofVerifyingFetcher::new(
                RpcStateFetcher::new(&client, BlockTag::Number(head_block)),
                &client,
                head_block,
                true,
            );
            let deadline = reconcile_deadline(true, reconcile_budget_ms(), Instant::now());
            if journal.recover_full_resync(mirror, &verifying, deadline) {
                // [FIX-AUDIT-2] recover_full_resync now clears BOTH FULL_RESYNC_REQUIRED and
                // REORG_RECOVERY on a complete re-prove (the whole resident footprint subsumes the reorg
                // footprint), so a reorg-degraded mirror recovers in-run — no reason-clear needed here.
                mirror.retag_block(head_block);
                if publish_snapshots && mirror.is_healthy() {
                    mirror.publish_certified(head_block);
                }
                metrics.full_resync_recoveries.fetch_add(1, Ordering::Relaxed);
                info!(head_block, "MIRROR_FULL_RESYNC: in-run recovery re-proved the mirror at a checked anchor — FULL_RESYNC_REQUIRED + REORG_RECOVERY cleared");
            }
        }
        // Block-boundary close + advance. Repin (if due) happens HERE, at the boundary, never
        // mid-block — so a block's replays all see the same fork pin and the journal `original`s
        // are consistent inverses.
        if head_block > 0 {
            if cur_block == 0 {
                cur_block = head_block;
                mirror.retag_block(head_block);
            } else {
                match classify_feed_advance(head_block, cur_block) {
                    // Head == working block (Hold), or regressed by exactly one block
                    // (ShallowRegress) → no boundary action, exactly as today (the per-block
                    // reconcile + health path already covers a one-block regression).
                    FeedAdvance::Hold | FeedAdvance::ShallowRegress => {}
                    FeedAdvance::Forward => {
                        let close = close_block(
                            &mut journal,
                            mirror,
                            cur_block,
                            &client,
                            reconcile_mode,
                            sample_n,
                            trip_pct,
                            gap_in_block,
                            metrics,
                            stage5.cold_verify,
                            stage5.receipt_sample_pct,
                        );
                        // [B1 + WP1·Fix C] Block-boundary PUBLISH: freeze the just-closed block
                        // (`cur_block`) as the validator-visible snapshot BEFORE we retag + start
                        // mutating the map for the next block — but ONLY when `close_block` certifies it
                        // publishable (a complete, gapless, debt-free reconcile). An incomplete/failed
                        // close publishes nothing, so a later heal can never make a previously captured
                        // uncertified snapshot usable. Gated so S1 (REVM_STATE=rpc) pays nothing.
                        if publish_snapshots && close.publishable {
                            mirror.publish_certified(cur_block);
                        }
                        gap_in_block = false;
                        cur_block = head_block;
                        mirror.retag_block(head_block);
                        metrics.blocks_closed.fetch_add(1, Ordering::Relaxed);
                        if pinned.is_none() && repin_due {
                            if let Ok(n) = client.block_number() {
                                block = BlockTag::Number(n);
                                exec.repin(RpcStateFetcher::new(&client, block));
                                composed.clear();
                            }
                            last_refresh = Instant::now();
                            repin_due = false;
                        }
                    }
                    // [B5] The feed regressed DEEPER than one block (a reorg). Under
                    // MIRROR_REORG_UNWIND, actively repair the mirror; otherwise fall through to
                    // today's behavior (no active handling — byte-identical to the pre-B5 advancer).
                    FeedAdvance::DeepRegress { target } if reorg_unwind => {
                        // [WP4 · Fix E] Deep reorg recovery. Under MIRROR_COLD_VERIFY=1 use the CHECKED,
                        // all-or-none path: PLAN the unwind WITHOUT mutating, VERIFY the complete
                        // canonical-at-`target` footprint through the same S5 boundary as normal
                        // reconcile, and COMMIT (mirror patch + journal prune together) ONLY if every
                        // value verifies — never mutate-then-fetch.
                        //
                        // Under =0 the reconcile itself is the pre-WP1 B5 exact-inverse rollback +
                        // infallible reconcile — but the =0 REORG path is TRUST-EQUIVALENT to (strictly
                        // safer than) original B5, NOT byte-identical, with two intentional deltas that
                        // only ever WITHHOLD trust, never grant it:
                        //   (1) the reorg publish is now gated on `is_healthy()` + debt-empty + no
                        //       resync-latch (`reorg_publishable`), where original B5 published a reorg
                        //       unconditionally;
                        //   (2) a beyond-journal =0 reorg degrades via a QUEUE_GAP (heal-on-clean-close),
                        //       whereas the checked path latches the sticky FULL_RESYNC_REQUIRED.
                        // (The validator's legacy `MirroredStateFetcher::new` live-map read the adversary
                        // flagged is NOT on this mirror-advance trust path: REVM_STATE=mirror uses
                        // `new_published`, which trusts ONLY certified snapshots; the live `new` path is
                        // the untrusted rpc-mode/legacy wiring, so a mid-reorg live read can't be served
                        // as trusted mirror state.)
                        if stage5.cold_verify {
                            match journal.plan_rollback_to(target) {
                                RollbackTo::Unwound { footprint, blocks_unwound } => {
                                    let verifying = ProofVerifyingFetcher::new(
                                        RpcStateFetcher::new(&client, BlockTag::Number(target)),
                                        &client,
                                        target,
                                        true,
                                    );
                                    let deadline = reconcile_deadline(true, reconcile_budget_ms(), Instant::now());
                                    let out = crate::mirror_journal::reconcile_footprint_checked(
                                        mirror, &footprint, &verifying, deadline,
                                    );
                                    if out.checked > 0 {
                                        metrics.reconcile_checked.fetch_add(out.checked as u64, Ordering::Relaxed);
                                    }
                                    if out.mismatched > 0 {
                                        metrics.reconcile_mismatches.fetch_add(out.mismatched as u64, Ordering::Relaxed);
                                    }
                                    if out.repaired > 0 {
                                        metrics.reconcile_repairs.fetch_add(out.repaired as u64, Ordering::Relaxed);
                                    }
                                    if out.complete {
                                        // Every footprint value verified + applied → COMMIT the journal
                                        // prune (all-or-none with the mirror patch), clear the reorg
                                        // reason, re-anchor, publish CERTIFIED.
                                        journal.commit_rollback_prune(target, blocks_unwound);
                                        mirror.clear_reason(health_reason::REORG_RECOVERY);
                                        if pinned.is_none() {
                                            block = BlockTag::Number(target);
                                            exec.repin(RpcStateFetcher::new(&client, block));
                                            composed.clear();
                                            last_refresh = Instant::now();
                                            repin_due = false;
                                        }
                                        gap_in_block = false;
                                        cur_block = target;
                                        mirror.retag_block(target);
                                        if publish_snapshots && journal.debt().is_empty() && mirror.is_healthy() {
                                            mirror.publish_certified(target);
                                        }
                                        metrics.blocks_closed.fetch_add(1, Ordering::Relaxed);
                                        info!(
                                            target,
                                            blocks_unwound,
                                            footprint = footprint.len(),
                                            "MIRROR_FEED_ADVANCE deep reorg — CHECKED unwind verified + committed"
                                        );
                                    } else {
                                        // Checked-fetch/anchor failure → ZERO mirror mutation, ZERO
                                        // prune (reconcile_footprint_checked applied nothing). [FIX-A]
                                        // Latch the mirror UNTRUSTED for a REAL re-hydration: the sticky
                                        // FULL_RESYNC_REQUIRED (heal-excluded, cleared only by an actual
                                        // resync) is the trust gate, REORG_RECOVERY the diagnostic marker
                                        // — so the "full resync" is honestly pending and a later reorg on
                                        // a different footprint can NOT launder this one. Drop the orphaned
                                        // journal (the sticky latch survives), re-anchor for RPC fallback,
                                        // publish nothing.
                                        if mirror.is_healthy() {
                                            metrics.unhealthy_trips.fetch_add(1, Ordering::Relaxed);
                                        }
                                        // [FIX-D] A reorg-time proof failure is observable on a =1 soak as
                                        // BOTH a cold-verify failure and an incomplete checked reconcile,
                                        // not only a reorg-unproven skip.
                                        metrics.reorg_unproven_skips.fetch_add(1, Ordering::Relaxed);
                                        metrics.cold_verify_failures.fetch_add(1, Ordering::Relaxed);
                                        metrics.reconcile_incomplete.fetch_add(1, Ordering::Relaxed);
                                        latch_reorg_untrusted(mirror, &mut journal);
                                        if pinned.is_none() {
                                            block = BlockTag::Number(target);
                                            exec.repin(RpcStateFetcher::new(&client, block));
                                            composed.clear();
                                            last_refresh = Instant::now();
                                            repin_due = false;
                                        }
                                        gap_in_block = true;
                                        cur_block = target;
                                        mirror.retag_block(target);
                                        info!(target, "MIRROR_FEED_ADVANCE deep reorg — CHECKED verify FAILED → untrusted + full resync LATCHED (until re-hydration)");
                                    }
                                }
                                RollbackTo::BeyondJournal => {
                                    // [FIX-A] Target below the journal ring: the unwind footprint is
                                    // unknowable, so this is likewise latched UNTRUSTED for a real
                                    // re-hydration (sticky FULL_RESYNC_REQUIRED), not just marked with the
                                    // launderable REORG_RECOVERY. Honest "full resync" pending.
                                    journal.note_reorg_beyond_journal();
                                    if mirror.is_healthy() {
                                        metrics.unhealthy_trips.fetch_add(1, Ordering::Relaxed);
                                    }
                                    latch_reorg_untrusted(mirror, &mut journal);
                                    if pinned.is_none() {
                                        block = BlockTag::Number(target);
                                        exec.repin(RpcStateFetcher::new(&client, block));
                                        composed.clear();
                                        last_refresh = Instant::now();
                                        repin_due = false;
                                    }
                                    gap_in_block = true;
                                    cur_block = target;
                                    mirror.retag_block(target);
                                    info!(target, "MIRROR_FEED_ADVANCE deep reorg BEYOND JOURNAL (checked) — untrusted + full resync LATCHED (until re-hydration)");
                                }
                                RollbackTo::NoOp => {
                                    cur_block = target;
                                    mirror.retag_block(target);
                                }
                            }
                        } else {
                            match journal.rollback_to(mirror, target) {
                                RollbackTo::Unwound { footprint, blocks_unwound } => {
                                    let canon = RpcStateFetcher::new(&client, BlockTag::Number(target));
                                    let rep = crate::mirror_journal::reconcile_footprint(
                                        mirror, &footprint, &canon,
                                    );
                                    if rep.checked > 0 {
                                        metrics.reconcile_checked.fetch_add(rep.checked as u64, Ordering::Relaxed);
                                    }
                                    if rep.mismatched > 0 {
                                        metrics.reconcile_mismatches.fetch_add(rep.mismatched as u64, Ordering::Relaxed);
                                    }
                                    if rep.repaired > 0 {
                                        metrics.reconcile_repairs.fetch_add(rep.repaired as u64, Ordering::Relaxed);
                                    }
                                    if pinned.is_none() {
                                        block = BlockTag::Number(target);
                                        exec.repin(RpcStateFetcher::new(&client, block));
                                        composed.clear();
                                        last_refresh = Instant::now();
                                        repin_due = false;
                                    }
                                    gap_in_block = false;
                                    cur_block = target;
                                    mirror.retag_block(target);
                                    if reorg_publishable(
                                        publish_snapshots,
                                        false,
                                        journal.debt().is_empty(),
                                        journal.full_resync_pending(),
                                        mirror.is_healthy(),
                                    ) {
                                        mirror.publish_certified(target);
                                    }
                                    metrics.blocks_closed.fetch_add(1, Ordering::Relaxed);
                                    info!(
                                        target,
                                        blocks_unwound,
                                        footprint = footprint.len(),
                                        reconcile_checked = rep.checked,
                                        reconcile_repaired = rep.repaired,
                                        "MIRROR_FEED_ADVANCE deep reorg — B5 unwind + reconcile (cold_verify off)"
                                    );
                                }
                                RollbackTo::BeyondJournal => {
                                    // =0 byte-identical trust behavior: degrade via a queue gap
                                    // (QUEUE_GAP heals on the next clean close) + reset + re-anchor. Does
                                    // NOT set the sticky REORG_RECOVERY reason — that is the checked-path
                                    // latch; =0 keeps today's heal-on-clean-close behavior.
                                    journal.reset();
                                    if pinned.is_none() {
                                        block = BlockTag::Number(target);
                                        exec.repin(RpcStateFetcher::new(&client, block));
                                        composed.clear();
                                        last_refresh = Instant::now();
                                        repin_due = false;
                                    }
                                    gap_in_block = true;
                                    cur_block = target;
                                    mirror.retag_block(target);
                                    info!(target, "MIRROR_FEED_ADVANCE deep reorg BEYOND JOURNAL (cold_verify off) — gap degrade + resync");
                                }
                                RollbackTo::NoOp => {
                                    cur_block = target;
                                    mirror.retag_block(target);
                                }
                            }
                        }
                    }
                    // Deep regression with the flag OFF → no active handling (today's behavior).
                    FeedAdvance::DeepRegress { .. } => {}
                }
            }
        }
        // Degraded-mode honesty: a lossless-queue gap means the current block may be missing a tx we
        // never saw. Flag it for forced reconcile and trip unhealthy (a later clean, gapless block
        // reconcile clears it). NEVER a silent skip.
        let g = feed.gaps.load(Ordering::Relaxed);
        if g > last_gaps {
            metrics.gaps.fetch_add(g - last_gaps, Ordering::Relaxed);
            last_gaps = g;
            gap_in_block = true;
            if cur_block > 0 {
                journal.flag_reconcile(cur_block);
            }
            mirror.set_reason(health_reason::QUEUE_GAP);
        }
        // Lag = feed head seq − last drained seq.
        metrics.lag_seq.store(
            feed.head_seq
                .load(Ordering::Relaxed)
                .saturating_sub(metrics.last_applied_seq.load(Ordering::Relaxed)),
            Ordering::Relaxed,
        );

        let Some(job) = feed.queue.pop() else {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        };
        // No freshness cutoff: the mirror must apply losslessly. (Row-7 wakes ride the same replay
        // and thereby cover the full stream — the plan's "row-7 wakes improve".)
        stats.jobs.fetch_add(1, Ordering::Relaxed);
        if let Some(clock) = clock {
            let (n, ts) = (clock.last_block_number(), clock.last_block_timestamp_secs());
            if n > 0 && ts > 0 && exec.block_env() != Some((n, ts)) {
                exec.set_block_env(n, ts);
            }
        }
        let tq = if emit { Some(targeted_q) } else { None };
        let seq = job.seq;
        {
            let mut ctx = AdvanceCtx {
                journal: &mut journal,
                mirror,
                block: cur_block,
                metrics,
            };
            process_job(
                &job,
                &mut exec,
                &needles,
                stats,
                &client,
                block,
                tq,
                detection,
                state_sender,
                state_emit,
                promotion_enabled,
                &mut composed,
                Some(&mut ctx),
            );
        }
        metrics.last_applied_seq.store(seq, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_jobs_are_skipped_fresh_jobs_pass() {
        let ms = 1_000_000u64; // ns per ms
                               // Exactly at the bound → still fresh; one ns past → stale.
        assert!(shadow_job_is_fresh(
            0,
            SHADOW_MAX_JOB_AGE_MS * ms,
            SHADOW_MAX_JOB_AGE_MS
        ));
        assert!(!shadow_job_is_fresh(
            0,
            SHADOW_MAX_JOB_AGE_MS * ms + 1,
            SHADOW_MAX_JOB_AGE_MS
        ));
        // Clock skew (recv in the future) must never panic and counts as fresh.
        assert!(shadow_job_is_fresh(10 * ms, 5 * ms, SHADOW_MAX_JOB_AGE_MS));
    }

    #[test]
    fn record_composition_commits_success_and_skips_failure() {
        use engine_revm::{ChangedSlot, ReplayOutcome, StateFetcher};
        struct NullFetcher;
        impl StateFetcher for NullFetcher {
            fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
                (U256::ZERO, 0, None)
            }
            fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
                U256::ZERO
            }
        }
        let pool = [0x41; 20];
        let token = [0x42; 20];
        let outcome = |success: bool| ReplayOutcome {
            success,
            output: Vec::new(),
            reason: String::new(),
            changed: vec![pool, token],
            changed_slots: vec![
                ChangedSlot {
                    address: pool,
                    slot: U256::ZERO,
                    original: U256::ZERO,
                    present: U256::from(7u64),
                },
                ChangedSlot {
                    address: token,
                    slot: U256::from(1u64),
                    original: U256::ZERO,
                    present: U256::from(9u64),
                },
            ],
        };
        let mut composed = ahash::AHashSet::new();
        // A REVERTED replay leaves no state on the real chain — nothing committed or recorded.
        let mut exec = ForkExecutor::new(NullFetcher);
        assert!(!record_composition(&mut exec, &outcome(false), &mut composed));
        assert!(composed.is_empty());
        assert_eq!(exec.storage(pool, U256::ZERO), U256::ZERO);
        // A successful replay commits its deltas (the NEXT replay composes) and records the window.
        assert!(record_composition(&mut exec, &outcome(true), &mut composed));
        assert_eq!(exec.storage(pool, U256::ZERO), U256::from(7u64));
        assert_eq!(exec.storage(token, U256::from(1u64)), U256::from(9u64));
        assert!(composed.contains(&pool) && composed.contains(&token));
    }

    /// EVM runtime that returns storage slot 4 for ANY call — a minimal `liquidity()` getter:
    /// `PUSH1 4 SLOAD PUSH1 0 MSTORE PUSH1 32 PUSH1 0 RETURN`.
    fn v3_liquidity_getter_bytecode() -> Vec<u8> {
        vec![0x60, 0x04, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3]
    }

    /// A fork whose POOL exposes `liquidity()` (slot 4) = `liq`, deliberately DIFFERENT from the
    /// resident base — the lagging-fork / lagging-resident divergence the CAS must catch. Code is
    /// returned ONLY for the pool (the caller must stay code-less — EIP-3607 rejects a sender with code).
    struct V3LiqFetcher {
        pool: [u8; 20],
        liq: U256,
    }
    impl engine_revm::StateFetcher for V3LiqFetcher {
        fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            let code = (a == self.pool).then(v3_liquidity_getter_bytecode);
            (U256::from(u128::MAX), 0, code)
        }
        fn fetch_storage(&self, a: [u8; 20], slot: U256) -> U256 {
            if a == self.pool && slot == U256::from(4u8) {
                self.liq
            } else {
                U256::ZERO
            }
        }
    }

    #[test]
    fn v3_liquidity_selector_matches_engine_rpc() {
        // The local const must stay byte-equal to engine-rpc's canonical `liquidity()` selector.
        assert_eq!(V3_LIQUIDITY_SELECTOR, [0x1a, 0x68, 0x65, 0x02]);
    }

    #[test]
    fn shadow_v3_pre_image_liquidity_is_fork_read_not_resident() {
        // WI-06 T3 fix round 6 K1 (derivation seam — BOTH round-6 verifiers' blocker): the V3 CAS
        // pre-image liquidity must be the FORK's ACTUAL liquidity (read over the non-overlaid fork),
        // NEVER the resident base. Round 5 copied `base.liquidity`, making the CAS liquidity term
        // `resident == resident` — a lagging in-range Mint/Burn that moved resident L (but not the
        // fork's) then passed the compare and blessed a sqrt/tick computed under stale L. This drives
        // the REAL `derive_v3_shadow_states` data flow (real ForkExecutor, real getter read, real
        // reconstruction) with a fork liquidity DIFFERENT from resident and IDENTICAL slot0 scalars.
        let pool = [0xd3u8; 20];
        let fork_liq: u128 = 5_000_000; // the fork's TRUE current liquidity
        let resident_liq: u128 = 9_999_999; // resident advanced (lagging in-range mint) — DIFFERENT
        let mut exec = ForkExecutor::new(V3LiqFetcher {
            pool,
            liq: U256::from(fork_liq),
        });
        // The reconstruction reads liquidity ONLY from the fork; `base` supplies swap-invariant fields.
        let base = V3State {
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336u128),
            tick: 100,
            liquidity: resident_liq,
            fee_pips: 3_000,
            tick_spacing: 60,
            initialised_ticks: Vec::new(),
        };
        // A NO-CROSS swap: only slot0 changed (slot 4 absent) — so the reconstruction MUST fall back
        // to the fork read, not to `base.liquidity`.
        let pre_word = base.sqrt_price_x96 | (U256::from(100u32) << 160);
        let post_word =
            base.sqrt_price_x96 | (U256::from(101u32 & 0x00FF_FFFF) << 160);
        let pool_slots = vec![ChangedSlot {
            address: pool,
            slot: U256::ZERO,
            original: pre_word,
            present: post_word,
        }];
        let (pre, post) = derive_v3_shadow_states(&mut exec, pool, &base, &pool_slots)
            .expect("no-cross V3 derivation must succeed");
        assert_eq!(
            pre.liquidity, fork_liq,
            "pre-image liquidity MUST be the fork read (slot 4), not resident base"
        );
        assert_ne!(
            pre.liquidity, resident_liq,
            "pre-image must NOT copy resident base.liquidity (the round-5 CAS tautology)"
        );
        // A no-cross swap does not move active liquidity, so the POST liquidity is the same fork value.
        assert_eq!(post.liquidity, fork_liq, "post liquidity is the fork value (no tick cross)");
    }

    /// EVM runtime dispatching on the 4-byte selector: `globalState()` (0xe76c01e4) returns 224 zero
    /// bytes (a decodable Algebra globalState); ANY other selector (i.e. `liquidity()`) REVERTS —
    /// modelling a pool whose liquidity getter is unavailable.
    fn algebra_globalstate_ok_liquidity_reverts_bytecode() -> Vec<u8> {
        vec![
            0x60, 0x00, // PUSH1 0
            0x35, // CALLDATALOAD
            0x60, 0xe0, // PUSH1 0xE0
            0x1c, // SHR  -> selector in low 4 bytes
            0x63, 0xe7, 0x6c, 0x01, 0xe4, // PUSH4 globalState()
            0x14, // EQ
            0x60, 0x14, // PUSH1 20 (globalState dest)
            0x57, // JUMPI
            0x60, 0x00, 0x60, 0x00, 0xfd, // REVERT (liquidity / other)
            0x5b, // JUMPDEST (offset 20)
            0x61, 0x00, 0xe0, // PUSH2 224
            0x60, 0x00, // PUSH1 0
            0xf3, // RETURN(0, 224)
        ]
    }

    struct AlgebraGetterFetcher {
        pool: [u8; 20],
    }
    impl engine_revm::StateFetcher for AlgebraGetterFetcher {
        fn fetch_account(&self, a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            let code = (a == self.pool).then(algebra_globalstate_ok_liquidity_reverts_bytecode);
            (U256::from(u128::MAX), 0, code)
        }
        fn fetch_storage(&self, _a: [u8; 20], _slot: U256) -> U256 {
            U256::ZERO
        }
    }

    #[test]
    fn shadow_algebra_getter_none_liquidity_declines_promotion() {
        // WI-06 T3 fix round 6 K4 (derivation seam): when the `liquidity()` getter reverts/absent, the
        // Algebra getter-overlay derivation DECLINES (None) — it must NOT seed the resident
        // `base.liquidity` (that forged pre-image, resident==resident, is the round-5 tautology). Real
        // getter-overlay (globalState() decodes, liquidity() reverts) + real decode + real `liq?`.
        let pool = [0xa9u8; 20];
        let mut exec = ForkExecutor::new(AlgebraGetterFetcher { pool });
        let base = V3State {
            sqrt_price_x96: U256::from(1u64),
            tick: 0,
            liquidity: 424_242, // the resident value a bug would forge into the pre-image
            fee_pips: 500,
            tick_spacing: 60,
            initialised_ticks: Vec::new(),
        };
        assert!(
            derive_algebra_v3state(&mut exec, pool, &base, &[]).is_none(),
            "a None liquidity() getter MUST decline the promotion (pre-fix seeded resident base.liquidity)"
        );
    }

    #[test]
    fn shadow_v3_tick_cross_replay_declines_promotion() {
        // WI-06 T3 fix round 6 K2 (derivation seam): a tick-crossing replay writes slot 4 — it
        // consumed the fork's tick set to compute the post-swap liquidity via the crossed tick's net.
        // A fork whose tick set LAGS a resident out-of-range Mint/Burn yields a stale post-liquidity a
        // PRE-value CAS cannot catch, so the V3 promotion is DECLINED by the fork-side belt (belt-only;
        // the publisher's tick-unchanged + interior-landing gate is the actual guarantee). No-cross swaps
        // (the control) are still promoted.
        let pool = [0xd4u8; 20];
        let mut exec = ForkExecutor::new(V3LiqFetcher {
            pool,
            liq: U256::from(5_000_000u128),
        });
        let base = V3State {
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336u128),
            tick: 100,
            liquidity: 5_000_000,
            fee_pips: 3_000,
            tick_spacing: 60,
            initialised_ticks: Vec::new(),
        };
        let pre_word = base.sqrt_price_x96 | (U256::from(100u32) << 160);
        let cross_word = base.sqrt_price_x96 | (U256::from(240u32) << 160);
        let slot0 = ChangedSlot {
            address: pool,
            slot: U256::ZERO,
            original: pre_word,
            present: cross_word,
        };
        let slot4_written = ChangedSlot {
            address: pool,
            slot: U256::from(4u8),
            original: U256::from(5_000_000u128),
            present: U256::from(7_000_000u128),
        };
        assert!(
            derive_v3_shadow_states(&mut exec, pool, &base, &[slot0, slot4_written]).is_none(),
            "a tick-crossing replay (slot 4 present) MUST decline the V3 promotion"
        );
        // Control: the identical swap WITHOUT the slot-4 write (no cross) is still promoted.
        assert!(
            derive_v3_shadow_states(&mut exec, pool, &base, &[slot0]).is_some(),
            "a no-cross swap (slot 4 absent) is still promoted"
        );
    }

    #[test]
    fn parity_classification_pass_fail_err() {
        let word = |b: u8| -> [u8; 32] { [b; 32] };
        let original = U256::from_be_bytes(word(0x11));
        assert_eq!(
            classify_parity(Ok(word(0x11)), original),
            ParityOutcome::Pass
        );
        assert_eq!(
            classify_parity(Ok(word(0x22)), original),
            ParityOutcome::Fail
        );
        assert_eq!(
            classify_parity(
                Err(engine_rpc::RpcError::Transport("boom".into())),
                original
            ),
            ParityOutcome::Err,
            "an errored read is a distinct, COUNTED outcome — not a silent skip"
        );
    }

    #[test]
    fn shadow_enqueue_keeps_newest_evicts_oldest() {
        let q: ArrayQueue<ShadowJob> = ArrayQueue::new(1);
        let stats = ShadowStats::default();
        let job = |seq: u64| ShadowJob::owned(vec![], seq, seq);
        shadow_enqueue(&q, &stats, job(1));
        shadow_enqueue(&q, &stats, job(2));
        let kept = q.pop().expect("one job kept");
        assert_eq!(kept.seq, 2, "overflow must keep the NEWEST job");
        assert_eq!(
            stats.dropped.load(Ordering::Relaxed),
            1,
            "the evicted oldest counts as dropped"
        );
    }

    #[test]
    fn pooled_shadow_job_reuses_exact_buffer_and_advancer_exhaustion_is_a_gap() {
        let pool = Arc::new(ArrayQueue::new(1));
        pool.push(Vec::with_capacity(64)).unwrap();
        let first = pool.pop().unwrap();
        let ptr = first.as_ptr();
        pool.push(first).unwrap();

        let job = ShadowJob::from_pool(&pool, b"signed-envelope", 7, 8).unwrap();
        assert_eq!(job.envelope, b"signed-envelope");
        assert_eq!(job.envelope.as_ptr(), ptr, "no replacement allocation");
        assert!(pool.is_empty(), "job exclusively owns its pooled Vec");
        drop(job);
        let returned = pool.pop().unwrap();
        assert_eq!(returned.as_ptr(), ptr, "Drop returns the same Vec");

        let empty_pool = Arc::new(ArrayQueue::new(1));
        let feed = AdvanceFeed::with_envelope_pool(1, empty_pool.clone());
        assert!(ShadowJob::from_pool(&empty_pool, b"tx", 9, 10).is_none());
        advance_pool_gap(&feed, 10);
        assert_eq!(feed.head_seq.load(Ordering::Relaxed), 10);
        assert_eq!(feed.gaps.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn advance_enqueue_is_lossless_ordered_rejects_newest_on_overflow() {
        // The advancer's queue is the OPPOSITE of the sampled one: it must apply the ordered prefix
        // losslessly, so on overflow it drops the NEWEST (records a gap) and keeps the older tx in
        // flight — never evicting a tx already queued. head_seq tracks the true feed head regardless.
        let feed = AdvanceFeed::new(1);
        let job = |seq: u64| ShadowJob::owned(vec![], seq, seq);
        advance_enqueue(&feed, job(1));
        advance_enqueue(&feed, job(2)); // overflow → dropped (newest), gap recorded
        let kept = feed.queue.pop().expect("the older job is kept");
        assert_eq!(kept.seq, 1, "overflow keeps the OLDER (in-flight ordered) job");
        assert_eq!(
            feed.gaps.load(Ordering::Relaxed),
            1,
            "the rejected newest is a counted gap (forces a block reconcile)"
        );
        assert_eq!(
            feed.head_seq.load(Ordering::Relaxed),
            2,
            "head_seq advances to the true feed head even on a dropped push (for mirror_lag_seq)"
        );
    }

    #[test]
    fn reconcile_health_reasons_and_heal_are_correct() {
        use super::reconcile_health;
        use crate::state_mirror::health_reason as hr;
        // WP2 RED-1/RED-3: proof/debt DOMINATES; gap sets QUEUE_GAP; gross sets GROSS_DIVERGENCE;
        // checked==0 never heals; a clean checked block heals the reconcile group.
        // Clean, gapless, checked → no set, HEAL.
        let c = reconcile_health(true, 0, false, 64, 0, false, 50);
        assert_eq!(c.set, 0);
        assert!(c.heal, "clean checked block heals");
        // A gap sets QUEUE_GAP and never heals (even with a clean sample).
        let g = reconcile_health(true, 0, false, 64, 0, true, 50);
        assert_eq!(g.set, hr::QUEUE_GAP);
        assert!(!g.heal);
        // Gross divergence (≥ trip%) sets GROSS_DIVERGENCE, no heal.
        let gross = reconcile_health(true, 0, false, 10, 5, false, 50);
        assert_eq!(gross.set, hr::GROSS_DIVERGENCE);
        assert!(!gross.heal);
        // Benign skew (< trip%) → no set, heals.
        let skew = reconcile_health(true, 0, false, 100, 3, false, 50);
        assert_eq!(skew.set, 0);
        assert!(skew.heal);
        // RED-1: a proof failure / outstanding debt DOMINATES a low mismatch% + would-be heal.
        let proof = reconcile_health(true, 0, true /*debt*/, 64, 0, false, 50);
        assert_eq!(proof.set, hr::PROOF_UNRESOLVED, "debt ⇒ PROOF_UNRESOLVED even at 0% mismatch");
        assert!(!proof.heal, "proof debt never heals");
        let fail = reconcile_health(false, 1, false, 0, 0, false, 50);
        assert_eq!(fail.set, hr::PROOF_UNRESOLVED, "canonical failure/incomplete ⇒ PROOF_UNRESOLVED");
        assert!(!fail.heal);
        // RED-3: checked==0 with no gap/debt/failure ⇒ no set, no heal (never a proof of health).
        let empty = reconcile_health(true, 0, false, 0, 0, false, 50);
        assert_eq!(empty.set, 0);
        assert!(!empty.heal, "checked==0 cannot heal");
        // Nothing checked but a gap → QUEUE_GAP, no heal.
        let empty_gap = reconcile_health(true, 0, false, 0, 0, true, 50);
        assert_eq!(empty_gap.set, hr::QUEUE_GAP);
        assert!(!empty_gap.heal);
    }

    #[test]
    fn advance_enqueue_tracks_monotonic_head_seq() {
        let feed = AdvanceFeed::new(8);
        let job = |seq: u64| ShadowJob::owned(vec![], seq, seq);
        advance_enqueue(&feed, job(5));
        advance_enqueue(&feed, job(3)); // out-of-order arrival must not lower head_seq
        advance_enqueue(&feed, job(9));
        assert_eq!(feed.head_seq.load(Ordering::Relaxed), 9);
        assert_eq!(feed.gaps.load(Ordering::Relaxed), 0, "no overflow → no gaps");
    }

    #[test]
    fn classify_feed_advance_routes_hold_forward_shallow_deep() {
        use super::{classify_feed_advance, FeedAdvance};
        // No feed head yet, or head == working block → Hold (no boundary action).
        assert_eq!(classify_feed_advance(0, 5), FeedAdvance::Hold);
        assert_eq!(classify_feed_advance(5, 5), FeedAdvance::Hold);
        // Head advanced → Forward (the existing close + advance path).
        assert_eq!(classify_feed_advance(6, 5), FeedAdvance::Forward);
        assert_eq!(classify_feed_advance(9, 5), FeedAdvance::Forward);
        // Head regressed by EXACTLY one block → shallow: stays on the existing per-block reconcile +
        // health path (no active unwind — no behavior change).
        assert_eq!(classify_feed_advance(4, 5), FeedAdvance::ShallowRegress);
        // Head regressed DEEPER than one block → active journal-driven unwind to the regressed-to
        // block (the `target`).
        assert_eq!(classify_feed_advance(3, 5), FeedAdvance::DeepRegress { target: 3 });
        assert_eq!(classify_feed_advance(1, 5), FeedAdvance::DeepRegress { target: 1 });
    }

    // ── [WP1] close_block: checked reconcile → certified publish ─────────────────────────────────

    /// A mock RPC serving `eth_getStorageAt = storage_value` and COUNTING `eth_getProof` +
    /// `eth_getBlockByNumber` (returned as null) — so a test can prove the MIRROR_COLD_VERIFY=0 close
    /// issues ZERO proof/header RPC. Returns `(port, proof_hits, header_hits)`.
    fn spawn_counting_rpc(
        storage_value: alloy_primitives::U256,
    ) -> (
        u16,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    ) {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering::SeqCst;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let proof_hits = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let header_hits = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (ph, hh) = (proof_hits.clone(), header_hits.clone());
        std::thread::spawn(move || {
            let val_hex = format!(
                "0x{}",
                alloy_primitives::hex::encode(storage_value.to_be_bytes::<32>())
            );
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let Ok(n) = sock.read(&mut tmp) else { break };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let body_start = p + 4;
                    let clen: usize = String::from_utf8_lossy(&buf[..body_start])
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while buf.len() < body_start + clen {
                        let Ok(n) = sock.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        match serde_json::from_slice(&buf[body_start..body_start + clen]) {
                            Ok(v) => v,
                            Err(_) => break,
                        };
                    let answer = |item: &serde_json::Value| -> serde_json::Value {
                        let id = item["id"].clone();
                        let result = match item["method"].as_str().unwrap_or("") {
                            "eth_getStorageAt" => serde_json::Value::String(val_hex.clone()),
                            "eth_getProof" => {
                                ph.fetch_add(1, SeqCst);
                                serde_json::Value::Null
                            }
                            "eth_getBlockByNumber" => {
                                hh.fetch_add(1, SeqCst);
                                serde_json::Value::Null
                            }
                            _ => serde_json::Value::String("0x0".into()),
                        };
                        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
                    };
                    let reply = if let Some(arr) = body.as_array() {
                        serde_json::Value::Array(arr.iter().map(answer).collect()).to_string()
                    } else {
                        answer(&body).to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.len(),
                        reply
                    );
                    let _ = sock.write_all(resp.as_bytes());
                    break;
                }
            }
        });
        (port, proof_hits, header_hits)
    }

    fn touch(pool: [u8; 20], slot: u64, value: alloy_primitives::U256) -> engine_revm::ChangedSlot {
        engine_revm::ChangedSlot {
            address: pool,
            slot: alloy_primitives::U256::from(slot),
            original: value,
            present: value,
        }
    }

    #[test]
    fn close_block_cold_verify_off_is_publishable_and_zero_proof_rpc() {
        // WP1 RED-11 + healthy path: MIRROR_COLD_VERIFY=0 constructs no proof adapter and issues ZERO
        // proof/header RPC; a clean reconcile heals and is publishable.
        use std::sync::atomic::Ordering::SeqCst;
        let value = alloy_primitives::U256::from(0x1234u64);
        let (port, proof_hits, header_hits) = spawn_counting_rpc(value);
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let mirror = StateMirror::new();
        let pool = [0xC1u8; 20];
        mirror.apply_batch([(pool, alloy_primitives::U256::ZERO, value)]);
        let mut journal = MirrorJournal::new(8);
        journal.record_and_apply(&mirror, 100, [1u8; 32], &[touch(pool, 0, value)]);
        let metrics = AdvancerMetrics::default();
        assert!(mirror.is_healthy());
        let out = close_block(
            &mut journal, &mirror, 100, &client, ReconcileMode::Logs, 64, 50, false, &metrics, false, 0,
        );
        assert!(out.publishable, "a clean cold_verify=off reconcile is publishable");
        assert!(out.complete);
        assert!(mirror.is_healthy(), "a clean checked reconcile heals");
        assert_eq!(proof_hits.load(SeqCst), 0, "MIRROR_COLD_VERIFY=0 issues ZERO eth_getProof");
        assert_eq!(header_hits.load(SeqCst), 0, "MIRROR_COLD_VERIFY=0 issues ZERO eth_getBlockByNumber");
    }

    #[test]
    fn close_block_queue_gap_is_not_publishable_and_trips_unhealthy() {
        // WP1: a lossless-queue gap means the block is not trustworthy — not publishable and the
        // mirror trips unhealthy, even when the reconcile sample itself was clean.
        let value = alloy_primitives::U256::from(7u64);
        let (port, _p, _h) = spawn_counting_rpc(value);
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let mirror = StateMirror::new();
        let pool = [0xC2u8; 20];
        mirror.apply_batch([(pool, alloy_primitives::U256::ZERO, value)]);
        let mut journal = MirrorJournal::new(8);
        journal.record_and_apply(&mirror, 100, [2u8; 32], &[touch(pool, 0, value)]);
        let metrics = AdvancerMetrics::default();
        assert!(mirror.is_healthy());
        let out = close_block(
            &mut journal, &mirror, 100, &client, ReconcileMode::Logs, 64, 50, true, &metrics, false, 0,
        );
        assert!(!out.publishable, "a gap block is never publishable");
        assert!(!mirror.is_healthy(), "a queue gap trips the mirror unhealthy");
    }

    #[test]
    fn close_block_full_resync_latch_blocks_heal_and_publish() {
        // WP1 FIX-1: with the sticky full-resync latch set (a prior debt-cap escalation), a later
        // CLEAN close does NOT re-heal and does NOT publish — the never-re-hydrated mirror stays
        // untrusted. The control (latch cleared) proves the same clean close would otherwise publish.
        //
        // [WP3 carry-in (b) — reassessed, document-and-leave] This test drives the FULL `close_block`,
        // which builds its own canonical fetcher (`RpcStateFetcher`, wrapped in `ProofVerifyingFetcher`
        // under cold_verify) against the in-process TCP mock below. Under heavy PARALLEL test load the
        // mock's single accept loop can be starved past the client read-timeout, so the CONTROL close
        // (the second, unlatched publish) flakes ~1/16; it passes with `--test-threads=1`. Making it
        // hermetic means injecting the fetcher — but `close_block` is non-generic, constructs the
        // fetcher internally with cold_verify TYPE VARIANCE, and is called at six sites including the
        // WP1–WP5-gated advancer, so that is a signature-level refactor of a five-times-gated component.
        // The refactor's destabilization risk outweighs a 1/16 harness flake, ESPECIALLY because the
        // same latch→no-heal→no-publish LOGIC is already covered hermetically (no TCP) by the pure
        // `reconcile_health_reasons_and_heal_are_correct`, `reorg_publishable_gate_is_symmetric_with_
        // forward_publish`, and `latch_reorg_untrusted_is_sticky_and_not_launderable` tests. Kept as an
        // end-to-end smoke; run `-j1` if it flaps.
        let value = alloy_primitives::U256::from(5u64);
        let (port, _p, _h) = spawn_counting_rpc(value);
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let pool = [0xC3u8; 20];

        let mirror = StateMirror::new();
        mirror.apply_batch([(pool, alloy_primitives::U256::ZERO, value)]);
        let mut journal = MirrorJournal::new(8);
        journal.record_and_apply(&mirror, 100, [3u8; 32], &[touch(pool, 0, value)]);
        journal.set_full_resync_pending(); // simulate a prior escalation
        mirror.set_reason(health_reason::FULL_RESYNC_REQUIRED);
        let metrics = AdvancerMetrics::default();
        let out = close_block(
            &mut journal, &mirror, 100, &client, ReconcileMode::Logs, 64, 50, false, &metrics, false, 0,
        );
        assert!(!out.publishable, "a resync-latched mirror never publishes on a clean close");
        assert!(!mirror.is_healthy(), "a clean close cannot heal a resync-latched mirror");

        // Control: the identical clean close on an UNLATCHED mirror publishes + heals.
        let mirror2 = StateMirror::new();
        mirror2.apply_batch([(pool, alloy_primitives::U256::ZERO, value)]);
        let mut journal2 = MirrorJournal::new(8);
        journal2.record_and_apply(&mirror2, 100, [4u8; 32], &[touch(pool, 0, value)]);
        let out2 = close_block(
            &mut journal2, &mirror2, 100, &client, ReconcileMode::Logs, 64, 50, false, &metrics, false, 0,
        );
        assert!(out2.publishable && mirror2.is_healthy(), "control: unlatched clean close publishes + heals");
    }

    #[test]
    fn reconcile_deadline_only_applies_on_cold_verify() {
        // WP1 FIX-3: the reconcile budget is applied ONLY on the checked-proof path. MIRROR_COLD_VERIFY=0
        // never gets a deadline (byte-identical to legacy), so a slow =0 reconcile cannot open debt.
        let now = std::time::Instant::now();
        assert!(reconcile_deadline(false, 5000, now).is_none(), "=0 never gets a deadline");
        assert!(reconcile_deadline(false, 1, now).is_none(), "=0 never gets a deadline, any budget");
        assert!(reconcile_deadline(true, 5000, now).is_some(), "=1 gets a deadline when budget>0");
        assert!(reconcile_deadline(true, 0, now).is_none(), "budget=0 disables the deadline");
    }

    #[test]
    fn close_block_clean_close_does_not_heal_reorg_recovery() {
        // WP4 · F1: REORG_RECOVERY is out of the close_block heal group — a clean forward close cannot
        // launder a failed checked reorg's trip. Only a later checked reorg reconcile (or full resync)
        // clears it.
        let value = alloy_primitives::U256::from(5u64);
        let (port, _p, _h) = spawn_counting_rpc(value);
        let client = ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"));
        let mirror = StateMirror::new();
        let pool = [0xC4u8; 20];
        mirror.apply_batch([(pool, alloy_primitives::U256::ZERO, value)]);
        let mut journal = MirrorJournal::new(8);
        journal.record_and_apply(&mirror, 100, [5u8; 32], &[touch(pool, 0, value)]);
        mirror.set_reason(health_reason::REORG_RECOVERY); // simulate a failed checked reorg
        let metrics = AdvancerMetrics::default();
        let out = close_block(
            &mut journal, &mirror, 100, &client, ReconcileMode::Logs, 64, 50, false, &metrics, false, 0,
        );
        assert!(
            mirror.has_reason(health_reason::REORG_RECOVERY),
            "a clean forward close must NOT heal REORG_RECOVERY (F1: dropped from the heal group)"
        );
        assert!(!out.publishable, "a REORG_RECOVERY-latched mirror is not publishable");
        assert!(!mirror.is_healthy());
    }

    #[test]
    fn reorg_publishable_gate_is_symmetric_with_forward_publish() {
        // WP4 · Fix E + F2: the =0 reorg publish gate. The checked =1 path owns its own certified
        // publish inline (never via this helper), so this gate is `false` under cold_verify. Under =0
        // it publishes only with snapshots on, no debt, no resync latch, AND a healthy mirror (F2 — the
        // `is_healthy()` term makes it symmetric with the forward close_block publish gate).
        // signature: reorg_publishable(publish_snapshots, cold_verify, debt_empty, resync_latched, is_healthy)
        assert!(
            reorg_publishable(true, false, true, false, true),
            "=0 healthy reorg with snapshots on + no debt/latch publishes (trust-equivalent to B5)"
        );
        assert!(!reorg_publishable(true, true, true, false, true), "cold_verify never uses this gate");
        assert!(!reorg_publishable(false, false, true, false, true), "snapshots off ⇒ no reorg publish");
        assert!(!reorg_publishable(true, false, false, false, true), "outstanding debt blocks reorg publish");
        assert!(!reorg_publishable(true, false, true, true, true), "resync latch blocks reorg publish");
        assert!(
            !reorg_publishable(true, false, true, false, false),
            "F2: an unhealthy mirror blocks the reorg publish (symmetry with forward publish)"
        );
    }

    #[test]
    fn latch_reorg_untrusted_is_sticky_and_not_launderable() {
        // WP4 · FIX-A: a deep reorg whose canonical footprint could NOT be proven (checked-fetch/anchor
        // failure or beyond-journal) must latch the mirror UNTRUSTED for a REAL re-hydration — not just
        // the diagnostic REORG_RECOVERY, which a later *successful* checked reorg clears at its commit
        // site. RED before FIX-A (the sites set only REORG_RECOVERY): clearing REORG_RECOVERY would
        // re-trust a mirror whose earlier failed-reorg footprint was never re-hydrated (laundering).
        let mirror = StateMirror::new();
        let mut journal = MirrorJournal::new(8);
        assert!(mirror.is_healthy(), "precondition: mirror starts trusted");

        latch_reorg_untrusted(&mirror, &mut journal);
        assert!(!mirror.is_healthy(), "an unproven reorg leaves the mirror untrusted");
        assert!(mirror.has_reason(health_reason::REORG_RECOVERY), "diagnostic marker set");
        assert!(mirror.has_reason(health_reason::FULL_RESYNC_REQUIRED), "sticky trust latch set");
        assert!(journal.full_resync_pending(), "journal sticky latch set (and survived the reset())");

        // A later successful CHECKED reorg clears REORG_RECOVERY (the commit path does exactly this).
        // It must NOT re-trust the mirror: the earlier failure's footprint was never re-hydrated.
        mirror.clear_reason(health_reason::REORG_RECOVERY);
        assert!(
            !mirror.is_healthy(),
            "clearing the diagnostic marker does NOT launder the sticky full-resync latch"
        );
        assert!(mirror.has_reason(health_reason::FULL_RESYNC_REQUIRED), "trust latch persists");
        assert!(journal.full_resync_pending(), "journal latch cleared ONLY by real re-hydration (WP5)");
    }

    #[test]
    fn post_resume_reorg_below_anchor_latches_and_is_unpublishable() {
        // FIX-WP5-A (confirmed fail-open repro): after a warm resume, `activate` sets head=anchor with an
        // EMPTY ring. A deep reorg BELOW the anchor must classify as BeyondJournal (not NoOp), so the
        // checked reorg path LATCHES FULL_RESYNC_REQUIRED. Otherwise (the old NoOp) the reorg would just
        // retag without latching, the resume-hydrated slot would survive, and the next clean close would
        // publish it CERTIFIED though it was never re-proved at the reorg target.
        use crate::mirror_journal::{ActivatedResume, MirrorJournal, RollbackTo};
        let mirror = StateMirror::new();
        let mut journal = MirrorJournal::new(8);
        let pool = [0xD5; 20];
        // Simulate a completed warm resume: hydrate pool/0 = 999, activate at anchor 101 (empty ring).
        journal.activate(
            &mirror,
            ActivatedResume {
                anchor_block: 101,
                patch: vec![(pool, U256::from(0u64), engine_revm::Proven::checked(U256::from(999u64)))],
                prefix_blocks: 1,
            },
        );
        assert_eq!(mirror.get_storage(pool, U256::from(0u64)), Some(U256::from(999u64)));
        // FIX-AUDIT-1(a)/3: activate holds the anchor untrusted (resume footprint in debt) — the reorg
        // path must STILL classify a below-anchor reorg as BeyondJournal and latch (the empty ring is the
        // fail-open this test guards, independent of the debt hold).
        assert!(!mirror.is_healthy(), "activate holds the anchor untrusted until the first re-prove");

        // The fix: an empty-ring reorg below head is BeyondJournal (RED before fix = NoOp).
        assert_eq!(
            journal.plan_rollback_to(96),
            RollbackTo::BeyondJournal,
            "empty-ring reorg below head must be BeyondJournal — NoOp here was the fail-open"
        );

        // The checked reorg path's BeyondJournal handler latches the mirror untrusted (WP4 FIX-A path).
        latch_reorg_untrusted(&mirror, &mut journal);
        assert!(!mirror.is_healthy(), "a below-anchor reorg latches the resumed mirror untrusted");
        assert!(mirror.has_reason(health_reason::FULL_RESYNC_REQUIRED));
        assert!(journal.full_resync_pending());
        // Publication is gated on is_healthy ⇒ the stale hydrated 999 can NEVER publish as certified.
        assert!(
            !reorg_publishable(
                true,
                false,
                journal.debt().is_empty(),
                journal.full_resync_pending(),
                mirror.is_healthy()
            ),
            "the never-re-proved resume slot is not publishable after a below-anchor reorg"
        );
    }

    // ── WI-06 T3 fix round 9 (N1): SHADOW_PROMOTION default-OFF gate ─────────────────────────────

    #[test]
    fn parse_shadow_promotion_defaults_off_and_parses_truthy_tokens() {
        // DEFAULT-OFF: a MISSING value, or any non-truthy token, keeps the experimental promotion
        // DISARMED. This is a PURE parser (no env mutation), so it never races other tests.
        assert!(!parse_shadow_promotion(None), "missing ⇒ OFF (default)");
        assert!(!parse_shadow_promotion(Some("")), "empty ⇒ OFF");
        assert!(!parse_shadow_promotion(Some("0")), "0 ⇒ OFF");
        assert!(!parse_shadow_promotion(Some("false")), "false ⇒ OFF");
        assert!(!parse_shadow_promotion(Some("off")), "off ⇒ OFF");
        assert!(!parse_shadow_promotion(Some("garbage")), "unknown ⇒ OFF (fail-safe)");
        // Explicit opt-in only.
        assert!(parse_shadow_promotion(Some("1")));
        assert!(parse_shadow_promotion(Some("true")));
        assert!(parse_shadow_promotion(Some("TRUE")), "case-insensitive");
        assert!(parse_shadow_promotion(Some(" on ")), "trimmed");
        assert!(parse_shadow_promotion(Some("yes")));
    }

    /// A minimal sealed universe → a real `StatePublisher` whose `command_sender()` backs the submit
    /// gate. The batch enqueues regardless of pool identity (the publisher validates on drain), so a
    /// single V3 pool is enough to exercise `submit_shadow_promotion_if_enabled`.
    fn publisher_with_sender() -> (
        engine_state::publication::StatePublisher,
        engine_state::publication::StateCommandSender,
    ) {
        use engine_state::{RiskClass, UniverseBuilder, VenueId};
        let mut ub = UniverseBuilder::new();
        let t0 = ub.add_token([0x11; 20], 6, RiskClass::Permissionless);
        let t1 = ub.add_token([0x22; 20], 6, RiskClass::Permissionless);
        ub.add_v3_pool(
            [0x33; 20],
            PoolKind::UniswapV3,
            t0,
            t1,
            VenueId(0),
            3_000,
            60,
            V3State {
                sqrt_price_x96: U256::from(1u128) << 96,
                tick: 0,
                liquidity: 1_000_000,
                fee_pips: 3_000,
                tick_spacing: 60,
                initialised_ticks: Vec::new(),
            },
        )
        .expect("v3 pool");
        let (universe, _permit) = ub.seal().into_parts();
        let publisher = engine_state::publication::StatePublisher::new(universe, 4, 64);
        let sender = publisher.command_sender();
        (publisher, sender)
    }

    fn sample_v3_write() -> engine_state::publication::FamilyWrite {
        engine_state::publication::FamilyWrite::V3(V3State {
            sqrt_price_x96: U256::from(1u128) << 96,
            tick: 0,
            liquidity: 1_000_000,
            fee_pips: 3_000,
            tick_spacing: 60,
            initialised_ticks: Vec::new(),
        })
    }

    #[test]
    fn shadow_promotion_off_submits_zero_across_the_corpus() {
        // N1 RED(a): with the EXPERIMENTAL gate OFF (default), ZERO Shadow-origin writes reach the
        // publisher no matter how many promotions the corpus would build — every one is suppressed
        // (routed to the parity+resync diagnostic sink) and counted in `promotion_suppressed`.
        let (_publisher, sender) = publisher_with_sender();
        let stats = ShadowStats::default();
        let corpus = 32u64;
        for _ in 0..corpus {
            let write = sample_v3_write();
            let pre = write.clone();
            let submitted = submit_shadow_promotion_if_enabled(
                /* promotion_enabled */ false,
                &sender,
                &stats,
                PoolId(0),
                7,
                engine_state::publication::UNVALIDATED_PRE_REPLAY_SEQ,
                pre,
                write,
            );
            assert!(!submitted, "an OFF gate must NEVER submit");
        }
        assert_eq!(
            stats.state_emitted.load(Ordering::Relaxed),
            0,
            "ZERO Shadow-origin submits reach the publisher when SHADOW_PROMOTION is OFF (default)"
        );
        assert_eq!(
            stats.promotion_suppressed.load(Ordering::Relaxed),
            corpus,
            "every would-be promotion is counted as suppressed (routed to the diagnostic sink)"
        );
    }

    #[test]
    fn shadow_promotion_on_submits_flow() {
        // N1 RED(b): with the gate explicitly ENABLED, the built promotion IS submitted — the submit
        // counter tracks the corpus and nothing is suppressed.
        let (_publisher, sender) = publisher_with_sender();
        let stats = ShadowStats::default();
        let corpus = 8u64;
        for _ in 0..corpus {
            let write = sample_v3_write();
            let pre = write.clone();
            let submitted = submit_shadow_promotion_if_enabled(
                /* promotion_enabled */ true,
                &sender,
                &stats,
                PoolId(0),
                7,
                engine_state::publication::UNVALIDATED_PRE_REPLAY_SEQ,
                pre,
                write,
            );
            assert!(submitted, "an ENABLED gate submits the built promotion");
        }
        assert_eq!(
            stats.state_emitted.load(Ordering::Relaxed),
            corpus,
            "every enabled promotion reaches the publisher (submit counter tracks the corpus)"
        );
        assert_eq!(
            stats.promotion_suppressed.load(Ordering::Relaxed),
            0,
            "nothing is suppressed when the gate is on"
        );
    }

    #[test]
    fn shadow_promotion_gate_does_not_touch_the_diagnostic_builders() {
        // N1 RED(c): the gate decides ONLY whether the resident-store write is submitted. It does NOT
        // build predictions or pre-images (those are computed unconditionally UPSTREAM of the gate),
        // so turning promotion OFF leaves the shadow lane's diagnostics byte-identical — the OFF path
        // simply suppresses the write and returns false so the caller falls through to the SAME
        // parity+resync diagnostic sink it uses when a prediction is unavailable. The gate carries no
        // predictor input; its OFF behavior is total and side-effect-free beyond the suppressed count.
        let (_publisher, sender) = publisher_with_sender();
        let stats = ShadowStats::default();
        let write = sample_v3_write();
        let pre = write.clone();
        let submitted = submit_shadow_promotion_if_enabled(
            false,
            &sender,
            &stats,
            PoolId(0),
            7,
            engine_state::publication::UNVALIDATED_PRE_REPLAY_SEQ,
            pre,
            write,
        );
        assert!(!submitted);
        // OFF path is confined to the suppressed counter — no parity/emit/state counters moved.
        assert_eq!(stats.state_emitted.load(Ordering::Relaxed), 0);
        assert_eq!(stats.emitted.load(Ordering::Relaxed), 0);
        assert_eq!(stats.parity_pass.load(Ordering::Relaxed), 0);
        assert_eq!(stats.parity_fail.load(Ordering::Relaxed), 0);
        assert_eq!(stats.promotion_suppressed.load(Ordering::Relaxed), 1);
    }
}
