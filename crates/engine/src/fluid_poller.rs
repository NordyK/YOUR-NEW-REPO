//! Fluid DEX background snapshot poller.
//!
//! Plan §6 — background control plane. Runs on a dedicated thread named
//! `engine-fluid-poller`. Periodically refreshes the per-pool snapshot for
//! every Fluid pool registered in the universe by calling the configured
//! resolvers, then atomically swaps the new `FluidPoolSnapshot` into the
//! `FluidStateStore` via `ArcSwap`. The hot path keeps reading lock-free.
//!
//! ## Today (v1/T1 production)
//!
//! For each registered Fluid v1 pool the poller calls
//! `DexReservesResolver.getPoolReservesAdjusted(address)` and decodes the
//! returned tuple into `V1SideReservesAdjusted` for both collateral and debt
//! sides, plus `constantsView2()` for the per-token precision numerators/
//! denominators (`TokenScale`). The same resolver payload also carries the
//! current withdrawable/borrowable limits; `getPoolLimits` is used when
//! available, otherwise those embedded limits are used as a conservative
//! fallback. Fee + pause flags come from the pool's `dexVariables2` storage
//! word. Together these fields fully populate `V1PoolState` and the kernel
//! can produce quotes.
//!
//! ## Tomorrow (v2 D3/D4)
//!
//! v2 D3/D4 pools require a per-pool `DexResolver.getPoolState(dexKey)`
//! decode plus a directional `getLiquidityAmounts(...)` walk to build the
//! `V2Boundary` arrays. This path is wired but currently no-ops because v2
//! is not GA on Arbitrum. When it lights up, plug in
//! `refresh_one_v2_pool` below.
//!
//! ## Failure semantics
//!
//! The poller is **fail-soft**: any per-pool resolver error logs at WARN
//! and leaves the previous snapshot in place. A pool whose first refresh
//! ever fails keeps its default-paused snapshot — the kernel returns
//! `PoolPaused` on any quote, which is correct (no execution against
//! state we cannot read).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use anyhow::Result;
use engine_amm::fluid::{
    TokenScale, V1ExpandableLimit, V1LimitsNative, V1PoolState, V1SideReservesAdjusted,
};
use engine_rpc::{ArbitrumRpcClient, BlockTag, RpcError};
use engine_state::{PoolId, PoolKind, StateSlot};
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info, warn};

use crate::pipeline::{submit_offthread_family_write, transitional_placeholder_point, DetectionContext};

/// Periodic safety-net cadence. Per the kernel's own
/// `POOL_DISCOVERY_AND_ARBITRUM_2026.md`: "Refresh a v1 pool whenever … no
/// relevant event for 5 to 10 minutes." Between event-driven refreshes we
/// run a full-table refresh at this cadence to defeat stale limit-expansion
/// state that doesn't emit pool events.
const FULL_REFRESH_INTERVAL_MS: u64 = 300_000;

/// Event-poll cadence. Arbitrum One average block time ≈ 250 ms; polling
/// `eth_getLogs` for `LogOperate` every 500 ms keeps dirty-pool detection
/// at most 2 blocks stale, well inside the FCFS window for arbitrage.
const EVENT_POLL_INTERVAL_MS: u64 = 500;

/// One snapshot poll attempt blocks for at most this long before yielding
/// the inner loop and re-checking the shutdown flag.
const POLL_BUDGET_MS: u64 = 1_500;

/// Reasonable per-cycle ceiling on the number of pools we refresh in one
/// pass. Above this, the poller logs at INFO and continues; the next pass
/// resumes where this one stopped.
const MAX_REFRESHES_PER_CYCLE: usize = 256;

/// Maximum block range per `eth_getLogs` LogOperate poll. Alchemy free tier
/// accepts up to 10 k blocks on topic-filtered queries; we stay well under.
const LOG_CHUNK_MAX_BLOCKS: u64 = 2_000;

