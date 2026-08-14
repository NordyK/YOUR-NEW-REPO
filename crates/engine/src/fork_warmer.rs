//! Fork-warmer thread (P2 of the always-warm plan): every warming RPC round-trip the
//! validator used to pay synchronously moves here.
//!
//! The validator (`engine-revm` thread) publishes its current pin + warm-key set and
//! pushes warm REQUESTS (dirty-pool read-sets, P3); this thread fetches the values over
//! its own RPC client — batched, pinned-block, canonical — and hands back BUNDLES the
//! validator applies as µs-cheap cache inserts. It also autonomously STAGES the next
//! repin: when the feed-observed head has advanced past the validator's pin, it
//! pre-fetches the whole warm set a hair behind the new head (`WARM_FETCH_HEAD_LAG` blocks,
//! default 1, so third-party RPC can actually serve the block) and the validator's repin
//! becomes a zero-RPC swap (P4). Never a state source: values are always RPC reads at an
//! explicit block; a bundle fetched for the wrong pin is dropped by the consumer.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use alloy_primitives::U256;
use crossbeam_queue::ArrayQueue;
use engine_rpc::{ArbitrumRpcClient, BlockTag};
use engine_state::PoolId;

/// Why a warm request/bundle exists (routing + telemetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarmPurpose {
    /// A pool went dirty; refresh the learned read-sets that touch it (P3).
    DirtyPool(PoolId),
    /// Pre-fetched warm set at a NEW head — consume at the next repin (P4).
    StagedRepin,
    /// [Stage 2 · CacheOnly] The exact `(address, slot)` reads a cache-only validation aborted on
    /// (mirror miss). Fetched at the validator's current pin and applied like a [`DirtyPool`] bundle
    /// (at the matching pin), so the re-queued candidate finds them warm on its retry.
    PendingWarm,
}

/// A batch of `(address, slot)` reads the warmer should fetch at `block`.
#[derive(Debug, Clone)]
pub struct WarmRequest {
    /// Block the values must be read at (the validator's current pin).
    pub block: u64,
    /// Slots to fetch.
    pub reads: Vec<([u8; 20], U256)>,
    /// Routing/telemetry tag.
    pub purpose: WarmPurpose,
}

/// Fetched canonical values, ready for µs-cheap insertion into the fork cache.
#[derive(Debug, Clone)]
pub struct WarmBundle {
    /// Block the values were read at — the consumer applies ONLY at this exact pin
    /// (or repins TO this block for [`WarmPurpose::StagedRepin`]).
    pub block: u64,
    /// `(address, slot, value)` triples (batch-fetch errors are simply absent).
    pub values: Vec<([u8; 20], U256, U256)>,
    /// Routing/telemetry tag.
    pub purpose: WarmPurpose,
}

/// Shared lock-free plumbing between the validator thread and the warmer thread.
pub struct ForkWarmerHandles {
    /// Validator/detection → warmer: read-sets to fetch.
    pub req_q: Arc<ArrayQueue<WarmRequest>>,
    /// Warmer → validator: fetched bundles.
    pub out_q: Arc<ArrayQueue<WarmBundle>>,
    /// Validator-published snapshot of the fork's warm storage keys (repin carry set).
    pub warm_keys: Arc<arc_swap::ArcSwap<Vec<([u8; 20], U256)>>>,
    /// Validator's current pin block (0 = no validator / not pinned yet).
    pub published_pin: Arc<AtomicU64>,
}