/// Spawn the Fluid snapshot poller. Returns a `JoinHandle` so the engine
/// can join it cleanly on Ctrl+C. The thread name is `engine-fluid-poller`.
///
/// The poller drives two loops on the same thread:
///   * Every `EVENT_POLL_INTERVAL_MS` it scans the Liquidity Layer's
///     `LogOperate` events since the last cursor. Any event whose indexed
///     `user` matches a registered Fluid pool dirties that pool; only dirty
///     pools get refreshed.
///   * Every `FULL_REFRESH_INTERVAL_MS` it does a full-table refresh as a
///     safety net for limit-expansion drift the docs warn about (state can
///     become stale even when no pool event fires).
///
/// `resolver_v1` — `DexReservesResolver` address. `None` disables v1 polling.
/// `resolver_v2` — `DexResolver` address. `None` disables v2 polling (today
/// it is a no-op even when supplied).
/// `liquidity_layer` — Fluid Liquidity Layer address for `LogOperate`
/// subscription. `None` falls back to periodic-only refresh.
pub fn spawn_fluid_poller(
    rpc_url: String,
    detection: Arc<DetectionContext>,
    resolver_v1: Option<[u8; 20]>,
    resolver_v2: Option<[u8; 20]>,
    liquidity_layer: Option<[u8; 20]>,
    // WI-06 §3.3 T3: send-only command sender to the SOLE publisher. Every Fluid snapshot the poller
    // computes is submitted as a `StateMutationBatch` (Fluid origin) — the poller NEVER writes the
    // resident store directly. The state-thread drain applies it, bumps the generation, and emits the
    // DirtyEvent AFTER the commit (commit-before-wake), which replaces the old `FluidWakeHandle` wake.
    // `None` = no pipeline (offline) → the poller reads but cannot apply (fail-safe: pool stays paused).
    command_sender: Option<engine_state::publication::StateCommandSender>,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>> {
    // Snapshot the list of Fluid pools at boot. The universe is immutable
    // after build, so this is safe (and we avoid re-walking metas() every
    // tick). (PoolId, state_slot, address) per Fluid pool — PoolId keys the publisher batch;
    // PoolId is the dense `metas()` index (`meta(id) = &metas[id.0]`), so enumerate yields it directly.
    let fluid_pools: Vec<(PoolId, StateSlot, [u8; 20])> = detection
        .universe
        .pools
        .metas()
        .iter()
        .enumerate()
        .filter(|(_, m)| m.kind == PoolKind::FluidDex)
        .map(|(i, m)| (PoolId(i as u32), m.state_slot, m.address))
        .collect();

    if fluid_pools.is_empty() {
        info!("no Fluid pools registered — poller not spawned");
        return thread::Builder::new()
            .name("engine-fluid-poller".into())
            .spawn(move || {
                debug!("Fluid poller exiting immediately (no pools)");
            })
            .map_err(Into::into);
    }
    let pool_addr_set: HashSet<[u8; 20]> = fluid_pools.iter().map(|(_, _, a)| *a).collect();
    info!(
        pool_count = fluid_pools.len(),
        ?resolver_v1,
        ?resolver_v2,
        ?liquidity_layer,
        full_refresh_ms = FULL_REFRESH_INTERVAL_MS,
        event_poll_ms = EVENT_POLL_INTERVAL_MS,
        "spawning Fluid snapshot poller (event-driven + safety-net periodic)"
    );

    thread::Builder::new()
        .name("engine-fluid-poller".into())
        .spawn(move || {
            // RPC split: Fluid state freshness bounds exotic-route staleness — hot endpoint.
            let client = ArbitrumRpcClient::new(engine_rpc::hot_rpc_url().unwrap_or(rpc_url));
            // Pre-warm: do one full-table refresh at boot so the first
            // wave of route enumeration sees real state. `refresh_cursor` rotates the
            // refresh window across passes so >256-pool universes don't starve their tail.
            let mut refresh_cursor = 0usize;
            refresh_cursor = run_full_refresh(
                &client,
                &detection,
                command_sender.as_ref(),
                &fluid_pools,
                resolver_v1,
                resolver_v2,
                refresh_cursor,
            );
            let mut last_full = Instant::now();
            let mut last_event_poll = Instant::now();
            let mut event_cursor_block: Option<u64> = match client.block_number() {
                Ok(bn) => Some(bn),
                Err(e) => {
                    warn!(?e, "Fluid poller: initial block_number failed; event poll disabled until next success");
                    None
                }
            };

            while !shutdown.load(Ordering::Acquire) {
                // Periodic safety-net refresh.
                if last_full.elapsed() >= Duration::from_millis(FULL_REFRESH_INTERVAL_MS) {
                    last_full = Instant::now();
                    refresh_cursor = run_full_refresh(
                        &client,
                        &detection,
                        command_sender.as_ref(),
                        &fluid_pools,
                        resolver_v1,
                        resolver_v2,
                        refresh_cursor,
                    );
                }
                // Event-driven dirty-pool refresh.
                if last_event_poll.elapsed() >= Duration::from_millis(EVENT_POLL_INTERVAL_MS) {
                    last_event_poll = Instant::now();
                    if let (Some(ll), Some(cursor)) = (liquidity_layer, event_cursor_block) {
                        match poll_log_operate_dirty(&client, &ll, &pool_addr_set, cursor) {
                            Ok((dirty, next_cursor)) => {
                                event_cursor_block = Some(next_cursor);
                                if !dirty.is_empty() {
                                    // WI-06 §3.3 T3: refresh_dirty submits each changed pool's
                                    // snapshot as a StateMutationBatch; the state-thread drain
                                    // commits it, bumps the generation, and emits the DirtyEvent
                                    // AFTER (commit-before-wake) — so there is NO separate wake call
                                    // here (a second wake would be a double-wake).
                                    refresh_dirty(
                                        &client,
                                        &detection,
                                        command_sender.as_ref(),
                                        &fluid_pools,
                                        resolver_v1,
                                        &dirty,
                                    );
                                }
                            }
                            Err(e) => {
                                debug!(?e, "LogOperate poll failed; will retry next tick");
                            }
                        }
                    } else if event_cursor_block.is_none() {
                        // Re-arm the cursor on a subsequent tick.
                        if let Ok(bn) = client.block_number() {
                            event_cursor_block = Some(bn);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
            info!("Fluid snapshot poller exiting");
        })
        .map_err(Into::into)
}

/// Outcome of routing a Fluid snapshot to the sole publisher.
enum FluidSubmit {
    /// Batched to the publisher (it will apply + bump + emit on the drain).
    Applied,
    /// No pipeline (offline) — the snapshot cannot be applied (fail-safe: pool stays paused).
    NoSender,
    /// The command queue was full / the publisher is gone — `submit_batch` already disarmed
    /// publication authority + scheduled reconciliation; the caller should stop this pass.
    Rejected,
}

/// WI-06 §3.3 T3: submit one Fluid v1 pool snapshot to the sole publisher as a single-mutation
/// `StateMutationBatch` (Fluid origin). The poller NEVER writes `fluid_state` directly.
fn fluid_submit(
    sender: Option<&engine_state::publication::StateCommandSender>,
    // WI-06 F2 fix-round-2: the pool generation the caller captured BEFORE the RPC refresh below —
    // threaded so a WSS/receipt commit that landed during the RPC refuses this Fluid snapshot
    // rather than clobbering the fresher committed state.
    snapshot_generation: engine_state::Generation,
    pool_id: PoolId,
    new_state: V1PoolState,
) -> FluidSubmit {
    let Some(sender) = sender else {
        return FluidSubmit::NoSender;
    };
    match submit_offthread_family_write(
        sender,
        engine_state::publication::BarrierOriginClass::Fluid,
        pool_id,
        snapshot_generation,
        // Fluid poll captures `snapshot_generation` at read time — the F3 generation check suffices;
        // no shadow-style pre-replay-seq guard needed.
        engine_state::publication::UNVALIDATED_PRE_REPLAY_SEQ,
        None,
        transitional_placeholder_point(),
        engine_state::publication::FamilyWrite::Fluid(Box::new(engine_state::FluidPoolSnapshot::V1(
            new_state,
        ))),
    ) {
        Ok(()) => FluidSubmit::Applied,
        Err(_) => FluidSubmit::Rejected,
    }
}

/// Refresh up to `MAX_REFRESHES_PER_CYCLE` pools per pass, starting at `start` and
/// wrapping around the pool list. Returns the cursor for the next pass so that when
/// the universe has more than `MAX_REFRESHES_PER_CYCLE` Fluid pools, every pool is
/// eventually refreshed instead of only the first window each time (which would leave
/// tail pools permanently stale). With ≤ `MAX_REFRESHES_PER_CYCLE` pools the window
/// covers the whole list every pass and the cursor is a no-op.
fn run_full_refresh(
    client: &ArbitrumRpcClient,
    detection: &Arc<DetectionContext>,
    sender: Option<&engine_state::publication::StateCommandSender>,
    pools: &[(PoolId, StateSlot, [u8; 20])],
    resolver_v1: Option<[u8; 20]>,
    _resolver_v2: Option<[u8; 20]>,
    start: usize,
) -> usize {
    let n = pools.len();
    if n == 0 {
        return 0;
    }
    let pass_start = Instant::now();
    let mut refreshed = 0usize;
    let mut errored = 0usize;
    let window = n.min(MAX_REFRESHES_PER_CYCLE);
    let mut attempted = 0usize;
    while attempted < window {
        if pass_start.elapsed() > Duration::from_millis(POLL_BUDGET_MS) {
            debug!(
                refreshed,
                remaining = window - attempted,
                "Fluid poller: full-refresh budget exhausted; remainder defers to next pass"
            );
            break;
        }
        let (pool_id, _slot, addr) = &pools[(start + attempted) % n];
        attempted += 1;
        if let Some(resolver) = resolver_v1 {
            // WI-06 F2 fix-round-2: capture the pool generation BEFORE the RPC refresh (the "source"
            // read), so a commit landing during the refresh refuses this snapshot.
            let snapshot_generation = detection.universe.pools.generation(*pool_id);
            match refresh_one_v1_pool(client, &resolver, addr) {
                Ok(new_state) => {
                    // WI-06 §3.3 T3: submit the fresh snapshot to the SOLE publisher (Fluid origin)
                    // — the publisher writes the store, bumps the generation, and the drain emits the
                    // DirtyEvent AFTER commit (no direct store write off-thread). No sender ⇒ no
                    // pipeline (offline): fail-safe, the snapshot cannot be applied and the pool stays
                    // paused rather than written by a non-sole writer.
                    match fluid_submit(sender, snapshot_generation, *pool_id, new_state) {
                        FluidSubmit::Applied => refreshed += 1,
                        FluidSubmit::NoSender => {}
                        FluidSubmit::Rejected => {
                            debug!(pool = ?addr, "Fluid full-refresh batch rejected — publication authority disarmed; deferring remainder");
                            break;
                        }
                    }
                }
                Err(e) => {
                    debug!(pool = ?addr, ?e, "Fluid v1 full-refresh failed; keeping prior snapshot");
                    errored += 1;
                }
            }
        }
    }
    // INFO (not debug): a Fluid pool only quotes once its snapshot is populated,
    // so the refreshed/errored split is operationally important — it tells us
    // whether the venue is actually live. Low cadence (5 min) keeps noise down.
    info!(
        refreshed,
        errored,
        pools = n,
        elapsed_ms = pass_start.elapsed().as_millis() as u64,
        "Fluid poller full-refresh pass complete"
    );
    // Advance the cursor past the pools we attempted this pass.
    (start + attempted) % n
}

/// Targeted refresh for a set of dirty pool addresses (LogOperate event
/// matched a registered pool). Iterates the full pool table to find slots
/// matching the dirty set — this is O(pools × dirty) but the constant is
/// tiny (Fluid pool count is small) and we avoid the HashMap allocation
/// hot-path concerns of a per-pool lookup table.
fn refresh_dirty(
    client: &ArbitrumRpcClient,
    detection: &Arc<DetectionContext>,
    sender: Option<&engine_state::publication::StateCommandSender>,
    pools: &[(PoolId, StateSlot, [u8; 20])],
    resolver_v1: Option<[u8; 20]>,
    dirty: &HashSet<[u8; 20]>,
) {
    let resolver = match resolver_v1 {
        Some(r) => r,
        None => return,
    };
    let pass_start = Instant::now();
    let mut refreshed = 0usize;
    let mut errored = 0usize;
    for (pool_id, _slot, addr) in pools {
        if !dirty.contains(addr) {
            continue;
        }
        if pass_start.elapsed() > Duration::from_millis(POLL_BUDGET_MS) {
            break;
        }
        // WI-06 F2 fix-round-2: capture the generation BEFORE the RPC refresh (source read).
        let snapshot_generation = detection.universe.pools.generation(*pool_id);
        match refresh_one_v1_pool(client, &resolver, addr) {
            Ok(new_state) => {
                // WI-06 §3.3 T3: route to the sole publisher (Fluid origin); the drain applies +
                // bumps + emits the DirtyEvent AFTER commit (no direct off-thread store write).
                match fluid_submit(sender, snapshot_generation, *pool_id, new_state) {
                    FluidSubmit::Applied => refreshed += 1,
                    FluidSubmit::NoSender => {}
                    FluidSubmit::Rejected => {
                        debug!(pool = ?addr, "Fluid dirty-refresh batch rejected — publication authority disarmed");
                        break;
                    }
                }
            }
            Err(e) => {
                debug!(pool = ?addr, ?e, "Fluid v1 dirty-refresh failed");
                errored += 1;
            }
        }
    }
    if refreshed > 0 || errored > 0 {
        debug!(
            refreshed,
            errored,
            dirty_count = dirty.len(),
            elapsed_ms = pass_start.elapsed().as_millis() as u64,
            "Fluid poller dirty-refresh complete"
        );
    }
}

/// Poll the Liquidity Layer for `LogOperate` events since `cursor_block`.
/// Returns `(dirty_pool_addresses, next_cursor)` where `next_cursor` is the
/// block just past the highest one we scanned (so the next poll resumes
/// without overlap).
///
/// Event signature: `LogOperate(address indexed user, address indexed token,
/// int256 supplyAmount, int256 borrowAmount, address withdrawTo, address
/// borrowTo, uint256 totalAmounts, uint256 exchangePricesAndConfig)`.
/// `user` is `topics[1]`; per the kernel docs we filter for `user == pool`.
fn poll_log_operate_dirty(
    client: &ArbitrumRpcClient,
    liquidity_layer: &[u8; 20],
    pool_set: &HashSet<[u8; 20]>,
    cursor_block: u64,
) -> Result<(HashSet<[u8; 20]>, u64), RpcError> {
    let head = client.block_number()?;
    if head < cursor_block {
        // RPC reorg or cursor ahead — re-arm at head.
        return Ok((HashSet::new(), head));
    }
    let from = cursor_block;
    let to = head.min(cursor_block.saturating_add(LOG_CHUNK_MAX_BLOCKS));
    let topic0 = log_operate_topic();
    let logs = client.call(
        "eth_getLogs",
        serde_json::json!([{
            "address": format!("0x{}", faster_hex::hex_string(liquidity_layer.as_slice())),
            "fromBlock": format!("0x{from:x}"),
            "toBlock": format!("0x{to:x}"),
            "topics": [format!("0x{}", faster_hex::hex_string(topic0.as_slice()))],
        }]),
    )?;
    let mut dirty = HashSet::new();
    if let Some(arr) = logs.as_array() {
        for entry in arr {
            let topics = entry.get("topics").and_then(|t| t.as_array());
            let topics = match topics {
                Some(t) if t.len() >= 2 => t,
                _ => continue,
            };
            let user_hex = match topics[1].as_str() {
                Some(s) => s,
                None => continue,
            };
            // user_hex is "0x" + 64 hex chars; address is the low 20 bytes.
            let trimmed = user_hex.strip_prefix("0x").unwrap_or(user_hex);
            if trimmed.len() != 64 {
                continue;
            }
            let mut word = [0u8; 32];
            if faster_hex::hex_decode(trimmed.as_bytes(), &mut word).is_err() {
                continue;
            }
            if word[0..12].iter().any(|&b| b != 0) {
                continue;
            }
            let mut addr = [0u8; 20];
            addr.copy_from_slice(&word[12..32]);
            if pool_set.contains(&addr) {
                dirty.insert(addr);
            }
        }
    }
    Ok((dirty, to.saturating_add(1)))
}

fn log_operate_topic() -> [u8; 32] {
    let mut k = Keccak::v256();
    k.update(b"LogOperate(address,address,int256,int256,address,address,uint256,uint256)");
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

/// Refresh one v1 pool. Reads:
///   * `getPoolReservesAdjusted(pool)` from the resolver — collateral +
///     debt adjusted reserves.
///   * `getPoolLimits(pool)` from the resolver — per-side withdrawable +
///     borrowable expandable limits (`V1ExpandableLimit` × 4).
///   * `pool.readFromStorage(DEX_VARIABLES2_SLOT)` — fee + enabled/paused flags.
///   * `pool.constantsView2()` — per-token precision numerators/denominators
///     for `TokenScale`.
fn refresh_one_v1_pool(
    client: &ArbitrumRpcClient,
    resolver: &[u8; 20],
    pool: &[u8; 20],
) -> Result<V1PoolState, RpcError> {
    refresh_one_v1_pool_at(client, resolver, pool, BlockTag::Latest)
}

/// Refresh one Fluid v1 pool at a specific block tag.
///
/// Bounded file replay uses this to hydrate the Fluid snapshot from the same
/// block as a replayed receipt before dirty-route simulation runs.
pub fn refresh_one_v1_pool_at(
    client: &ArbitrumRpcClient,
    resolver: &[u8; 20],
    pool: &[u8; 20],
    block: BlockTag,
) -> Result<V1PoolState, RpcError> {
    // 1) getPoolReservesAdjusted(pool) — returns the collateral + debt
    //    reserves and, on current T1 resolvers, the active limit triples.
    let reserves_call = encode_address_call(&selector_get_pool_reserves_adjusted(), pool);
    let reserves_raw = client.eth_call(resolver, &reserves_call, block)?;
    let reserves = decode_pool_reserves_adjusted(&reserves_raw)
        .map_err(|e| RpcError::Decode(format!("getPoolReservesAdjusted: {e}")))?;
    let limits_from_reserves =
        decode_pool_limits_from_reserves_adjusted(&reserves_raw).unwrap_or_else(|e| {
            debug!(
                pool = ?pool,
                err = e,
                "getPoolReservesAdjusted limit-tail decode failed; using default limits if getPoolLimits is unavailable"
            );
            V1LimitsNative::default()
        });

    // 2) getPoolLimits(pool) — expandable native-token limits with snapshot
    //    timestamp. Fail-soft: if the call reverts (resolver method missing
    //    on a particular Fluid contract version) we fall back to the limit
    //    tail already returned by getPoolReservesAdjusted.
    let limits_call = encode_address_call(&selector_get_pool_limits(), pool);
    let limits = match client.eth_call(resolver, &limits_call, block) {
        Ok(raw) => decode_pool_limits(&raw).unwrap_or_else(|e| {
            debug!(pool = ?pool, err = e, "getPoolLimits decode failed; using reserves-adjusted limits");
            limits_from_reserves
        }),
        Err(e) => {
            debug!(pool = ?pool, ?e, "getPoolLimits revert; using reserves-adjusted limits");
            limits_from_reserves
        }
    };

    // 3) pool.readFromStorage(DEX_VARIABLES2_SLOT) — fee_1e6, paused,
    //    smart-collateral and smart-debt enable flags. These fields are not
    //    part of the public constantsView() struct on current T1 deployments.
    let dex_variables2_call =
        encode_bytes32_call(&selector_read_from_storage(), &dex_variables2_slot());
    let dex_variables2_raw = client.eth_call(pool, &dex_variables2_call, block)?;
    let dex_variables2 = decode_dex_variables2(&dex_variables2_raw)
        .map_err(|e| RpcError::Decode(format!("dexVariables2: {e}")))?;

    // 4) pool.constantsView2() — token precision numerators + denominators.
    let cv2_raw = client.eth_call(pool, &selector_constants_view2(), block)?;
    let scales = decode_constants_view2(&cv2_raw)
        .map_err(|e| RpcError::Decode(format!("constantsView2: {e}")))?;

    Ok(V1PoolState {
        fee_1e6: dex_variables2.fee_1e6,
        token0_scale: TokenScale {
            numerator: scales.token0_numerator,
            denominator: scales.token0_denominator,
        },
        token1_scale: TokenScale {
            numerator: scales.token1_numerator,
            denominator: scales.token1_denominator,
        },
        collateral: reserves.collateral,
        debt: reserves.debt,
        limits,
        smart_collateral_enabled: dex_variables2.smart_collateral_enabled,
        smart_debt_enabled: dex_variables2.smart_debt_enabled,
        swaps_paused: dex_variables2.swaps_paused,
    })
}

/// Build calldata for a `f(address)` resolver method — 4-byte selector +
/// 32-byte left-padded address word.
fn encode_address_call(selector: &[u8; 4], addr: &[u8; 20]) -> Vec<u8> {
    let mut out = Vec::with_capacity(36);
    out.extend_from_slice(selector);
    out.extend_from_slice(&[0u8; 12]);
    out.extend_from_slice(addr);
    out
}

fn encode_bytes32_call(selector: &[u8; 4], word: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(36);
    out.extend_from_slice(selector);
    out.extend_from_slice(word);
    out
}

// === selectors ===

fn selector_get_pool_reserves_adjusted() -> [u8; 4] {
    keccak4("getPoolReservesAdjusted(address)")
}

fn selector_constants_view2() -> [u8; 4] {
    keccak4("constantsView2()")
}

fn selector_read_from_storage() -> [u8; 4] {
    keccak4("readFromStorage(bytes32)")
}

fn selector_get_pool_limits() -> [u8; 4] {
    keccak4("getPoolLimits(address)")
}

fn dex_variables2_slot() -> [u8; 32] {
    let mut slot = [0u8; 32];
    slot[31] = 1;
    slot
}

fn keccak4(sig: &str) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig.as_bytes());
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

// === decoders ===

#[derive(Debug)]
struct ReservesPair {
    collateral: V1SideReservesAdjusted,
    debt: V1SideReservesAdjusted,
}

/// Decode `getPoolReservesAdjusted(address)` into both reserve sides.
///
/// The resolver returns `PoolWithReserves` as a head-only tuple. The reserve
/// fields start after pool/token/fee/centerPrice:
///
/// ```text
///   word  0: pool
///   word  1: token0
///   word  2: token1
///   word  3: fee
///   word  4: centerPrice
///   word  5: collateral.token0_real
///   word  6: collateral.token1_real
///   word  7: collateral.token0_imag
///   word  8: collateral.token1_imag
///   word  9: debt.token0_debt
///   word 10: debt.token1_debt
///   word 11: debt.token0_real
///   word 12: debt.token1_real
///   word 13: debt.token0_imag
///   word 14: debt.token1_imag
/// ```
///
/// The remaining words are limits. This decoder consumes only the reserve
/// fields; `getPoolLimits`/limit decoding remains separate.
fn decode_pool_reserves_adjusted(raw: &[u8]) -> Result<ReservesPair, &'static str> {
    if raw.len() < 15 * 32 {
        return Err("payload < 15 words");
    }
    let w = |i: usize| -> U256 {
        let s = i * 32;
        U256::from_be_slice(&raw[s..s + 32])
    };
    Ok(ReservesPair {
        collateral: V1SideReservesAdjusted {
            token0_real: w(5),
            token1_real: w(6),
            token0_imaginary: w(7),
            token1_imaginary: w(8),
        },
        debt: V1SideReservesAdjusted {
            token0_real: w(11),
            token1_real: w(12),
            token0_imaginary: w(13),
            token1_imaginary: w(14),
        },
    })
}

#[derive(Debug)]
struct DexVariables2 {
    fee_1e6: u32,
    smart_collateral_enabled: bool,
    smart_debt_enabled: bool,
    swaps_paused: bool,
}

/// Decode Fluid v1/T1 `dexVariables2` from `readFromStorage(slot 1)`.
///
/// ```text
/// bit 0      smart collateral enabled
/// bit 1      smart debt enabled
/// bits 2-18  fee, in 1e6 precision
/// bit 255    swap/arbitrage pause flag
/// ```
fn decode_dex_variables2(raw: &[u8]) -> Result<DexVariables2, &'static str> {
    if raw.len() < 32 {
        return Err("payload < 1 word");
    }
    let word = U256::from_be_slice(&raw[0..32]);
    let fee_word = (word >> 2) & U256::from(0x1ffffu64);
    Ok(DexVariables2 {
        fee_1e6: u64::try_from(fee_word).unwrap_or(0) as u32,
        smart_collateral_enabled: (word & U256::from(1u8)) != U256::ZERO,
        smart_debt_enabled: (word & U256::from(2u8)) != U256::ZERO,
        swaps_paused: ((word >> 255) & U256::from(1u8)) != U256::ZERO,
    })
}

#[derive(Debug)]
struct ConstantsView2 {
    token0_numerator: U256,
    token0_denominator: U256,
    token1_numerator: U256,
    token1_denominator: U256,
}

/// Decode `pool.constantsView2()` returning the four precision values used
/// to build the `TokenScale`s. Layout:
///
/// ```text
///   word 0:  token0_numerator_precision
///   word 1:  token0_denominator_precision
///   word 2:  token1_numerator_precision
///   word 3:  token1_denominator_precision
/// ```
///
/// Both denominators must be non-zero — the kernel divides by them and
/// will panic-return-error on zero. We fall back to unit scale {1, 1} if
/// the resolver returns zeros (cold pool not yet initialised).
fn decode_constants_view2(raw: &[u8]) -> Result<ConstantsView2, &'static str> {
    if raw.len() < 128 {
        return Err("payload < 4 words");
    }
    let mut t0n = U256::from_be_slice(&raw[0..32]);
    let mut t0d = U256::from_be_slice(&raw[32..64]);
    let mut t1n = U256::from_be_slice(&raw[64..96]);
    let mut t1d = U256::from_be_slice(&raw[96..128]);
    let one = U256::from(1u8);
    if t0d.is_zero() {
        t0n = one;
        t0d = one;
    }
    if t1d.is_zero() {
        t1n = one;
        t1d = one;
    }
    Ok(ConstantsView2 {
        token0_numerator: t0n,
        token0_denominator: t0d,
        token1_numerator: t1n,
        token1_denominator: t1d,
    })
}

/// Decode `getPoolLimits(address)` into `V1LimitsNative`. The resolver
/// returns four `V1ExpandableLimit` triples (available, expandsTo,
/// expandDuration) plus a snapshot timestamp:
///
/// ```text
///   word  0: withdrawable_token0.available_native
///   word  1: withdrawable_token0.expands_to_native
///   word  2: withdrawable_token0.expand_duration_left_secs (low 64 bits)
///   word  3: withdrawable_token1.available_native
///   word  4: withdrawable_token1.expands_to_native
///   word  5: withdrawable_token1.expand_duration_left_secs
///   word  6: borrowable_token0.available_native
///   word  7: borrowable_token0.expands_to_native
///   word  8: borrowable_token0.expand_duration_left_secs
///   word  9: borrowable_token1.available_native
///   word 10: borrowable_token1.expands_to_native
///   word 11: borrowable_token1.expand_duration_left_secs
///   word 12: snapshot_timestamp (low 64 bits)
/// ```
///
/// 13 head-only uint256 words = 416 bytes. If the deployed resolver uses
/// a different layout the kernel falls back to its zero-limit guards
/// (which manifest as `LimitExceeded` errors on quote, not corrupt fills).
fn decode_pool_limits(raw: &[u8]) -> Result<V1LimitsNative, &'static str> {
    if raw.len() < 13 * 32 {
        return Err("payload < 13 words");
    }
    let read = |i: usize| -> U256 {
        let s = i * 32;
        U256::from_be_slice(&raw[s..s + 32])
    };
    let read_u64 = |i: usize| -> u64 {
        let s = i * 32;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&raw[s + 24..s + 32]);
        u64::from_be_bytes(bytes)
    };
    let triple = |base: usize| V1ExpandableLimit {
        available_native: read(base),
        expands_to_native: read(base + 1),
        expand_duration_left_secs: read_u64(base + 2),
    };
    Ok(V1LimitsNative {
        withdrawable_token0: triple(0),
        withdrawable_token1: triple(3),
        borrowable_token0: triple(6),
        borrowable_token1: triple(9),
        snapshot_timestamp: read_u64(12),
    })
}

/// Decode the four limit triples embedded in the
/// `getPoolReservesAdjusted(address)` `PoolWithReserves` payload.
///
/// These words are current available/expand-to/duration values, but the
/// payload does not include a separate timestamp. Use `u64::MAX` so
/// `expand_v1_limit` consumes the available values as-is instead of growing
/// them from an unknown snapshot time.
fn decode_pool_limits_from_reserves_adjusted(raw: &[u8]) -> Result<V1LimitsNative, &'static str> {
    if raw.len() < 27 * 32 {
        return Err("payload < 27 words");
    }
    let read = |i: usize| -> U256 {
        let s = i * 32;
        U256::from_be_slice(&raw[s..s + 32])
    };
    let read_u64 = |i: usize| -> u64 {
        let s = i * 32;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&raw[s + 24..s + 32]);
        u64::from_be_bytes(bytes)
    };
    let triple = |base: usize| V1ExpandableLimit {
        available_native: read(base),
        expands_to_native: read(base + 1),
        expand_duration_left_secs: read_u64(base + 2),
    };
    Ok(V1LimitsNative {
        withdrawable_token0: triple(15),
        withdrawable_token1: triple(18),
        borrowable_token0: triple(21),
        borrowable_token1: triple(24),
        snapshot_timestamp: u64::MAX,
    })
}