impl ForkWarmerHandles {
    /// Queues bounded to `cap` entries each.
    pub fn new(cap: usize) -> Self {
        Self {
            req_q: Arc::new(ArrayQueue::new(cap.max(1))),
            out_q: Arc::new(ArrayQueue::new(cap.max(1))),
            warm_keys: Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
            published_pin: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Cheap clone of the shared handles (Arc bumps).
    pub fn share(&self) -> Self {
        Self {
            req_q: self.req_q.clone(),
            out_q: self.out_q.clone(),
            warm_keys: self.warm_keys.clone(),
            published_pin: self.published_pin.clone(),
        }
    }
}

/// Bundles produced (fulfilled requests + stagings).
pub static WARMER_BUNDLES: AtomicU64 = AtomicU64::new(0);
/// Staged-repin bundles produced.
pub static WARMER_STAGED: AtomicU64 = AtomicU64::new(0);
/// [B3] Autonomous staged-repin passes SKIPPED because the validator trusts a healthy mirror
/// (`warm_staging_suppressed()`). Counts would-be stagings suppressed, at the stage cadence — NOT
/// per 5ms tick. 0 in `rpc` mode + whenever the mirror is degraded (staging resumes, fail-safe).
pub static WARM_STAGING_SUPPRESSED: AtomicU64 = AtomicU64::new(0);
/// [B3] Non-PendingWarm bundles (StagedRepin parks + DirtyPool applies) DROPPED at
/// `apply_warm_bundles` because the mirror is trusted+healthy. PendingWarm is never dropped. 0 in
/// `rpc` mode + whenever the mirror is degraded.
pub static WARM_APPLY_SUPPRESSED: AtomicU64 = AtomicU64::new(0);
/// RPC storage-read errors: per-slot JSON-RPC errors + every slot of a wholesale-failed batch.
/// These are the "block not available" PROVIDER races. Split out of the legacy `WARMER_ERRORS` so
/// the heartbeat can distinguish provider races from queue pressure (`WARMER_PUSH_DROPS`).
pub static WARMER_SLOT_ERRORS: AtomicU64 = AtomicU64::new(0);
/// Bundles dropped because the warmer→validator output queue was full — QUEUE PRESSURE, not a
/// provider error. The other half of the legacy `WARMER_ERRORS` split.
pub static WARMER_PUSH_DROPS: AtomicU64 = AtomicU64::new(0);

/// Max requests fulfilled per tick (keeps a burst from starving the staging check).
const MAX_REQUESTS_PER_TICK: usize = 8;

/// One scheduling pass of the warmer: fulfil queued requests, then (when `stage_due`
/// and a block newer than the pin is servable) stage the warm set `head_lag` blocks behind
/// the feed head. Returns true when any work was done (caller sleeps briefly otherwise).
///
/// `head_lag` (env `WARM_FETCH_HEAD_LAG`, default 1) is how many blocks BEHIND the feed head
/// the staged repin is fetched: the just-arrived head is often not yet servable by third-party
/// RPC. `retry_ms` (env `WARM_RETRY_MS`, default 150) is the one-shot backoff before re-fetching
/// a wholesale-failed DirtyPool/PendingWarm request at the SAME block.
/// [WP6 · Finding 7 row 4] `WARM_FETCH_HEAD_LAG` contract: unset ⇒ **1** (stage one block behind the
/// feed head — the intentional A2 default, NOT the pre-A2 stage-at-head); an explicit value is used
/// verbatim, and `0` restores the legacy stage-at-head behavior. Unparseable ⇒ the default `1`. Pure so
/// the default contract is tested without touching process env.
pub(crate) fn warm_head_lag(raw: Option<String>) -> u64 {
    raw.and_then(|s| s.trim().parse().ok()).unwrap_or(1)
}

/// [WP6 · Finding 7 row 5] `WARM_RETRY_MS` contract: unset ⇒ **150** (a one-shot backoff before the
/// wholesale-failed re-fetch); `0` removes the DELAY but does NOT disable the retry — `warmer_tick`
/// still re-fetches once on a wholesale failure (the refetch is unconditional on `wholesale_failed`),
/// so `0` does not fully recreate pre-A2 behavior. Unparseable ⇒ `150`. Pure for testing.
pub(crate) fn warm_retry_ms(raw: Option<String>) -> u64 {
    raw.and_then(|s| s.trim().parse().ok()).unwrap_or(150)
}

pub(crate) fn warmer_tick(
    client: &ArbitrumRpcClient,
    handles: &ForkWarmerHandles,
    feed_head: u64,
    stage_due: bool,
    stage_max_slots: usize,
    head_lag: u64,
    retry_ms: u64,
) -> bool {
    let mut did_work = false;

    // 1) Fulfil queued requests (bounded per tick). DirtyPool/PendingWarm are fetched at EXACTLY
    //    `req.block` (the validator's pin): the value is canonical only there, so a read at any
    //    other block must never reach the fork cache. On a WHOLESALE batch failure (the whole
    //    `storage_batch` call errored — e.g. the pin==head boot race) retry the same block once
    //    after `retry_ms`, then drop. Per-slot errors keep current behavior (the slot stays lazy).
    for _ in 0..MAX_REQUESTS_PER_TICK {
        let Some(req) = handles.req_q.pop() else { break };
        if req.reads.is_empty() {
            continue;
        }
        let mut outcome = fetch_values(client, req.block, &req.reads);
        did_work = true;
        if outcome.wholesale_failed {
            std::thread::sleep(std::time::Duration::from_millis(retry_ms));
            let retry = fetch_values(client, req.block, &req.reads);
            outcome.slot_errors += retry.slot_errors;
            outcome.values = retry.values; // re-fetch is at the same block → canonical
        }
        if outcome.slot_errors > 0 {
            WARMER_SLOT_ERRORS.fetch_add(outcome.slot_errors, Ordering::Relaxed);
        }
        if outcome.values.is_empty() {
            continue; // nothing fetched (all slots errored / the retry still failed)
        }
        let bundle = WarmBundle {
            block: req.block,
            values: outcome.values,
            purpose: req.purpose,
        };
        if handles.out_q.push(bundle).is_err() {
            WARMER_PUSH_DROPS.fetch_add(1, Ordering::Relaxed);
        } else {
            WARMER_BUNDLES.fetch_add(1, Ordering::Relaxed);
        }
    }

    // 2) Stage the next repin `head_lag` blocks behind the feed head so third-party RPC can serve
    //    the block, making the repin a zero-RPC swap. The validator repins TO the bundle's block,
    //    so values stay canonical; the cost is ~one block (~250ms) of freshness vs. the repin
    //    cadence. Only when that staged block is actually newer than the pin, a pin exists, and due.
    //    No retry here: the head-lag is the race fix, and a stale staged set is harmless (dropped).
    if stage_due {
        let pin = handles.published_pin.load(Ordering::Relaxed);
        let stage_block = feed_head.saturating_sub(head_lag);
        if pin > 0 && stage_block > pin {
            let keys = handles.warm_keys.load();
            if !keys.is_empty() {
                let take = keys.len().min(stage_max_slots.max(1));
                let outcome = fetch_values(client, stage_block, &keys[..take]);
                did_work = true;
                if outcome.slot_errors > 0 {
                    WARMER_SLOT_ERRORS.fetch_add(outcome.slot_errors, Ordering::Relaxed);
                }
                if !outcome.values.is_empty() {
                    let bundle = WarmBundle {
                        block: stage_block,
                        values: outcome.values,
                        purpose: WarmPurpose::StagedRepin,
                    };
                    if handles.out_q.push(bundle).is_err() {
                        WARMER_PUSH_DROPS.fetch_add(1, Ordering::Relaxed);
                    } else {
                        WARMER_BUNDLES.fetch_add(1, Ordering::Relaxed);
                        WARMER_STAGED.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    did_work
}

/// Result of a [`fetch_values`] pass: canonical triples plus the error accounting the caller folds
/// into the split counters. PURE (no global side effects) so the metric split is unit-testable and
/// the caller can decide whether to retry a wholesale failure.
struct FetchOutcome {
    /// `(address, slot, value)` triples successfully read (errored slots are simply absent).
    values: Vec<([u8; 20], U256, U256)>,
    /// Slots that errored: per-slot JSON-RPC errors + every slot of a wholesale-failed chunk.
    /// The caller folds this into [`WARMER_SLOT_ERRORS`].
    slot_errors: u64,
    /// True when at least one chunk's `storage_batch` RPC call failed OUTRIGHT ("block not
    /// available" for the whole request). The caller retries DirtyPool/PendingWarm requests once.
    wholesale_failed: bool,
}

/// Fetch `reads` at `block` (≤400 per RPC round-trip) into value triples. A per-slot error drops
/// that slot (it stays lazy-fetchable); a wholesale batch error drops the whole chunk and flags
/// [`FetchOutcome::wholesale_failed`]. Both are counted in [`FetchOutcome::slot_errors`]; the caller
/// owns the [`WARMER_SLOT_ERRORS`] counter and the wholesale retry.
fn fetch_values(
    client: &ArbitrumRpcClient,
    block: u64,
    reads: &[([u8; 20], U256)],
) -> FetchOutcome {
    const BATCH: usize = 400;
    let mut values = Vec::with_capacity(reads.len());
    let mut slot_errors = 0u64;
    let mut wholesale_failed = false;
    for chunk in reads.chunks(BATCH) {
        let req: Vec<([u8; 20], [u8; 32])> =
            chunk.iter().map(|(a, s)| (*a, s.to_be_bytes())).collect();
        match client.storage_batch(&req, BlockTag::Number(block)) {
            Ok(items) => {
                for ((addr, slot), item) in chunk.iter().zip(items) {
                    match item {
                        Ok(word) => values.push((*addr, *slot, U256::from_be_bytes(word))),
                        Err(_) => slot_errors += 1,
                    }
                }
            }
            Err(_) => {
                // Wholesale batch failure: the block wasn't servable for the whole request.
                slot_errors += chunk.len() as u64;
                wholesale_failed = true;
            }
        }
    }
    FetchOutcome {
        values,
        slot_errors,
        wholesale_failed,
    }
}

/// Process-global dirty→warm hint queue (P3): every dirty-pool funnel tees pool ids in
/// here (best-effort, lock-free); the validator drains them, resolves pool→hot-routes→
/// learned read-sets, and asks the warmer to refresh those slots BEFORE candidates on
/// the moved pool arrive. Bounded; a full queue silently drops hints (they are hints).
static WARM_HINTS: std::sync::OnceLock<ArrayQueue<PoolId>> = std::sync::OnceLock::new();

/// The shared hint queue (lazily initialized, capacity 1024).
pub fn warm_hints() -> &'static ArrayQueue<PoolId> {
    WARM_HINTS.get_or_init(|| ArrayQueue::new(1024))
}

/// Per-pool rate limit for dirty→warm hints so a hot pool emitting events every block
/// doesn't re-fetch the same read-set hundreds of times a minute.
pub struct WarmHintThrottle {
    last: ahash::AHashMap<u32, std::time::Instant>,
    min_interval: std::time::Duration,
}

impl WarmHintThrottle {
    /// New throttle admitting each pool at most once per `min_interval`.
    pub fn new(min_interval: std::time::Duration) -> Self {
        Self {
            last: ahash::AHashMap::new(),
            min_interval,
        }
    }

    /// True when `pool` may be warmed now (records the admission).
    pub fn admit(&mut self, pool: PoolId, now: std::time::Instant) -> bool {
        match self.last.get(&pool.0) {
            Some(prev) if now.duration_since(*prev) < self.min_interval => false,
            _ => {
                self.last.insert(pool.0, now);
                true
            }
        }
    }
}

/// Resolve admitted dirty pools into warm requests: for each pool take its top
/// `max_routes_per_pool` hot routes (caller supplies the score-ordered lookup), union
/// their LEARNED read-sets, and emit one request per pool at the current `pin`. Pools
/// whose routes have no learned read-set yet produce nothing (first validation will
/// learn them).
pub fn hints_to_requests(
    pools: &[PoolId],
    routes_of_pool: impl Fn(PoolId) -> Vec<engine_state::RouteId>,
    readsets: &crate::route_readsets::RouteReadSets,
    pin: u64,
    max_routes_per_pool: usize,
) -> Vec<WarmRequest> {
    let mut out = Vec::new();
    for &pool in pools {
        let mut reads: Vec<([u8; 20], U256)> = Vec::new();
        for route in routes_of_pool(pool).into_iter().take(max_routes_per_pool.max(1)) {
            if let Some(rs) = readsets.get(route) {
                reads.extend_from_slice(&rs.slots);
            }
        }
        if reads.is_empty() {
            continue;
        }
        reads.sort_unstable();
        reads.dedup();
        out.push(WarmRequest {
            block: pin,
            reads,
            purpose: WarmPurpose::DirtyPool(pool),
        });
    }
    out
}

/// Drain the warmer's output queue on the validator thread: bundles fetched at the
/// CURRENT pin are applied as µs-cheap cache inserts; a [`WarmPurpose::StagedRepin`]
/// bundle is parked in `staged` (newest block wins) for the next repin to consume;
/// anything else at a non-current block is dropped (stale — fetched for a pin that
/// no longer exists). Returns `(slots_applied, bundles_dropped_stale, bundles_suppressed)`.
///
/// [B3] `suppress_staged` (the caller's `warm_staging_suppressed()`): under a trusted+healthy mirror
/// the StagedRepin and DirtyPool bundles are REDUNDANT (the mirror serves head state) and their
/// canonical-at-pin values would SHADOW the mirror-head for non-route slots — so they are DROPPED
/// (counted in the third return value), NOT parked/applied. [`WarmPurpose::PendingWarm`] is ALWAYS
/// applied: it is the cache-only retry's on-demand miss-warm (B2), which the validator needs even
/// under a trusted mirror. `suppress_staged == false` (rpc mode / degraded mirror) is byte-identical
/// to the pre-B3 behaviour.
pub fn apply_warm_bundles<F: engine_revm::StateFetcher>(
    exec: &mut engine_revm::ForkExecutor<F>,
    current_pin: Option<u64>,
    out_q: &ArrayQueue<WarmBundle>,
    staged: &mut Option<WarmBundle>,
    suppress_staged: bool,
) -> (usize, usize, usize) {
    let mut applied = 0usize;
    let mut dropped = 0usize;
    let mut suppressed = 0usize;
    while let Some(bundle) = out_q.pop() {
        match bundle.purpose {
            // On-demand cache-only miss-warm (B2): ALWAYS applied at the current pin (else stale) —
            // the validator needs it even under a trusted mirror, so it is NEVER suppressed.
            WarmPurpose::PendingWarm => {
                if current_pin == Some(bundle.block) {
                    for (a, s, v) in &bundle.values {
                        exec.preload_storage(*a, *s, *v);
                        applied += 1;
                    }
                } else {
                    dropped += 1;
                }
            }
            // Autonomous repin warm-set: parked for the next repin (newest block wins), unless
            // suppressed (trusted+healthy mirror ⇒ redundant + would shadow mirror-head).
            WarmPurpose::StagedRepin => {
                if suppress_staged {
                    suppressed += 1;
                } else {
                    let newer = staged.as_ref().is_none_or(|s| bundle.block > s.block);
                    if newer {
                        *staged = Some(bundle);
                    }
                }
            }
            // Proactive dirty-pool warm: applied at the current pin, unless suppressed — under a
            // trusted mirror the advancer already reflects the move (and the Stage-3 wipe re-reads
            // the pool), so a DirtyPool apply is redundant + would shadow the mirror-head.
            WarmPurpose::DirtyPool(_) => {
                if suppress_staged {
                    suppressed += 1;
                } else if current_pin == Some(bundle.block) {
                    for (a, s, v) in &bundle.values {
                        exec.preload_storage(*a, *s, *v);
                        applied += 1;
                    }
                } else {
                    dropped += 1;
                }
            }
        }
    }
    (applied, dropped, suppressed)
}

/// Spawn the `engine-fork-warmer` thread. `stage_interval_ms` throttles autonomous
/// staged-repin fetches; `stage_max_slots` caps the staged warm set (mirrors
/// `REVM_REPIN_REFRESH_MAX_SLOTS`).
pub fn spawn_fork_warmer(
    rpc_url: String,
    handles: ForkWarmerHandles,
    batch_clock: Arc<engine_gas::BatchClock>,
    shutdown: Arc<AtomicBool>,
    stage_interval_ms: u64,
    stage_max_slots: usize,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("engine-fork-warmer".into())
        .spawn(move || {
            // [A4] Optional dedicated core for the fork-warmer. DEFAULT (FORK_WARMER_CORE unset)
            // = NO pin — byte-identical to today's unpinned warmer (it runs on core 0 alongside
            // the OS + other background threads). The AWS runbook sets FORK_WARMER_CORE=4 to move
            // it onto the one otherwise-free core: under SIM_EVAL_TOPK<=16 the sim helper on core 4
            // never fires, and the warmer is I/O-blocked on RPC ~all the time, so it costs ~zero
            // CPU there. FORK_WARMER_CORE=-1 (or any negative) explicitly disables pinning (laptops
            // / non-isolcpus hosts). Best-effort via `pin_to_core` (logs + continues on failure).
            match std::env::var("FORK_WARMER_CORE") {
                Ok(v) => {
                    if let Ok(core) = v.trim().parse::<i64>() {
                        if core >= 0 {
                            crate::runtime::pin_to_core(core as usize, "engine-fork-warmer");
                        }
                    }
                }
                // unset → leave unpinned (byte-identical to today)
                Err(_) => {}
            }
            // Warming feeds the validator's latency-critical path → hot endpoint,
            // same resolution as the validator's own client.
            let client =
                ArbitrumRpcClient::new(engine_rpc::hot_rpc_url().unwrap_or(rpc_url));
            // Stage the repin warm-set this many blocks BEHIND the feed head (the just-arrived
            // head is often not yet servable by third-party RPC → per-slot "block not available"
            // storms). Default 1; 0 reproduces the pre-A2 stage-at-head behavior.
            let head_lag: u64 = warm_head_lag(std::env::var("WARM_FETCH_HEAD_LAG").ok());
            // One-shot backoff before re-fetching a wholesale-failed DirtyPool/PendingWarm request
            // at the SAME (pin) block — recovers the pin==head boot race without a wrong-block read.
            let retry_ms: u64 = warm_retry_ms(std::env::var("WARM_RETRY_MS").ok());
            let mut last_stage = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_millis(stage_interval_ms))
                .unwrap_or_else(std::time::Instant::now);
            while !shutdown.load(Ordering::Relaxed) {
                let stage_due_base =
                    last_stage.elapsed().as_millis() as u64 >= stage_interval_ms;
                // [B3] Suppress the autonomous staged-repin warm-set when the validator trusts a
                // HEALTHY mirror: it is redundant RPC (the mirror serves head state) and its
                // canonical-at-pin values would SHADOW the mirror-head for non-route slots. The
                // on-demand PendingWarm requests in `warmer_tick` part 1 still flow. Reset the
                // cadence clock when suppressing so the counter ticks once per would-be staging
                // (the stage cadence), not on every 5ms loop pass. `false` in rpc mode + the moment
                // the mirror degrades ⇒ staging resumes ⇒ byte-identical to today when untrusted.
                let stage_due = if stage_due_base && crate::pipeline::warm_staging_suppressed() {
                    WARM_STAGING_SUPPRESSED.fetch_add(1, Ordering::Relaxed);
                    last_stage = std::time::Instant::now();
                    false
                } else {
                    stage_due_base
                };
                let feed_head = batch_clock.last_block_number();
                let did = warmer_tick(
                    &client,
                    &handles,
                    feed_head,
                    stage_due,
                    stage_max_slots,
                    head_lag,
                    retry_ms,
                );
                if did && stage_due {
                    last_stage = std::time::Instant::now();
                }
                if !did {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revm_fork::spawn_mock_rpc;

    fn mk_client(storage: Vec<(([u8; 20], U256), U256)>) -> ArbitrumRpcClient {
        let (port, _conns) = spawn_mock_rpc([0u8; 20], Vec::new(), storage);
        ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"))
    }

    #[test]
    fn request_becomes_bundle_with_canonical_values() {
        let pool = [0xA1u8; 20];
        let client = mk_client(vec![
            ((pool, U256::from(5u64)), U256::from(0x55u64)),
            ((pool, U256::from(7u64)), U256::from(0x77u64)),
        ]);
        let h = ForkWarmerHandles::new(16);
        h.req_q
            .push(WarmRequest {
                block: 9,
                reads: vec![(pool, U256::from(5u64)), (pool, U256::from(7u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(3)),
            })
            .unwrap();
        let did = warmer_tick(&client, &h, 0, false, 4096, 1, 0);
        assert!(did, "fulfilling a request counts as work");
        let b = h.out_q.pop().expect("bundle must be produced");
        assert_eq!(b.block, 9, "DirtyPool bundle is fetched at exactly the request block");
        assert_eq!(b.purpose, WarmPurpose::DirtyPool(PoolId(3)));
        assert!(b.values.contains(&(pool, U256::from(5u64), U256::from(0x55u64))));
        assert!(b.values.contains(&(pool, U256::from(7u64), U256::from(0x77u64))));
    }

    #[test]
    fn warm_env_defaults_are_intentional_not_pre_a2_identical() {
        // [WP6 · Finding 7 rows 4-5] The unset A2 defaults intentionally DIFFER from the pre-A2
        // baseline — so "every flag unset is byte-identical" is FALSE. These are the tested contracts.
        assert_eq!(warm_head_lag(None), 1, "WARM_FETCH_HEAD_LAG unset ⇒ 1 (stage one block behind head)");
        assert_eq!(warm_head_lag(Some("0".into())), 0, "0 restores the legacy stage-at-head behavior");
        assert_eq!(warm_head_lag(Some("3".into())), 3, "explicit value used verbatim");
        assert_eq!(warm_head_lag(Some("nonsense".into())), 1, "unparseable ⇒ the default 1");
        assert_eq!(warm_retry_ms(None), 150, "WARM_RETRY_MS unset ⇒ 150ms one-shot backoff");
        assert_eq!(
            warm_retry_ms(Some("0".into())),
            0,
            "0 removes the DELAY (but warmer_tick still re-fetches once — the retry is not disabled)"
        );
        assert_eq!(warm_retry_ms(Some("bad".into())), 150, "unparseable ⇒ the default 150");
    }

    #[test]
    fn stages_warm_set_at_head_minus_lag() {
        let pool = [0xA2u8; 20];
        let client = mk_client(vec![
            ((pool, U256::from(1u64)), U256::from(0x11u64)),
            ((pool, U256::from(2u64)), U256::from(0x22u64)),
        ]);
        let h = ForkWarmerHandles::new(16);
        h.published_pin.store(10, Ordering::Relaxed);
        h.warm_keys
            .store(Arc::new(vec![(pool, U256::from(1u64)), (pool, U256::from(2u64))]));
        // Default head-lag (1): stage ONE block behind the feed head (12) so RPC can serve it.
        assert!(warmer_tick(&client, &h, 12, true, 4096, 1, 0), "staging counts as work");
        let b = h.out_q.pop().expect("staged bundle must be produced");
        assert_eq!(b.purpose, WarmPurpose::StagedRepin);
        assert_eq!(b.block, 11, "staged at feed_head - WARM_FETCH_HEAD_LAG");
        assert_eq!(b.values.len(), 2);
        // head-lag 0 reproduces the pre-A2 behavior byte-for-byte (stage AT the feed head).
        assert!(warmer_tick(&client, &h, 12, true, 4096, 0, 0));
        let b0 = h.out_q.pop().expect("staged bundle at lag 0");
        assert_eq!(b0.block, 12, "lag 0 stages at the feed head (legacy behavior)");
    }

    #[test]
    fn does_not_stage_when_head_not_past_pin_or_not_due() {
        let pool = [0xA3u8; 20];
        let client = mk_client(vec![((pool, U256::from(1u64)), U256::from(0x11u64))]);
        let h = ForkWarmerHandles::new(16);
        h.published_pin.store(10, Ordering::Relaxed);
        h.warm_keys.store(Arc::new(vec![(pool, U256::from(1u64))]));
        assert!(!warmer_tick(&client, &h, 10, true, 4096, 1, 0), "head==pin: no staging");
        assert!(h.out_q.pop().is_none());
        // head == pin+1 with lag 1 → the staged block would BE the pin → nothing newer to stage.
        assert!(!warmer_tick(&client, &h, 11, true, 4096, 1, 0), "head-lag lands on pin: no staging");
        assert!(h.out_q.pop().is_none());
        assert!(!warmer_tick(&client, &h, 12, false, 4096, 1, 0), "not due: no staging");
        assert!(h.out_q.pop().is_none());
        // No pin published yet (0) → never stage.
        h.published_pin.store(0, Ordering::Relaxed);
        assert!(!warmer_tick(&client, &h, 12, true, 4096, 1, 0), "pin 0: no staging");
        assert!(h.out_q.pop().is_none());
    }

    #[test]
    fn throttle_admits_then_blocks_then_readmits() {
        use std::time::{Duration, Instant};
        let mut th = WarmHintThrottle::new(Duration::from_millis(250));
        let t0 = Instant::now();
        assert!(th.admit(PoolId(1), t0), "first hint admits");
        assert!(!th.admit(PoolId(1), t0 + Duration::from_millis(100)), "within interval blocks");
        assert!(th.admit(PoolId(2), t0 + Duration::from_millis(100)), "other pool unaffected");
        assert!(th.admit(PoolId(1), t0 + Duration::from_millis(300)), "after interval readmits");
    }

    #[test]
    fn hints_resolve_to_learned_readset_requests() {
        use engine_state::RouteId;
        let mut readsets = crate::route_readsets::RouteReadSets::new(8, 16);
        let pool_a = [0xD1u8; 20];
        readsets.record(RouteId(1), 100, &[(pool_a, U256::from(5u64))], &[]);
        readsets.record(RouteId(2), 100, &[(pool_a, U256::from(5u64)), (pool_a, U256::from(7u64))], &[]);
        // RouteId(3) has NO learned set.
        let routes_of = |p: PoolId| -> Vec<RouteId> {
            match p.0 {
                10 => vec![RouteId(1), RouteId(2), RouteId(3)],
                11 => vec![RouteId(3)],
                _ => vec![],
            }
        };
        let reqs = hints_to_requests(&[PoolId(10), PoolId(11), PoolId(12)], routes_of, &readsets, 42, 4);
        assert_eq!(reqs.len(), 1, "only the pool with learned routes produces a request");
        let r = &reqs[0];
        assert_eq!(r.block, 42);
        assert_eq!(r.purpose, WarmPurpose::DirtyPool(PoolId(10)));
        assert_eq!(r.reads.len(), 2, "union of the routes' read-sets, deduped: {:?}", r.reads);
        assert!(r.reads.contains(&(pool_a, U256::from(5u64))));
        assert!(r.reads.contains(&(pool_a, U256::from(7u64))));
    }

    #[test]
    fn hints_respect_route_cap() {
        use engine_state::RouteId;
        let mut readsets = crate::route_readsets::RouteReadSets::new(8, 16);
        let a = [0xD2u8; 20];
        readsets.record(RouteId(1), 100, &[(a, U256::from(1u64))], &[]);
        readsets.record(RouteId(2), 100, &[(a, U256::from(2u64))], &[]);
        let routes_of = |_p: PoolId| vec![RouteId(1), RouteId(2)];
        let reqs = hints_to_requests(&[PoolId(10)], routes_of, &readsets, 42, 1);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].reads.len(), 1, "route cap 1 → only the first route's set");
    }

    struct ZeroFetcher;
    impl engine_revm::StateFetcher for ZeroFetcher {
        fn fetch_account(&self, _a: [u8; 20]) -> (U256, u64, Option<Vec<u8>>) {
            (U256::ZERO, 0, None)
        }
        fn fetch_storage(&self, _a: [u8; 20], _s: U256) -> U256 {
            U256::ZERO
        }
    }

    #[test]
    fn bundles_apply_only_at_matching_pin_and_staged_is_parked() {
        let pool = [0xB1u8; 20];
        let mut exec = engine_revm::ForkExecutor::new(ZeroFetcher);
        let out_q: ArrayQueue<WarmBundle> = ArrayQueue::new(8);
        // Matching pin → applied.
        out_q
            .push(WarmBundle {
                block: 10,
                values: vec![(pool, U256::from(5u64), U256::from(0x55u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(1)),
            })
            .unwrap();
        // Wrong pin → dropped stale.
        out_q
            .push(WarmBundle {
                block: 9,
                values: vec![(pool, U256::from(6u64), U256::from(0x66u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(1)),
            })
            .unwrap();
        // Staged repins park; the NEWEST block wins.
        out_q
            .push(WarmBundle {
                block: 12,
                values: vec![(pool, U256::from(1u64), U256::from(0x11u64))],
                purpose: WarmPurpose::StagedRepin,
            })
            .unwrap();
        out_q
            .push(WarmBundle {
                block: 14,
                values: vec![(pool, U256::from(2u64), U256::from(0x22u64))],
                purpose: WarmPurpose::StagedRepin,
            })
            .unwrap();
        let mut staged = None;
        let (applied, dropped, suppressed) =
            apply_warm_bundles(&mut exec, Some(10), &out_q, &mut staged, false);
        assert_eq!(applied, 1, "only the matching-pin bundle applies");
        assert_eq!(dropped, 1, "the wrong-pin bundle is dropped");
        assert_eq!(suppressed, 0, "nothing suppressed when suppress_staged=false");
        assert!(
            exec.is_storage_cached(pool, U256::from(5u64)),
            "applied slot must be in the warm cache"
        );
        assert!(
            !exec.is_storage_cached(pool, U256::from(6u64)),
            "stale bundle must NOT touch the cache"
        );
        let st = staged.expect("staged bundle parked");
        assert_eq!(st.block, 14, "newest staged block wins");
        assert!(
            !exec.is_storage_cached(pool, U256::from(2u64)),
            "staged values must NOT apply before the repin"
        );
    }

    #[test]
    fn no_pin_drops_everything_but_still_parks_staged() {
        let pool = [0xB2u8; 20];
        let mut exec = engine_revm::ForkExecutor::new(ZeroFetcher);
        let out_q: ArrayQueue<WarmBundle> = ArrayQueue::new(4);
        out_q
            .push(WarmBundle {
                block: 10,
                values: vec![(pool, U256::from(5u64), U256::from(0x55u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(1)),
            })
            .unwrap();
        out_q
            .push(WarmBundle {
                block: 11,
                values: vec![(pool, U256::from(1u64), U256::from(0x11u64))],
                purpose: WarmPurpose::StagedRepin,
            })
            .unwrap();
        let mut staged = None;
        let (applied, dropped, suppressed) =
            apply_warm_bundles(&mut exec, None, &out_q, &mut staged, false);
        assert_eq!((applied, dropped, suppressed), (0, 1, 0));
        assert_eq!(staged.map(|b| b.block), Some(11));
    }

    #[test]
    fn suppress_staged_drops_staged_and_dirtypool_but_keeps_pendingwarm() {
        // [B3] Under a trusted+healthy mirror (`suppress_staged = true`): StagedRepin bundles are
        // NOT parked and DirtyPool bundles are NOT applied (both redundant + would shadow the
        // mirror-head); only the on-demand PendingWarm miss-warm still lands in the CacheDB.
        let pool = [0xB3u8; 20];
        let mut exec = engine_revm::ForkExecutor::new(ZeroFetcher);
        let out_q: ArrayQueue<WarmBundle> = ArrayQueue::new(8);
        // A StagedRepin bundle: must be DROPPED (not parked) under suppression.
        out_q
            .push(WarmBundle {
                block: 12,
                values: vec![(pool, U256::from(1u64), U256::from(0x11u64))],
                purpose: WarmPurpose::StagedRepin,
            })
            .unwrap();
        // A DirtyPool bundle at the current pin: must be DROPPED (not applied) under suppression.
        out_q
            .push(WarmBundle {
                block: 10,
                values: vec![(pool, U256::from(5u64), U256::from(0x55u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(1)),
            })
            .unwrap();
        // A PendingWarm bundle at the current pin: must STILL be applied (the validator needs it).
        out_q
            .push(WarmBundle {
                block: 10,
                values: vec![(pool, U256::from(7u64), U256::from(0x77u64))],
                purpose: WarmPurpose::PendingWarm,
            })
            .unwrap();
        let mut staged = None;
        let (applied, dropped, suppressed) =
            apply_warm_bundles(&mut exec, Some(10), &out_q, &mut staged, true);
        assert_eq!(applied, 1, "only the PendingWarm slot applies under suppression");
        assert_eq!(dropped, 0, "no stale drops here (StagedRepin+DirtyPool are suppressed, not stale)");
        assert_eq!(suppressed, 2, "StagedRepin + DirtyPool are both suppressed");
        assert!(staged.is_none(), "StagedRepin must NOT be parked under suppression");
        assert!(
            exec.is_storage_cached(pool, U256::from(7u64)),
            "PendingWarm slot must land in the cache even under suppression"
        );
        assert!(
            !exec.is_storage_cached(pool, U256::from(5u64)),
            "DirtyPool slot must NOT be applied under suppression"
        );
    }

    #[test]
    fn staged_slot_cap_is_respected() {
        let pool = [0xA4u8; 20];
        let client = mk_client(vec![
            ((pool, U256::from(1u64)), U256::from(0x11u64)),
            ((pool, U256::from(2u64)), U256::from(0x22u64)),
            ((pool, U256::from(3u64)), U256::from(0x33u64)),
        ]);
        let h = ForkWarmerHandles::new(16);
        h.published_pin.store(10, Ordering::Relaxed);
        h.warm_keys.store(Arc::new(vec![
            (pool, U256::from(1u64)),
            (pool, U256::from(2u64)),
            (pool, U256::from(3u64)),
        ]));
        assert!(warmer_tick(&client, &h, 12, true, 2, 1, 0));
        let b = h.out_q.pop().expect("staged bundle");
        assert_eq!(b.values.len(), 2, "staged set must respect the cap");
    }

    // ---- A2: fork-warmer head-race fix (block-exact fetch + wholesale retry + metric split) ----

    /// A block-aware, optionally first-fails storage mock (the plain `mk_client` mock ignores the
    /// block tag, so it can't exercise the head-race). Behavior of `eth_getStorageAt`:
    /// - the first `fail_first` storage round-trips reply with a single TOP-LEVEL error object (not
    ///   a JSON array) → the client's `Vec<RpcResponse>` decode fails → an outer `Err` from
    ///   `storage_batch` = a WHOLESALE failure;
    /// - otherwise each slot is served its mapped value ONLY when the request's block tag equals
    ///   `ok_block` (or `ok_block` is None); at any other block the slot gets a PER-ITEM JSON-RPC
    ///   error ("block not available").
    /// Returns the port and a shared count of storage round-trips (to assert "retry exactly once").
    fn spawn_block_aware_mock(
        storage: Vec<(([u8; 20], U256), U256)>,
        ok_block: Option<u64>,
        fail_first: u32,
    ) -> (u16, Arc<std::sync::atomic::AtomicU32>) {
        use alloy_primitives::hex;
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let hits_srv = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let body_start;
                while let Ok(n) = sock.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        body_start = p + 4;
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
                        let arr = body
                            .as_array()
                            .cloned()
                            .unwrap_or_else(|| vec![body.clone()]);
                        let nth = hits_srv.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let reply = if nth < fail_first {
                            // Wholesale failure: one top-level error object (NOT an array).
                            serde_json::json!({
                                "jsonrpc":"2.0","id":0,
                                "error":{"code":-32000,"message":"block not available"}
                            })
                            .to_string()
                        } else {
                            let items: Vec<serde_json::Value> = arr
                                .iter()
                                .map(|item| {
                                    let id = item["id"].clone();
                                    let a = item["params"][0].as_str().unwrap_or("");
                                    let s = item["params"][1].as_str().unwrap_or("");
                                    let blk = item["params"][2].as_str().unwrap_or("");
                                    let block_ok = match ok_block {
                                        None => true,
                                        Some(b) => blk.eq_ignore_ascii_case(&format!("0x{:x}", b)),
                                    };
                                    if !block_ok {
                                        return serde_json::json!({
                                            "jsonrpc":"2.0","id":id,
                                            "error":{"code":-32000,"message":"block not available"}
                                        });
                                    }
                                    let hit = storage.iter().find(|((ad, sl), _)| {
                                        format!("0x{}", hex::encode(ad)).eq_ignore_ascii_case(a)
                                            && format!(
                                                "0x{}",
                                                hex::encode(sl.to_be_bytes::<32>())
                                            )
                                            .eq_ignore_ascii_case(s)
                                    });
                                    let result = match hit {
                                        Some((_, v)) => {
                                            format!("0x{}", hex::encode(v.to_be_bytes::<32>()))
                                        }
                                        None => format!("0x{}", "00".repeat(32)),
                                    };
                                    serde_json::json!({"jsonrpc":"2.0","id":id,"result":result})
                                })
                                .collect();
                            serde_json::Value::Array(items).to_string()
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
            }
        });
        (port, hits)
    }

    fn mk_client_from_port(port: u16) -> ArbitrumRpcClient {
        ArbitrumRpcClient::new(format!("http://127.0.0.1:{port}"))
    }

    #[test]
    fn fetch_values_serves_at_block_with_no_errors() {
        let pool = [0xC1u8; 20];
        let (port, _h) =
            spawn_block_aware_mock(vec![((pool, U256::from(5u64)), U256::from(0x55u64))], Some(7), 0);
        let client = mk_client_from_port(port);
        let out = fetch_values(&client, 7, &[(pool, U256::from(5u64))]);
        assert_eq!(out.slot_errors, 0);
        assert!(!out.wholesale_failed);
        assert_eq!(out.values, vec![(pool, U256::from(5u64), U256::from(0x55u64))]);
    }

    #[test]
    fn fetch_values_wrong_block_is_per_slot_errors_not_wholesale() {
        let pool = [0xC2u8; 20];
        let (port, _h) = spawn_block_aware_mock(
            vec![
                ((pool, U256::from(5u64)), U256::from(0x55u64)),
                ((pool, U256::from(6u64)), U256::from(0x66u64)),
            ],
            Some(7),
            0,
        );
        let client = mk_client_from_port(port);
        // Ask at block 8 (mock only serves 7): each slot errors individually but the batch CALL
        // succeeds → per-slot, NOT wholesale → DirtyPool keeps the slots lazy (no retry).
        let out = fetch_values(&client, 8, &[(pool, U256::from(5u64)), (pool, U256::from(6u64))]);
        assert!(out.values.is_empty());
        assert_eq!(out.slot_errors, 2, "both slots errored per-item");
        assert!(!out.wholesale_failed, "per-slot errors are not a wholesale batch failure");
    }

    #[test]
    fn fetch_values_wholesale_failure_sets_flag_and_counts_all_slots() {
        let pool = [0xC3u8; 20];
        let (port, _h) = spawn_block_aware_mock(
            vec![
                ((pool, U256::from(5u64)), U256::from(0x55u64)),
                ((pool, U256::from(6u64)), U256::from(0x66u64)),
            ],
            None,
            999, // always wholesale-fail
        );
        let client = mk_client_from_port(port);
        let out = fetch_values(&client, 7, &[(pool, U256::from(5u64)), (pool, U256::from(6u64))]);
        assert!(out.values.is_empty());
        assert!(out.wholesale_failed, "outer batch Err must set the wholesale flag");
        assert_eq!(out.slot_errors, 2, "every slot of the failed chunk is counted");
    }

    #[test]
    fn dirty_request_fetches_at_exactly_request_block() {
        let pool = [0xC4u8; 20];
        // Mock serves ONLY at block 42.
        let (port, _h) = spawn_block_aware_mock(
            vec![((pool, U256::from(5u64)), U256::from(0x55u64))],
            Some(42),
            0,
        );
        let client = mk_client_from_port(port);
        let h = ForkWarmerHandles::new(16);
        // Request pinned at 42 → the warmer must read at 42 (never the feed head 100) → served.
        h.req_q
            .push(WarmRequest {
                block: 42,
                reads: vec![(pool, U256::from(5u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(1)),
            })
            .unwrap();
        assert!(warmer_tick(&client, &h, 100, false, 4096, 1, 0));
        let b = h.out_q.pop().expect("bundle at the request block");
        assert_eq!(b.block, 42);
        assert_eq!(b.values, vec![(pool, U256::from(5u64), U256::from(0x55u64))]);
        // A request at a DIFFERENT block (43) yields nothing — proving the mock is genuinely
        // block-selective, so the success above can ONLY mean the warmer used req.block, not 100.
        h.req_q
            .push(WarmRequest {
                block: 43,
                reads: vec![(pool, U256::from(5u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(1)),
            })
            .unwrap();
        assert!(warmer_tick(&client, &h, 100, false, 4096, 1, 0));
        assert!(h.out_q.pop().is_none(), "a read at the wrong block yields no bundle");
    }

    #[test]
    fn wholesale_failure_retries_once_then_serves() {
        let pool = [0xC5u8; 20];
        // First storage round-trip wholesale-fails; the second serves (the pin==head boot race).
        let (port, hits) = spawn_block_aware_mock(
            vec![((pool, U256::from(9u64)), U256::from(0x99u64))],
            Some(50),
            1,
        );
        let client = mk_client_from_port(port);
        let h = ForkWarmerHandles::new(16);
        h.req_q
            .push(WarmRequest {
                block: 50,
                reads: vec![(pool, U256::from(9u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(1)),
            })
            .unwrap();
        // Tiny retry_ms keeps the test fast; the retry HAPPENING is what matters.
        assert!(warmer_tick(&client, &h, 100, false, 4096, 1, 5));
        let b = h.out_q.pop().expect("bundle produced after the one retry");
        assert_eq!(b.block, 50);
        assert_eq!(b.values, vec![(pool, U256::from(9u64), U256::from(0x99u64))]);
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "exactly one retry at the same block (2 round-trips)"
        );
    }

    #[test]
    fn wholesale_failure_retries_exactly_once_then_drops() {
        let pool = [0xC6u8; 20];
        // Every round-trip wholesale-fails → no bundle, and the retry is capped at ONE (2 trips).
        let (port, hits) = spawn_block_aware_mock(
            vec![((pool, U256::from(9u64)), U256::from(0x99u64))],
            Some(50),
            999,
        );
        let client = mk_client_from_port(port);
        let h = ForkWarmerHandles::new(16);
        h.req_q
            .push(WarmRequest {
                block: 50,
                reads: vec![(pool, U256::from(9u64))],
                purpose: WarmPurpose::PendingWarm,
            })
            .unwrap();
        assert!(warmer_tick(&client, &h, 100, false, 4096, 1, 1));
        assert!(h.out_q.pop().is_none(), "no bundle when the block never becomes available");
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one wholesale failure + exactly one retry, then drop (no unbounded loop)"
        );
    }

    #[test]
    fn full_out_queue_bumps_push_drops_not_slot_errors() {
        let pool = [0xC7u8; 20];
        let (port, _h) =
            spawn_block_aware_mock(vec![((pool, U256::from(1u64)), U256::from(0x11u64))], None, 0);
        let client = mk_client_from_port(port);
        let h = ForkWarmerHandles::new(1); // out_q capacity 1
        // Pre-fill out_q so the produced bundle cannot be pushed (queue pressure).
        h.out_q
            .push(WarmBundle {
                block: 1,
                values: vec![],
                purpose: WarmPurpose::DirtyPool(PoolId(9)),
            })
            .unwrap();
        h.req_q
            .push(WarmRequest {
                block: 5,
                reads: vec![(pool, U256::from(1u64))],
                purpose: WarmPurpose::DirtyPool(PoolId(1)),
            })
            .unwrap();
        // Monotonic global → assert a lower bound so parallel tests can't make this flaky.
        let drops_before = WARMER_PUSH_DROPS.load(Ordering::Relaxed);
        assert!(warmer_tick(&client, &h, 100, false, 4096, 1, 0));
        assert!(
            WARMER_PUSH_DROPS.load(Ordering::Relaxed) >= drops_before + 1,
            "a full queue bumps push-drops (queue pressure, not a provider error)"
        );
        // Deterministic proof the fetch succeeded yet the bundle was dropped (never delivered):
        // only the pre-filled dummy remains, not the real block-5 bundle.
        let remaining = h.out_q.pop().expect("dummy still queued");
        assert_eq!(remaining.block, 1, "the real bundle (block 5) was dropped, not delivered");
        assert!(h.out_q.pop().is_none(), "queue held only the one dummy");
    }
}