/// Parse a hex `0x...` resolver address from config into a 20-byte array.
/// Returns `None` for empty / malformed strings so the spawn path can skip
/// silently.
pub fn parse_resolver_addr(s: &str) -> Option<[u8; 20]> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    if trimmed.len() != 40 {
        if !s.is_empty() {
            warn!(addr = %s, "Fluid resolver address has wrong length (must be 20 bytes hex)");
        }
        return None;
    }
    let mut out = [0u8; 20];
    if faster_hex::hex_decode(trimmed.as_bytes(), &mut out).is_ok() {
        Some(out)
    } else {
        warn!(addr = %s, "Fluid resolver address not valid hex");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_are_stable() {
        let a = selector_get_pool_reserves_adjusted();
        let b = selector_get_pool_reserves_adjusted();
        assert_eq!(a, b);
        // Different sigs → different selectors.
        assert_ne!(a, selector_read_from_storage());
        assert_ne!(a, selector_constants_view2());
        assert_ne!(selector_read_from_storage(), selector_constants_view2());
    }

    #[test]
    fn decode_reserves_rejects_short() {
        let raw = vec![0u8; 100];
        assert!(decode_pool_reserves_adjusted(&raw).is_err());
    }

    #[test]
    fn decode_reserves_skips_pool_with_reserves_header() {
        let mut raw = vec![0u8; 27 * 32];
        let set = |raw: &mut [u8], word: usize, value: u64| {
            let start = word * 32;
            raw[start + 24..start + 32].copy_from_slice(&value.to_be_bytes());
        };
        set(&mut raw, 5, 10);
        set(&mut raw, 6, 20);
        set(&mut raw, 7, 30);
        set(&mut raw, 8, 40);
        set(&mut raw, 9, 50);
        set(&mut raw, 10, 60);
        set(&mut raw, 11, 70);
        set(&mut raw, 12, 80);
        set(&mut raw, 13, 90);
        set(&mut raw, 14, 100);

        let r = decode_pool_reserves_adjusted(&raw).unwrap();
        assert_eq!(r.collateral.token0_real, U256::from(10u64));
        assert_eq!(r.collateral.token1_real, U256::from(20u64));
        assert_eq!(r.collateral.token0_imaginary, U256::from(30u64));
        assert_eq!(r.collateral.token1_imaginary, U256::from(40u64));
        assert_eq!(r.debt.token0_real, U256::from(70u64));
        assert_eq!(r.debt.token1_real, U256::from(80u64));
        assert_eq!(r.debt.token0_imaginary, U256::from(90u64));
        assert_eq!(r.debt.token1_imaginary, U256::from(100u64));
    }

    #[test]
    fn decode_limits_from_pool_with_reserves_tail() {
        let mut raw = vec![0u8; 27 * 32];
        let set = |raw: &mut [u8], word: usize, value: u64| {
            let start = word * 32;
            raw[start + 24..start + 32].copy_from_slice(&value.to_be_bytes());
        };
        set(&mut raw, 15, 10);
        set(&mut raw, 16, 20);
        set(&mut raw, 17, 30);
        set(&mut raw, 18, 40);
        set(&mut raw, 19, 50);
        set(&mut raw, 20, 60);
        set(&mut raw, 21, 70);
        set(&mut raw, 22, 80);
        set(&mut raw, 23, 90);
        set(&mut raw, 24, 100);
        set(&mut raw, 25, 110);
        set(&mut raw, 26, 120);

        let l = decode_pool_limits_from_reserves_adjusted(&raw).unwrap();
        assert_eq!(l.withdrawable_token0.available_native, U256::from(10u64));
        assert_eq!(l.withdrawable_token1.expands_to_native, U256::from(50u64));
        assert_eq!(l.borrowable_token0.expand_duration_left_secs, 90);
        assert_eq!(l.borrowable_token1.available_native, U256::from(100u64));
        assert_eq!(l.snapshot_timestamp, u64::MAX);
    }

    #[test]
    fn decode_constants_view2_unit_scale_on_zero_denom() {
        let raw = vec![0u8; 128]; // all zeros
        let r = decode_constants_view2(&raw).unwrap();
        assert_eq!(r.token0_denominator, U256::from(1u8));
        assert_eq!(r.token1_denominator, U256::from(1u8));
    }

    #[test]
    fn decode_constants_view2_preserves_nonzero() {
        let mut raw = vec![0u8; 128];
        // word 0 = 1e6, word 1 = 1, word 2 = 1, word 3 = 1e12
        raw[28..32].copy_from_slice(&(1_000_000u32).to_be_bytes());
        raw[63] = 1;
        raw[95] = 1;
        raw[120..128].copy_from_slice(&(1_000_000_000_000u64).to_be_bytes());
        let r = decode_constants_view2(&raw).unwrap();
        assert_eq!(r.token0_numerator, U256::from(1_000_000u64));
        assert_eq!(r.token0_denominator, U256::from(1u8));
        assert_eq!(r.token1_numerator, U256::from(1u8));
        assert_eq!(r.token1_denominator, U256::from(1_000_000_000_000u64));
    }

    #[test]
    fn decode_dex_variables2_e066_pool_enables_smart_debt() {
        let mut raw = [0u8; 32];
        faster_hex::hex_decode(
            b"00fa3e8ceb1146cee6146000000000000000ffffff00000002ee002ee0c8001e",
            &mut raw,
        )
        .unwrap();
        let r = decode_dex_variables2(&raw).unwrap();
        assert_eq!(r.fee_1e6, 7);
        assert!(!r.smart_collateral_enabled);
        assert!(r.smart_debt_enabled);
        assert!(!r.swaps_paused);
    }

    #[test]
    fn parse_resolver_addr_accepts_valid() {
        let s = "0x05Bd8269A20C472b148246De20E6852091BF16Ff";
        let a = parse_resolver_addr(s).unwrap();
        assert_eq!(a[0], 0x05);
        assert_eq!(a[19], 0xFF);
    }

    #[test]
    fn parse_resolver_addr_rejects_empty() {
        assert!(parse_resolver_addr("").is_none());
    }

    #[test]
    fn parse_resolver_addr_rejects_short() {
        assert!(parse_resolver_addr("0x1234").is_none());
    }

    #[test]
    fn log_operate_topic_stable() {
        let a = log_operate_topic();
        let b = log_operate_topic();
        assert_eq!(a, b);
        // Non-trivial.
        assert!(a.iter().any(|&b| b != 0));
    }

    #[test]
    fn decode_pool_limits_full_layout() {
        // 13 words: 4 triples + timestamp. Each triple encodes
        // (available, expands_to, expand_duration_secs).
        let mut raw = vec![0u8; 13 * 32];
        // withdrawable_token0
        raw[24..32].copy_from_slice(&100u64.to_be_bytes());
        raw[32 + 24..32 + 32].copy_from_slice(&200u64.to_be_bytes());
        raw[64 + 24..64 + 32].copy_from_slice(&10u64.to_be_bytes());
        // withdrawable_token1
        raw[96 + 24..96 + 32].copy_from_slice(&300u64.to_be_bytes());
        raw[128 + 24..128 + 32].copy_from_slice(&400u64.to_be_bytes());
        raw[160 + 24..160 + 32].copy_from_slice(&20u64.to_be_bytes());
        // borrowable_token0
        raw[192 + 24..192 + 32].copy_from_slice(&500u64.to_be_bytes());
        raw[224 + 24..224 + 32].copy_from_slice(&600u64.to_be_bytes());
        raw[256 + 24..256 + 32].copy_from_slice(&30u64.to_be_bytes());
        // borrowable_token1
        raw[288 + 24..288 + 32].copy_from_slice(&700u64.to_be_bytes());
        raw[320 + 24..320 + 32].copy_from_slice(&800u64.to_be_bytes());
        raw[352 + 24..352 + 32].copy_from_slice(&40u64.to_be_bytes());
        // snapshot_timestamp
        raw[384 + 24..384 + 32].copy_from_slice(&1_700_000_000u64.to_be_bytes());

        let l = decode_pool_limits(&raw).unwrap();
        assert_eq!(l.withdrawable_token0.available_native, U256::from(100u64));
        assert_eq!(l.withdrawable_token0.expands_to_native, U256::from(200u64));
        assert_eq!(l.withdrawable_token0.expand_duration_left_secs, 10);
        assert_eq!(l.withdrawable_token1.available_native, U256::from(300u64));
        assert_eq!(l.borrowable_token0.available_native, U256::from(500u64));
        assert_eq!(l.borrowable_token1.expand_duration_left_secs, 40);
        assert_eq!(l.snapshot_timestamp, 1_700_000_000);
    }

    #[test]
    fn decode_pool_limits_rejects_short() {
        let raw = vec![0u8; 13 * 32 - 1];
        assert!(decode_pool_limits(&raw).is_err());
    }

    #[test]
    fn encode_address_call_shape() {
        let sel = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut addr = [0u8; 20];
        addr[19] = 0xFF;
        let v = encode_address_call(&sel, &addr);
        assert_eq!(v.len(), 36);
        assert_eq!(&v[..4], &sel);
        // Padding zeros.
        for &b in &v[4..16] {
            assert_eq!(b, 0);
        }
        // Address tail.
        assert_eq!(v[35], 0xFF);
    }
}
