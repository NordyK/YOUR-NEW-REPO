//! Startup orchestrator.
//!
//! Implements the §9 startup sequence steps that are wired into the production binary:
//! parses the JSON config, **discovers pools dynamically** via factory event scan
//! (§5.4 gate 1), validates each via codehash + token-pair gates (§5.4 gates 2–3),
//! snapshots V2 / V3 pool state at a fixed block tag (§5.4 gate 3), scores each pool
//! (§6.1), elects the top-N as the hot tier (§5.5 / §6.2), enumerates the routes per
//! topology config, builds the CSR pool → hot-route index, and returns the immutable
//! `DetectionContext` the pipeline runs against.
//!
//! The static `pools` array on the config was removed per the plan's "No static pool
//! whitelist" rule (§line 2116). Every pool comes through `engine-discovery`.
//!
//! RPC calls are background-only (plan §0.4 rule 1) and run before the hot path starts.

use alloy_primitives::U256;
use anyhow::{anyhow, Context, Result};
use tracing::{info, warn};

use ahash::AHashSet;
use engine_amm::TickInfo;
use engine_config::EngineConfig;
use engine_detect::SelectorRegistry;
use engine_discovery::{
    discover_algebra_pools, discover_fluid_v1_pools, discover_fluid_v2_pools, discover_lb_pools,
    discover_v2_pools, discover_v3_pools, discover_v4_pools, score_pool, AlgebraFactoryConfig,
    CodehashAllowlist, DiscoveredPool, FluidResolverConfig, FluidVariant, LbFactoryConfig,
    PoolScoreInputs, V2FactoryConfig, V3FactoryConfig, V4PoolManagerConfig,
};
use engine_routes::{
    enumerate_k_hop_cycles_gated, enumerate_triangles_gated, enumerate_two_hop_routes_gated,
    CsrBuilder,
    RouteDispatch, RouteFastPath, RouteTable,
};
use engine_rpc::{
    aggregate3, fetch_algebra_global_state, fetch_pool_states_batched, fetch_token0, fetch_token1,
    fetch_v2_reserves, fetch_v3_liquidity, fetch_v3_slot0, fetch_v3_tick_window,
    fetch_v3_windows_batched, fetch_v4_liquidity, fetch_v4_slot0, fetch_v4_tick_window,
    ArbitrumRpcClient, BlockTag, Call3, PoolStateUpdate, PriceFetchKind, MULTICALL3_ARBITRUM,
};
use engine_state::{
    PoolKind, RiskClass, UniverseBuilder, V2Reserves256, V2State, V3State, VenueId,
};

use crate::pipeline::DetectionContext;

/// Opt-IN env flag (default OFF). Mirror of `pipeline::env_flag_default_on` but inverted:
/// the feature is disabled unless the operator explicitly sets `1`/`true`/`on`/`yes`. Used for
/// features that are correct but require an external validation step before they're safe to
/// run (e.g. Maverick — ABI fixed, quote math pending live-vector validation).
fn env_flag_opt_in(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("on") | Some("yes")
    )
}

/// Default-ON env flag (mirrors `pipeline::env_flag_default_on`, duplicated to avoid a public
/// re-export dependency in the startup path). Anything not in {0,false,off,no} stays on.
fn env_flag_default_on_local(name: &str) -> bool {
    match std::env::var(name)
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
    {
        Some(v) => !matches!(v.as_str(), "0" | "false" | "off" | "no"),
        None => true,
    }
}

pub(crate) const DEFAULT_ENGINE_TICK_WIDTH: i32 = 20;

pub(crate) fn engine_tick_width_from_env() -> i32 {
    std::env::var("ENGINE_TICK_WIDTH")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(DEFAULT_ENGINE_TICK_WIDTH)
}

/// How a survivor pool's expensive tick/bin window is fetched in Phase 2 (after the §5.5
/// liquidity floor culls near-dead pools). Phase 1 stores cheap state with an EMPTY window;
/// Phase 2 backfills only survivors. This routing is a pure function of the family so it can be
/// unit-tested (the phase-2 loop lives inside the live-RPC `build_detection_from_config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase2WindowRoute {
    /// V3-ABI spacing-window families (Uniswap/Sushi/Pancake V3, Algebra) — batched
    /// `ticks(int24)` via one aggregate3 multicall per chunk.
    BatchedV3,
    /// Uniswap V4 — per-pool StateView read keyed by poolId (no plain pool address).
    PerPoolV4,
    /// KyberSwap Elastic — per-pool walk of the on-chain initialized-tick LINKED LIST
    /// (`nearestCurrentTick` → `initializedTicks(t).prev/next`). [AUD-012] A spacing-window
    /// scan finds NOTHING when the liquidity sits far from the current tick (proven live:
    /// 130k-tick gap), so Kyber must never take the batched window.
    PerPoolKyber,
    /// No Phase-2 window needed: the family either has no tick window (V2/Solidly/DODO/Curve/
    /// Maverick/Fluid/Balancer) or fetched its full window already in Phase 1 (Trader Joe LB).
    None,
}

/// PB-PATHS compiled-route-table cache path (env `ROUTE_GRAPH_CACHE`, default
/// `route_graph_cache.bin` in the working dir).
fn route_graph_cache_path() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("ROUTE_GRAPH_CACHE").unwrap_or_else(|_| "route_graph_cache.bin".to_string()),
    )
}

/// PB-PATHS fingerprint of everything the enumerated route TABLE depends on: an enumeration
/// schema version, the topology config, the token count, and — critically, in `PoolId` order
/// — each pool's `(address, token0, token1, kind, fee)`. A match guarantees the identical
/// `PoolId`→pool mapping, so a cached route table (whose hops store dense `PoolId`s) is valid.
/// Any change to the universe / id assignment / topology / enumeration schema flips it and
/// forces a rebuild — a stale graph can never mis-route.
fn route_graph_fingerprint(
    pools: &engine_state::PoolTable,
    n_tokens: usize,
    topology: &engine_config::TopologyConfig,
    policy_record_hash: &[u8; 32],
    pool_eligibility: &[bool],
) -> u64 {
    // Bump when the enumeration LOGIC changes (rejects old artifacts even if pools are unchanged).
    // v2 (WI-02): policy-gated enumeration — the boot RuntimePolicy record hash and the
    // per-pool eligibility mask are part of the artifact identity (P1-09 family binding).
    const ENUMERATION_SCHEMA_VERSION: u64 = 2;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut fold = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    fold(ENUMERATION_SCHEMA_VERSION);
    for chunk in policy_record_hash.chunks(8) {
        let mut b = [0u8; 8];
        b[..chunk.len()].copy_from_slice(chunk);
        fold(u64::from_le_bytes(b));
    }
    for (i, e) in pool_eligibility.iter().enumerate() {
        if !*e {
            fold(0x5751_0200 ^ i as u64);
        }
    }
    fold(n_tokens as u64);
    fold(topology.enable_two_hop as u64);
    fold(topology.enable_triangle as u64);
    for k in 4u8..=8 {
        fold(topology.enabled(k) as u64);
    }
    fold(topology.max_routes_per_deep_tier as u64);
    for i in 0..pools.len() {
        let m = pools.meta(engine_state::PoolId(i as u32));
        fold(i as u64);
        for chunk in m.address.as_slice().chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            fold(u64::from_le_bytes(b));
        }
        fold(m.token0.0 as u64);
        fold(m.token1.0 as u64);
        fold(m.kind as u64);
        fold(m.fee_pips_or_bps as u64);
    }
    h
}

/// Decide a survivor's Phase-2 window route from its family. `is_v3_snapshot` is whether the
/// pool was stored as `SnapshotState::V3` in Phase 1 (Algebra/Kyber reuse the V3 state shape).
fn phase2_window_route(family: PoolKind, is_v3_snapshot: bool) -> Phase2WindowRoute {
    match family {
        PoolKind::UniswapV4 if is_v3_snapshot => Phase2WindowRoute::PerPoolV4,
        PoolKind::KyberSwapElastic if is_v3_snapshot => Phase2WindowRoute::PerPoolKyber,
        PoolKind::UniswapV3 | PoolKind::SushiV3 | PoolKind::PancakeV3 | PoolKind::AlgebraV19
            if is_v3_snapshot =>
        {
            Phase2WindowRoute::BatchedV3
        }
        // Everything else: no second-phase window (V2/Solidly reserves, DODO/Curve/Maverick/
        // Fluid/Balancer bespoke state, Trader Joe LB which already has its bins from Phase 1).
        _ => Phase2WindowRoute::None,
    }
}

fn dedupe_startup_discovered_pools(discovered: &mut Vec<DiscoveredPool>) -> usize {
    let before = discovered.len();
    let mut seen_balancer: AHashSet<[u8; 20]> = AHashSet::default();
    discovered.retain(|pool| {
        pool.family != PoolKind::BalancerStable || seen_balancer.insert(pool.address)
    });
    before - discovered.len()
}

/// Boot-time errors.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    /// Config malformed.
    #[error("config error: {0}")]
    Config(String),
    /// Address parse failure.
    #[error("invalid address: {0}")]
    Address(String),
    /// RPC fetch failed.
    #[error("rpc error: {0}")]
    Rpc(String),
}

/// Route collection-priority score: the **mean** per-pool heat across the route's hops (each
/// input is a Q32.32 pool score; the return is the integer mean). Hop-count-**neutral** by
/// construction — the earlier `sum` biased the CSR toward deeper routes (a 3-hop summed three pool
/// scores, a 2-hop only two), so the bounded per-dirty-event collector filled with triangular/deep
/// and STARVED 2-hop. But 2-hop is ~74% of landed Arbitrum atomic arbs (Dune query 7911104: 182/h
/// 2-hop vs 46/h triangular), so a 2-hop through two hot pools must not rank below a 4-hop merely
/// for having fewer hops. With the mean: two equally-hot routes of different length tie, and a
/// 2-hop through hot pools beats a deep route padded with cold pools.
/// WI-02b boundary 1 (cache path): filter a discovery-cache seed by the boot policy BEFORE
/// the pools enter validation/scoring — an inactive cached pool must never occupy a
/// `hot_pool_cap` slot and displace active pools (the boundary-3 registration gate runs only
/// AFTER truncation, too late to restore them). Order-preserving; returns the admitted pools
/// + excluded count. The same one-decision-gates-every-path shape the Fluid registrar uses.
pub fn policy_filtered_cache_seed(
    policy: &crate::runtime_policy::RuntimePolicy,
    cached: &[DiscoveredPool],
) -> (Vec<DiscoveredPool>, usize) {
    let admitted: Vec<DiscoveredPool> = cached
        .iter()
        .filter(|p| crate::runtime_policy::family_admitted_at_discovery(policy, p.family))
        .copied()
        .collect();
    let excluded = cached.len() - admitted.len();
    (admitted, excluded)
}

fn mean_route_heat(hop_pool_scores_q32: &[u64]) -> u32 {
    if hop_pool_scores_q32.is_empty() {
        return 0;
    }
    let mut s: u64 = 0;
    for &sc in hop_pool_scores_q32 {
        s = s.saturating_add(sc);
    }
    ((s / hop_pool_scores_q32.len() as u64) >> 32).min(u32::MAX as u64) as u32
}

/// Build a populated detection context from a config. If `rpc_endpoint` is `Some`, every
/// pool's state is snapshotted at `block_tag`; otherwise the engine boots with zeroed
/// state (useful for offline tests).
///
/// WI-02: `policy` is the ONE immutable boot-resolved `RuntimePolicy`. It is enforced here
/// at boundaries 1 (venue discovery), 3 (universe/state-store registration), 4 (route
/// graph, incl. the compiled-route-cache fingerprint) and at the price-anchor projection
/// (P1-08) — an inactive family is structurally absent from all of them.
pub fn build_detection_from_config(
    cfg: &EngineConfig,
    rpc_endpoint: Option<&str>,
    block_tag: BlockTag,
    policy: &crate::runtime_policy::RuntimePolicy,
) -> Result<(DetectionContext, engine_state::publication::StateWriterPermit)> {
    let mut ub = UniverseBuilder::new();

    // 1. Insert tokens.
    let mut token_id_by_addr: ahash::AHashMap<[u8; 20], engine_state::TokenId> =
        ahash::AHashMap::default();
    for t in &cfg.tokens {
        let addr =
            parse_address(&t.address).with_context(|| format!("token address {}", t.address))?;
        let risk = match t.risk_class.as_str() {
            "permissionless" => RiskClass::Permissionless,
            "centralised_stable" => RiskClass::CentralisedStable,
            "excluded" => RiskClass::Excluded,
            other => return Err(anyhow!("unknown risk class {other} for token {}", t.symbol)),
        };
        let id = ub.add_token(addr, t.decimals, risk);
        token_id_by_addr.insert(addr, id);
    }
    info!(tokens = cfg.tokens.len(), "tokens registered");

    // 2. RPC client (optional for offline tests; mandatory for live boot via the
    //    discovery scanner that runs in step 4).
    let rpc = rpc_endpoint.map(ArbitrumRpcClient::new);
    if let Some(client) = &rpc {
        match client.block_number() {
            Ok(bn) => info!(block_number = bn, "rpc reachable"),
            Err(e) => warn!("rpc reachability check failed: {e}"),
        }
        // AUD-CHAINID: verify the endpoint serves Arbitrum One (42161) before any pool discovery.
        // A chain-id mismatch is a hard config error — every hardcoded Arbitrum address would read
        // garbage — so we hard-fail the boot. A transient RPC error only warns (discovery would
        // surface it anyway); a *confirmed wrong chain* aborts.
        match client.verify_chain_id(engine_feed::constants::ARB_ONE_CHAIN_ID) {
            Ok(()) => info!("chain id verified = Arbitrum One (42161)"),
            Err(engine_rpc::RpcError::Decode(msg)) if msg.contains("endpoint serves chain id") => {
                return Err(anyhow::anyhow!("LIVE-SAFE refuse-to-boot: {msg}"));
            }
            Err(e) => warn!("chain-id check skipped (transient RPC error): {e}"),
        }
    }

    // §5.4 token decimal attestation: config `decimals` drive ALL amount math (liquidity-floor
    // TVL, profit valuation, q96 pricing). A stale/migrated `decimals` value silently computes
    // wrong amounts → financial loss. Verify each config token against the on-chain ERC-20
    // decimals() at boot. Live-submit fails closed on confirmed mismatch or unchecked tokens;
    // detection-only warns loudly and keeps running with the configured value.
    if let Some(client) = &rpc {
        let mut mismatches = 0usize;
        let mut checked = 0usize;
        for t in &cfg.tokens {
            let Ok(addr) = parse_address(&t.address) else {
                continue;
            };
            match engine_rpc::fetch_erc20_decimals(
                client,
                &addr,
                engine_rpc::client::BlockTag::Latest,
            ) {
                Ok(on_chain) => {
                    checked += 1;
                    if on_chain != t.decimals {
                        mismatches += 1;
                        warn!(
                            token = %t.symbol,
                            address = %t.address,
                            config_decimals = t.decimals,
                            on_chain_decimals = on_chain,
                            "§5.4 TOKEN DECIMAL MISMATCH — config decimals differ from on-chain; \
                             amount math will be wrong"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        token = %t.symbol,
                        address = %t.address,
                        error = %e,
                        "§5.4 TOKEN DECIMAL ATTESTATION UNCHECKED — decimals() could not be read; \
                         live-submit will fail closed"
                    );
                }
            }
        }
        enforce_decimal_attestation(cfg.live_submit, mismatches, checked, cfg.tokens.len())?;
    }

    // 4. §5.4 gate 1 (Discovered) + §5.6 codehash gate + §5.4 gate 2 (Validated)
    //    + §5.4 gate 3 (SnapshotReady) — discover pools dynamically from factory event
    //    logs, validate, snapshot.
    //
    //    Pool list is NEVER read from the config (plan §line 2116 "No static pool
    //    whitelist"). Every pool address comes from a `PoolCreated` / `PairCreated`
    //    event on a configured factory.
    let rpc_client = rpc.as_ref().ok_or_else(|| {
        anyhow!(
            "engine-discovery requires an RPC endpoint (set ARBITRUM_RPC_URL). \
             The plan §5.4 pipeline is fail-closed on RPC absence."
        )
    })?;
    let head_block = if let Some(h) = cfg.discovery.head_block_override {
        h
    } else {
        // Retry transient RPC failures (TLS handshake hiccups, brief clock skew,
        // throttling) with exponential backoff before failing fatal — a 1-second
        // network blip at boot must not kill the engine (observed: a cert
        // "not valid yet" by 1s from container clock skew).
        let mut attempt = 0u32;
        loop {
            match rpc_client.block_number() {
                Ok(bn) => break bn,
                Err(e) => {
                    attempt += 1;
                    if attempt >= 5 {
                        return Err(anyhow!(
                            "could not read head block for discovery after {attempt} attempts: {e}"
                        ));
                    }
                    let backoff_ms = 500u64 << (attempt - 1); // 0.5s, 1s, 2s, 4s
                    warn!(attempt, backoff_ms, error = %e, "head-block RPC failed; retrying");
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                }
            }
        }
    };
    info!(head_block, "head block resolved for discovery");

    // Build the token allowlist; only pools whose token0 AND token1 are both in this set
    // are emitted by the discovery scanner.
    let token_allowlist: AHashSet<[u8; 20]> = token_id_by_addr.keys().copied().collect();

    // Build the codehash allowlist from the venue list. Gate is enforced only when
    // `cfg.discovery.enforce_codehash` is true; otherwise the gate operates in research
    // mode and admits every pool (the user must promote to enforce_codehash=true before
    // live submit per §5.6).
    let artifact_dir =
        std::env::var("CODEHASH_RECORD_DIR").unwrap_or_else(|_| "research/dex".to_string());
    let codehash_allow = if cfg.discovery.enforce_codehash {
        // Base source: recorded artifacts under `research/dex/<family>/runtime.hash` (seeded by
        // a prior record-pass). The artifact allowlist is the bulk source — engine.json keeps
        // only any hand-pinned overrides.
        let (mut allow, families) =
            engine_discovery::load_allowlist_from_dir(std::path::Path::new(&artifact_dir));
        if families > 0 {
            info!(families, dir = %artifact_dir, "codehash artifacts loaded for enforce pass");
        }
        // Overlay inline `runtime_hash` per venue in engine.json (additive — both are accepted).
        for v in &cfg.venues {
            if let (Some(kind), Some(h)) =
                (family_to_kind(&v.family), parse_codehash(&v.runtime_hash))
            {
                allow.add(kind, h);
            }
        }
        // Observability (admit/reject behaviour unchanged): in enforce mode the codehash gate is
        // fail-OPEN for any family with no recorded hash (`accepts()` returns true for it). Warn
        // per live, configured family that is unseeded so the operator sees exactly which venues
        // lack bytecode identity and can seed them before trusting the gate.
        let live_families: Vec<PoolKind> = cfg
            .venues
            .iter()
            .filter_map(|v| family_to_kind(&v.family))
            .collect();
        for fam in allow.unseeded_families(&live_families) {
            warn!(
                family = ?fam,
                "enforce_codehash: live family has NO recorded codehash — gate is fail-open for it (admitted unchecked); seed research/dex/<family>/runtime.hash to enforce"
            );
        }
        allow
    } else {
        CodehashAllowlist::new()
    };
    // Record-pass: when NOT enforcing, accumulate observed runtime hashes so a single live
    // discovery run seeds `research/dex/<family>/runtime.hash`. Default ON in non-enforce mode
    // (zero cost beyond the bytecode fetch we already do for enforcing families); the operator
    // disables with `CODEHASH_RECORD=0`. Written out after the snapshot loop.
    let mut codehash_recorder: Option<engine_discovery::CodehashRecorder> =
        if !cfg.discovery.enforce_codehash && env_flag_default_on_local("CODEHASH_RECORD") {
            Some(engine_discovery::CodehashRecorder::new())
        } else {
            None
        };

    // V4 pools are keyed by a 32-byte poolId but registered under a synthetic 20-byte
    // address (poolId[0..20]). This side map recovers the full poolId at snapshot time
    // (StateView reads need it) and is also used to wire the log-updater addr_map.
    let mut v4_pool_id_by_addr: ahash::AHashMap<[u8; 20], [u8; 32]> = ahash::AHashMap::default();
    // WETH address for mapping V4's native-ETH sentinel (currency = address(0)).
    let v4_weth: [u8; 20] = cfg
        .tokens
        .iter()
        .find(|t| t.symbol.eq_ignore_ascii_case("WETH"))
        .and_then(|t| parse_address(&t.address).ok())
        .unwrap_or([0u8; 20]);

    // Run the per-venue scan and accumulate.
    let mut discovered: Vec<DiscoveredPool> = Vec::new();

    // Discovery cache: if a cache for this exact config exists, seed the discovered set
    // from it and scan only the blocks created since it was written. This turns a
    // multi-minute from-deployment-block rescan into a seconds-long incremental scan.
    let cache_path = crate::discovery_cache::DiscoveryCache::path();
    // WI-02c: the cache key is POLICY-AWARE — each ACTIVE_FAMILIES profile owns its cache
    // lineage (load AND the later save both use this key), so a narrow boot's filtered,
    // head-advanced cache can never serve or overwrite a broader boot's coverage.
    let cfg_hash = crate::discovery_cache::policy_aware_config_hash(
        &cfg.venues,
        &token_allowlist,
        &policy.active_families.tokens(),
    );
    let cache = cache_path
        .as_ref()
        .and_then(|p| crate::discovery_cache::DiscoveryCache::load(p, cfg_hash));
    if let Some(c) = &cache {
        // WI-02b boundary 1 (cache path): the cache seed is policy-gated HERE, before
        // validation/scoring — the cache key knows nothing about ACTIVE_FAMILIES, so an
        // unfiltered seed would let inactive cached pools occupy hot_pool_cap slots and
        // displace active pools (the boundary-3 registration gate runs after truncation).
        let (admitted, policy_excluded) = policy_filtered_cache_seed(policy, &c.pools);
        info!(
            cached_pools = c.pools.len(),
            admitted = admitted.len(),
            policy_excluded,
            cached_head = c.head_block,
            new_blocks = head_block.saturating_sub(c.head_block),
            "discovery cache hit — seeding policy-admitted cached pools, scanning only new blocks"
        );
        discovered.extend(admitted);
        for (addr, pid) in &c.v4_pool_ids {
            v4_pool_id_by_addr.insert(*addr, *pid);
        }
    } else if cache_path.is_some() {
        info!("discovery cache miss/stale — full scan from venue deployment blocks");
    }

    let mut venue_id_for_family: ahash::AHashMap<String, u16> = ahash::AHashMap::default();
    for (i, v) in cfg.venues.iter().enumerate() {
        venue_id_for_family.insert(v.family.clone(), i as u16);
        let Some(kind) = family_to_kind(&v.family) else {
            warn!(family = %v.family, "unknown venue family; skipping discovery");
            continue;
        };
        // WI-02 boundary 1 (config/discovery): an inactive family never scans its venue.
        if !crate::runtime_policy::family_admitted_at_discovery(policy, kind) {
            info!(family = %v.family, "family INACTIVE under the boot RuntimePolicy; discovery skipped");
            continue;
        }
        let factory_addr =
            parse_address(&v.factory).with_context(|| format!("venue factory {}", v.factory))?;
        let chunk = cfg.discovery.block_range_chunk;
        // With a valid cache, resume each venue's scan just past the cached head; otherwise
        // scan from its deployment block. If no new blocks exist the range is empty (fast).
        let scan_start = match &cache {
            Some(c) => (c.head_block + 1).max(v.deployment_block),
            None => v.deployment_block,
        };
        let venue_pools = match kind {
            PoolKind::UniswapV3 | PoolKind::SushiV3 | PoolKind::PancakeV3 => {
                let fc = V3FactoryConfig {
                    address: factory_addr,
                    family: kind,
                    deployment_block: scan_start,
                };
                discover_v3_pools(rpc_client, &fc, head_block, chunk, &token_allowlist)
                    .with_context(|| format!("discover v3 from {}", v.factory))?
            }
            PoolKind::UniswapV4 => {
                // Uniswap V4 singleton PoolManager. Discover hookless pools from the
                // Initialize logs; each is registered under a synthetic address
                // (poolId[0..20]) with the same V3 kernel / V3State store. The full
                // 32-byte poolId is stashed in `v4_pool_id_by_addr` so the StateView
                // snapshot can address the pool.
                let mc = V4PoolManagerConfig {
                    address: factory_addr,
                    deployment_block: scan_start,
                    weth: v4_weth,
                };
                let v4 = discover_v4_pools(rpc_client, &mc, head_block, chunk, &token_allowlist)
                    .with_context(|| format!("discover v4 from {}", v.factory))?;
                let mut out = Vec::with_capacity(v4.len());
                for p in v4 {
                    v4_pool_id_by_addr.insert(p.address, p.pool_id);
                    out.push(DiscoveredPool {
                        address: p.address,
                        family: PoolKind::UniswapV4,
                        token0: p.token0,
                        token1: p.token1,
                        fee_pips: p.fee_pips,
                        fee_bps: 0,
                        tick_spacing: p.tick_spacing,
                        created_block: p.created_block,
                    });
                }
                out
            }
            PoolKind::TraderJoeLb => {
                // Trader Joe Liquidity Book v2.1/v2.2 emits `LBPairCreated` from the
                // LBFactory. The scanner stashes the binStep in `fee_pips`; the real
                // (base + variable) fee is resolved from on-chain state at snapshot
                // time and the registered bps is derived there.
                let fc = LbFactoryConfig {
                    address: factory_addr,
                    deployment_block: scan_start,
                };
                discover_lb_pools(rpc_client, &fc, head_block, chunk, &token_allowlist)
                    .with_context(|| format!("discover trader joe lb from {}", v.factory))?
            }
            PoolKind::AlgebraV19 => {
                // Algebra V1.9 (Camelot V3) emits `Pool(address,address,address)` instead
                // of Uniswap V3's `PoolCreated(address,address,uint24,int24,address)`.
                // Different topic hash and different data layout — separate scanner.
                let fc = AlgebraFactoryConfig {
                    address: factory_addr,
                    family: kind,
                    deployment_block: scan_start,
                };
                discover_algebra_pools(rpc_client, &fc, head_block, chunk, &token_allowlist)
                    .with_context(|| format!("discover algebra from {}", v.factory))?
            }
            PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => {
                let fc = V2FactoryConfig {
                    address: factory_addr,
                    family: kind,
                    deployment_block: scan_start,
                    default_fee_bps: v.default_fee_bps.max(30),
                };
                discover_v2_pools(rpc_client, &fc, head_block, chunk, &token_allowlist)
                    .with_context(|| format!("discover v2 from {}", v.factory))?
            }
            PoolKind::KyberSwapElastic => {
                // KyberSwap Elastic uses Uniswap V3's `PoolCreated(address,address,uint24,int24,address)`
                // event signature — same topic-0 — so reuse discover_v3_pools with the
                // KyberSwap family tag so downstream dispatch picks the right kernel.
                let fc = V3FactoryConfig {
                    address: factory_addr,
                    family: kind,
                    deployment_block: scan_start,
                };
                discover_v3_pools(rpc_client, &fc, head_block, chunk, &token_allowlist)
                    .with_context(|| format!("discover kyber from {}", v.factory))?
            }
            PoolKind::DodoV2 => {
                // DODO V2 has 3 factories (DVM/DPP/DSP); the venue family tag is "dodo_*"
                // and the specific event topic comes from the family suffix. Operator
                // config lists all three sibling factories under their dodo_dvm/dpp/dsp
                // labels so we know which topic to filter on.
                let topic_opt: Option<[u8; 32]> = match v.family.as_str() {
                    "dodo_dvm" => Some(*engine_discovery::new_dvm_topic()),
                    "dodo_dpp" => Some(*engine_discovery::new_dpp_topic()),
                    "dodo_dsp" => Some(*engine_discovery::new_dsp_topic()),
                    _ => None,
                };
                match topic_opt {
                    Some(event_topic) => {
                        let fc = engine_discovery::DodoFactoryConfig {
                            address: factory_addr,
                            deployment_block: scan_start,
                            event_topic,
                        };
                        engine_discovery::discover_dodo_pools(
                            rpc_client,
                            &fc,
                            head_block,
                            chunk,
                            &token_allowlist,
                        )
                        .with_context(|| format!("discover dodo from {}", v.factory))?
                    }
                    None => {
                        warn!(family = %v.family, "unknown DODO sub-family; expected dodo_dvm/dpp/dsp");
                        Vec::new()
                    }
                }
            }
            PoolKind::MaverickV2 => {
                // The lens ABI bug is FIXED (2026-05-28): `maverick_lens.rs` now calls the
                // real `getTicksAroundActive(pool,radius)` + `getTickSqrtPriceAndL(pool,tick)`
                // + `getState()` ABIs (all selector-locked + fixture-tested). `getState()`
                // decoding fixes the original "activeTick = 2.06e9 garbage" — that was the
                // bogus-selector return being mis-decoded.
                //
                // Discovery is gated behind `MAVERICK_ENABLE=1` AND the in-code live-vector
                // readiness gate. The ABI is now correct, but the per-tick quote math
                // (sqrt-bound tiling + liquidity) has not been validated against ≥3 live
                // known-quote vectors — and we never ship unvalidated quote math that could
                // mis-size a real trade. The operator flag alone cannot activate Maverick while
                // `live_reference_vectors_ready()` is false.
                let requested = env_flag_opt_in("MAVERICK_ENABLE");
                let parity_ready = engine_amm::maverick_v2::live_reference_vectors_ready();
                if requested && parity_ready {
                    let fc = engine_discovery::MaverickFactoryConfig {
                        address: factory_addr,
                    };
                    match engine_discovery::discover_maverick_pools(
                        rpc_client,
                        &fc,
                        head_block,
                        &token_allowlist,
                    ) {
                        Ok(pools) => {
                            info!(family = %v.family, count = pools.len(), "Maverick discovery enabled after live-vector readiness gate");
                            pools
                        }
                        Err(e) => {
                            warn!(family = %v.family, ?e, "Maverick discovery failed; skipping");
                            Vec::new()
                        }
                    }
                } else {
                    info!(family = %v.family, requested, parity_ready, "Maverick discovery OFF; requires MAVERICK_ENABLE=1 and reviewed live quote vectors");
                    Vec::new()
                }
            }
            PoolKind::SolidlyV3 => {
                let fc = engine_discovery::SolidlyFactoryConfig {
                    address: factory_addr,
                    deployment_block: scan_start,
                    default_volatile_fee_bps: v.default_fee_bps.max(20),
                    default_stable_fee_bps: 5,
                };
                engine_discovery::discover_solidly_pools(
                    rpc_client,
                    &fc,
                    head_block,
                    chunk,
                    &token_allowlist,
                )
                .with_context(|| format!("discover solidly from {}", v.factory))?
            }
            PoolKind::CurveStableSwapNg if cache.is_some() => {
                // Curve enumerates a registry rather than scanning a block range, so there's
                // no incremental scan. With a valid cache its pools are already seeded; skip
                // the (RPC-heavy) re-enumeration. Delete the cache to pick up new Curve pools.
                Vec::new()
            }
            PoolKind::CurveStableSwapNg => {
                // Curve has no shared create event; enumerate the StableSwap-NG factory's
                // pool_count()/pool_list(i) and keep PLAIN pools whose coins are ALL
                // allowlisted (2..=3 for v1). Discovery emits one pair per coin pair; we
                // dedup to one primary pair per pool address (the snapshot/state is shared
                // and registering once per pool keeps the Curve resync refresh clean).
                let cc = engine_discovery::CurveFactoryConfig {
                    address: factory_addr,
                    max_coins: 3,
                    family: PoolKind::CurveStableSwapNg,
                };
                let mut pairs =
                    engine_discovery::discover_curve_pools(rpc_client, &cc, &token_allowlist)
                        .with_context(|| format!("discover curve from {}", v.factory))?;
                let mut seen_addr: AHashSet<[u8; 20]> = AHashSet::default();
                pairs.retain(|p| seen_addr.insert(p.address));
                pairs
            }
            PoolKind::CurveCryptoSwap => {
                // Curve CryptoSwap-NG (Tricrypto-NG, 3-coin) — same factory enumeration surface as
                // StableSwap-NG (pool_count/pool_list/coins), tagged CurveCryptoSwap so the snapshot
                // dispatches to fetch_cryptoswap_state + the crypto kernel. Activates only when a
                // `curve_cryptoswap` venue (with the Tricrypto-NG factory address) is in the config;
                // validate quote parity vs on-chain get_dy before live submit (see DEX_AMM_COVERAGE.md).
                let cc = engine_discovery::CurveFactoryConfig {
                    address: factory_addr,
                    max_coins: 3,
                    family: PoolKind::CurveCryptoSwap,
                };
                let mut pairs =
                    engine_discovery::discover_curve_pools(rpc_client, &cc, &token_allowlist)
                        .with_context(|| format!("discover curve-crypto from {}", v.factory))?;
                let mut seen_addr: AHashSet<[u8; 20]> = AHashSet::default();
                pairs.retain(|p| seen_addr.insert(p.address));
                pairs
            }
            PoolKind::BalancerStable => {
                // `factory` for a Balancer venue is the singleton Vault address.
                let bc = engine_discovery::BalancerVaultConfig {
                    vault: factory_addr,
                    deployment_block: scan_start,
                };
                let mut pairs = engine_discovery::discover_balancer_pools(
                    rpc_client,
                    &bc,
                    head_block,
                    chunk,
                    &token_allowlist,
                )
                .with_context(|| format!("discover balancer from {}", v.factory))?;
                // One primary pair per pool address (MVP): the snapshot/state is shared,
                // and registering once per pool keeps the resync refresh clean.
                let mut seen_addr: AHashSet<[u8; 20]> = AHashSet::default();
                pairs.retain(|p| seen_addr.insert(p.address));
                pairs
            }
            _ => {
                warn!(family = %v.family, "family not supported by discovery yet; skipping");
                Vec::new()
            }
        };
        info!(family = %v.family, venue_idx = i, count = venue_pools.len(), "venue discovery done");
        discovered.extend(venue_pools);
    }
    let duplicate_discovered = dedupe_startup_discovered_pools(&mut discovered);
    if duplicate_discovered > 0 {
        warn!(
            removed = duplicate_discovered,
            remaining = discovered.len(),
            "startup discovery sanitizer removed duplicate Balancer pool rows"
        );
    }
    info!(
        total_discovered = discovered.len(),
        "§5.4 gate 1 (Discovered) complete"
    );
    // Dashboard: discovery is done (incl. the fast cache-hit path) — fill its bar now so it doesn't
    // sit at 0% through the multi-minute snapshot phase, and seed snapshot's total.
    if let Some(p) = crate::dashboard::progress() {
        p.set(
            crate::dashboard::phase::DISCOVERY,
            discovered.len() as u64,
            discovered.len() as u64,
        );
        p.complete(crate::dashboard::phase::DISCOVERY);
        p.set(
            crate::dashboard::phase::SNAPSHOT,
            0,
            discovered.len() as u64,
        );
        p.set_status(format!(
            "discovered {} pools — snapshotting state…",
            discovered.len()
        ));
    }

    // Persist the gate-1 discovered set so the next boot scans only new blocks. We cache the
    // RAW discovered set (pre-validation/snapshot/scoring) since reboot re-runs those gates.
    // Fluid pools are resolver-enumerated separately (below) and folded into this cache via a
    // second save once they're known, so offline/cold-cache boots (no RPC) can still seed them.
    // We keep the constructed cache alive (`discovery_cache_to_resave`) for that second save.
    let mut discovery_cache_to_resave: Option<crate::discovery_cache::DiscoveryCache> = None;
    if let Some(p) = &cache_path {
        let v4_ids: Vec<([u8; 20], [u8; 32])> =
            v4_pool_id_by_addr.iter().map(|(a, b)| (*a, *b)).collect();
        let mut to_save = crate::discovery_cache::DiscoveryCache::new(
            cfg_hash,
            head_block,
            discovered.clone(),
            v4_ids,
        );
        // Carry forward any Fluid pools from the prior cache so a boot that can't reach the
        // resolver (no RPC this run) doesn't erase them from the on-disk cache.
        if let Some(c) = &cache {
            to_save.fluid_pools = c.fluid_pools.clone();
        }
        match to_save.save(p) {
            Ok(()) => info!(
                saved_pools = to_save.pools.len(),
                head_block,
                path = %p.display(),
                "discovery cache written"
            ),
            Err(e) => warn!(error = %e, "failed to write discovery cache (non-fatal)"),
        }
        discovery_cache_to_resave = Some(to_save);
    }

    // §5.4 gate 2 (Validated) — codehash gate, and §5.4 gate 3 (SnapshotReady) — fetch
    // V2/V3 state. Pools that pass become candidates for the initial hot-tier score.
    struct PoolWithSnapshot {
        venue: VenueId,
        d: DiscoveredPool,
        snapshot: SnapshotState,
    }
    enum SnapshotState {
        V2(V2State),
        V3(V3State),
        Dodo(engine_amm::dodo_v2::DodoState),
        Maverick(engine_amm::maverick_v2::MaverickState),
        BalancerStable(engine_state::BalancerStableSlot),
        BalancerWeighted(engine_state::BalancerWeightedSlot),
        Curve(engine_state::CurveSlot),
        CurveCrypto(engine_state::CryptoCurveSlot),
        Lb(engine_amm::trader_joe_lb::LBState),
    }
    let mut validated: Vec<PoolWithSnapshot> = Vec::new();
    let mut codehash_fail = 0usize;
    let mut snapshot_fail = 0usize;
    let total_discovered = discovered.len();
    let mut processed = 0usize;
    let mut last_log_ts = std::time::Instant::now();

    // ── Phase 1 PRE-PASS: batch the cheap per-pool reads via Multicall3 ────────────
    //
    // The per-pool loop below previously issued, for every one of the ~3,000 discovered
    // pools, 2 RPC for the token-pair gate (token0()+token1()) + 1–2 RPC for the cheap
    // state (V2 getReserves / V3 slot0+liquidity) — sequentially, rate-limited by the
    // provider to ~25 RPS. That alone was ~10 minutes before the §5.5 floor even ran.
    //
    // Here we collapse those reads into a handful of `aggregate3` multicalls (one RPC per
    // ~150-pool chunk, one block — so the whole batch is mutually consistent):
    //   * `tokenpair_by_addr`  — batched token0()/token1() for the gate families.
    //   * `state_by_addr`      — batched V2 reserves / Uniswap-V3 slot0+liquidity.
    // The loop then reads these maps and only falls back to a per-pool RPC on a batch
    // MISS (transient multicall hiccup), so the validation/gate logic is byte-for-byte
    // the same as before — only the I/O is batched. Tick windows are NOT fetched here;
    // they remain deferred to Phase 2 (survivors only).
    //
    // Families with bespoke read ABIs (V4 by poolId, Kyber getPoolState, DODO, Maverick,
    // Balancer, LB) are NOT batched here (they keep their existing per-pool fetch); they
    // are a small minority of the universe.
    const TOKENPAIR_SELECTOR0: [u8; 4] = [0x0d, 0xfe, 0x16, 0x81]; // token0()
    const TOKENPAIR_SELECTOR1: [u8; 4] = [0xd2, 0x12, 0x20, 0xa7]; // token1()
    const PHASE1_CHUNK: usize = 150;
    let runs_token_pair_gate = |fam: PoolKind| {
        matches!(
            fam,
            PoolKind::UniswapV2
                | PoolKind::SushiV2
                | PoolKind::CamelotV2
                | PoolKind::UniswapV3
                | PoolKind::SushiV3
                | PoolKind::PancakeV3
                | PoolKind::AlgebraV19
                | PoolKind::KyberSwapElastic
                | PoolKind::SolidlyV3
        )
    };
    // Map a family to the batched price-fetch kind, or None if it has a bespoke ABI
    // that `fetch_pool_states_batched` can't read, OR needs a field the batch omits.
    //   * V4/Kyber/DODO/Maverick/Balancer/LB — bespoke read ABIs.
    //   * Algebra V1.9 — the batched decode returns price/tick/liquidity but NOT the
    //     dynamic `fee_pips` (which lives in globalState and is required for both the
    //     V3State fee and registration), so Algebra keeps its per-pool globalState read.
    //     There are only ~37 of them, so the sequential cost is negligible.
    let batched_price_kind = |fam: PoolKind| -> Option<PriceFetchKind> {
        match fam {
            PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 | PoolKind::SolidlyV3 => {
                Some(PriceFetchKind::V2)
            }
            PoolKind::UniswapV3 | PoolKind::SushiV3 | PoolKind::PancakeV3 => {
                Some(PriceFetchKind::UniswapV3)
            }
            _ => None,
        }
    };

    let mut tokenpair_by_addr: ahash::AHashMap<[u8; 20], ([u8; 20], [u8; 20])> =
        ahash::AHashMap::default();
    let mut state_by_addr: ahash::AHashMap<[u8; 20], PoolStateUpdate> = ahash::AHashMap::default();
    // Bespoke-family batched cheap-state maps (DODO / Balancer / V4). Same pattern as the
    // V2/V3 `state_by_addr` map: the validation loop reads these first and only falls back
    // to a per-pool RPC on a MISS, so registration / snapshot_fail accounting is identical
    // — only the transport is batched (one aggregate3 per chunk vs ~1–4 RPC per pool).
    let mut dodo_by_addr: ahash::AHashMap<[u8; 20], engine_amm::dodo_v2::DodoState> =
        ahash::AHashMap::default();
    let mut balancer_by_addr: ahash::AHashMap<[u8; 20], engine_rpc::BalancerStableView> =
        ahash::AHashMap::default();
    // Balancer addresses whose batch chunk resolved (aggregate3 Ok). For these, a MISS in
    // `balancer_by_addr` is a DEFINITIVE non-stable skip (the batch ran getAmp and it
    // reverted) — identical outcome to the per-pool path, so we skip WITHOUT re-issuing
    // the per-pool reads. Only addresses NOT in this set (a transient chunk Err) fall back
    // per-pool. This is what turns ~660 sequential non-stable reverts into one multicall.
    let mut balancer_batched: AHashSet<[u8; 20]> = AHashSet::default();
    let mut v4_state_by_addr: ahash::AHashMap<[u8; 20], engine_rpc::V4CheapState> =
        ahash::AHashMap::default();

    // (a) Batch token0()/token1() for gate families.
    {
        let gate_addrs: Vec<[u8; 20]> = discovered
            .iter()
            .filter(|d| runs_token_pair_gate(d.family))
            .map(|d| d.address)
            .collect();
        for chunk in gate_addrs.chunks(PHASE1_CHUNK) {
            let mut calls: Vec<Call3> = Vec::with_capacity(chunk.len() * 2);
            for addr in chunk {
                calls.push(Call3 {
                    target: *addr,
                    allow_failure: true,
                    call_data: TOKENPAIR_SELECTOR0.to_vec(),
                });
                calls.push(Call3 {
                    target: *addr,
                    allow_failure: true,
                    call_data: TOKENPAIR_SELECTOR1.to_vec(),
                });
            }
            match aggregate3(rpc_client, &MULTICALL3_ARBITRUM, &calls, block_tag) {
                Ok(results) => {
                    for (i, addr) in chunk.iter().enumerate() {
                        let r0 = results.get(i * 2);
                        let r1 = results.get(i * 2 + 1);
                        if let (Some(r0), Some(r1)) = (r0, r1) {
                            if r0.success
                                && r0.return_data.len() >= 32
                                && r1.success
                                && r1.return_data.len() >= 32
                            {
                                let mut t0 = [0u8; 20];
                                t0.copy_from_slice(&r0.return_data[12..32]);
                                let mut t1 = [0u8; 20];
                                t1.copy_from_slice(&r1.return_data[12..32]);
                                tokenpair_by_addr.insert(*addr, (t0, t1));
                            }
                        }
                    }
                }
                Err(e) => {
                    // Fail-open: leave this chunk out of the map; the loop falls back to
                    // per-pool token0()/token1().
                    warn!(
                        ?e,
                        "phase-1 batched token-pair fetch failed for chunk; falling back per-pool"
                    );
                }
            }
        }
    }

    // (b) Batch V2 reserves / V3 (Uniswap + Algebra) slot0+liquidity.
    {
        let state_inputs: Vec<([u8; 20], PriceFetchKind)> = discovered
            .iter()
            .filter_map(|d| batched_price_kind(d.family).map(|k| (d.address, k)))
            .collect();
        for chunk in state_inputs.chunks(PHASE1_CHUNK) {
            match fetch_pool_states_batched(rpc_client, &MULTICALL3_ARBITRUM, chunk, block_tag) {
                Ok(states) => {
                    for ((addr, _kind), st) in chunk.iter().zip(states.into_iter()) {
                        if let Some(upd) = st {
                            state_by_addr.insert(*addr, upd);
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        ?e,
                        "phase-1 batched state fetch failed for chunk; falling back per-pool"
                    );
                }
            }
        }
    }

    // (c) Batch V4 cheap state (StateView getSlot0 + getLiquidity by poolId). 2 calls per
    //     pool → ~75 pools/chunk keeps each aggregate3 ≤150 calls.
    {
        let head = match block_tag {
            BlockTag::Number(n) => n,
            _ => 0,
        };
        let _ = head; // V4 cheap state carries no block_number field; head unused here.
        if let Ok(state_view) = parse_address(&cfg.discovery.v4_state_view) {
            // (addr, poolId) for every discovered V4 pool that has a poolId side-map entry.
            let v4_inputs: Vec<([u8; 20], [u8; 32])> = discovered
                .iter()
                .filter(|d| d.family == PoolKind::UniswapV4)
                .filter_map(|d| {
                    v4_pool_id_by_addr
                        .get(&d.address)
                        .map(|pid| (d.address, *pid))
                })
                .collect();
            const V4_CHUNK: usize = 75; // 2 calls/pool
            for chunk in v4_inputs.chunks(V4_CHUNK) {
                let pool_ids: Vec<[u8; 32]> = chunk.iter().map(|(_, pid)| *pid).collect();
                match engine_rpc::fetch_v4_states_batched(
                    rpc_client,
                    &state_view,
                    &pool_ids,
                    block_tag,
                ) {
                    Ok(states) => {
                        for ((addr, _pid), st) in chunk.iter().zip(states.into_iter()) {
                            if let Some(cheap) = st {
                                v4_state_by_addr.insert(*addr, cheap);
                            }
                        }
                    }
                    Err(e) => {
                        warn!(?e, "phase-1 batched V4 state fetch failed for chunk; falling back per-pool");
                    }
                }
            }
        }
    }

    // (d) Batch DODO cheap state. 5 calls per pool (base/quote/pmm/fee/version) → ~29
    //     pools/chunk. The snapshot reads each pool's on-chain `version()` and sets the true
    //     class (DVM/DPP/DSP); the value passed here is only the fallback when `version()`
    //     reverts. (DPP/DSP pools were historically mislabelled `Dvm` — see DETECTOR_PROFIT_VERDICT.)
    {
        let head = match block_tag {
            BlockTag::Number(n) => n,
            _ => 0,
        };
        let dodo_addrs: Vec<[u8; 20]> = discovered
            .iter()
            .filter(|d| d.family == PoolKind::DodoV2)
            .map(|d| d.address)
            .collect();
        const DODO_CHUNK: usize = 29; // 5 calls/pool
        for chunk in dodo_addrs.chunks(DODO_CHUNK) {
            let pools: Vec<([u8; 20], engine_amm::dodo_v2::DodoPoolClass)> = chunk
                .iter()
                .map(|a| (*a, engine_amm::dodo_v2::DodoPoolClass::Dvm))
                .collect();
            match engine_rpc::fetch_dodo_states_batched(
                rpc_client,
                &MULTICALL3_ARBITRUM,
                &pools,
                block_tag,
                head,
            ) {
                Ok(states) => {
                    for (addr, st) in chunk.iter().zip(states.into_iter()) {
                        if let Some(s) = st {
                            dodo_by_addr.insert(*addr, s);
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        ?e,
                        "phase-1 batched DODO state fetch failed for chunk; falling back per-pool"
                    );
                }
            }
        }
    }

    // (e) Batch Balancer cheap state via the Vault. 5 pool reads + 1 poolId read per pool
    //     across 2 aggregate3 round-trips per chunk; ~30 pools/chunk. Non-stable pools
    //     (weighted etc.) revert getAmplificationParameter inside aggregate3 → None for
    //     that pool (skipped exactly as the per-pool path skips them).
    {
        let balancer_addrs: Vec<[u8; 20]> = discovered
            .iter()
            .filter(|d| d.family == PoolKind::BalancerStable)
            .map(|d| d.address)
            .collect();
        const BALANCER_CHUNK: usize = 30; // 5 calls/pool in round 2
        for chunk in balancer_addrs.chunks(BALANCER_CHUNK) {
            match engine_rpc::fetch_balancer_states_batched(
                rpc_client,
                &MULTICALL3_ARBITRUM,
                &engine_rpc::BALANCER_VAULT_ARBITRUM,
                chunk,
                block_tag,
            ) {
                Ok(states) => {
                    for (addr, st) in chunk.iter().zip(states.into_iter()) {
                        // The chunk's aggregate3 resolved → record every pool as batched
                        // (a None here means non-stable / getAmp reverted = definitive skip).
                        balancer_batched.insert(*addr);
                        if let Some(v) = st {
                            balancer_by_addr.insert(*addr, v);
                        }
                    }
                }
                Err(e) => {
                    warn!(?e, "phase-1 batched Balancer state fetch failed for chunk; falling back per-pool");
                }
            }
        }
    }

    info!(
        gate_pairs = tokenpair_by_addr.len(),
        states = state_by_addr.len(),
        v4_states = v4_state_by_addr.len(),
        dodo_states = dodo_by_addr.len(),
        balancer_states = balancer_by_addr.len(),
        total = total_discovered,
        "§5.4 gate 3 phase-1 pre-pass complete (batched token-pair + cheap state via multicall)"
    );

    'snap: for d in discovered {
        processed += 1;
        // Dashboard: drive the snapshot bar off THIS loop (the dominant ~minutes-long boot phase),
        // so it animates instead of sitting at 0% until the later tick backfill. Boot-only, off
        // the hot path (relaxed atomics).
        if let Some(p) = crate::dashboard::progress() {
            p.set(
                crate::dashboard::phase::SNAPSHOT,
                processed as u64,
                total_discovered as u64,
            );
        }
        if last_log_ts.elapsed() >= std::time::Duration::from_secs(15) {
            info!(
                processed,
                total = total_discovered,
                validated = validated.len(),
                codehash_fail,
                snapshot_fail,
                family = ?d.family,
                "snapshot phase progress"
            );
            if let Some(p) = crate::dashboard::progress() {
                p.set_status(format!(
                    "snapshot {:?} · {processed}/{total_discovered} · {} validated · {snapshot_fail} skipped",
                    d.family,
                    validated.len(),
                ));
            }
            last_log_ts = std::time::Instant::now();
        }
        // Record-pass: in non-enforce mode, fetch the pool's runtime bytecode hash once and
        // accumulate it so this discovery run seeds `research/dex/<family>/runtime.hash`. The
        // fetch is the same `get_code` the enforce path does — recording is otherwise free.
        if let Some(rec) = codehash_recorder.as_mut() {
            if let Ok(code) = rpc_client.get_code(&d.address, BlockTag::Latest) {
                if !code.is_empty() {
                    use tiny_keccak::Hasher;
                    let mut k = tiny_keccak::Keccak::v256();
                    k.update(&code);
                    let mut hash = [0u8; 32];
                    k.finalize(&mut hash);
                    rec.record(d.family, hash);
                }
            }
        }
        // Codehash gate — skip-mode if `enforce_codehash=false`.
        if codehash_allow.enforces(d.family) {
            match engine_discovery::validate_pool_codehash(
                rpc_client,
                &d.address,
                d.family,
                &codehash_allow,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    codehash_fail += 1;
                    continue;
                }
                Err(e) => {
                    warn!(pool = ?d.address, ?e, "codehash gate RPC error; skipping");
                    codehash_fail += 1;
                    continue;
                }
            }
        }
        // Token-pair gate: discovered events already include the indexed tokens, but
        // belt-and-braces: re-read token0()/token1() and refuse to admit on mismatch.
        // Only applies to V2/V3 families that expose those selectors — DODO uses
        // `_BASE_TOKEN_/_QUOTE_TOKEN_`, Maverick uses `tokenA/tokenB`, Solidly stable
        // reuses V2 selectors. For families where the selector doesn't exist, trust
        // the discovery record.
        //
        // The token0()/token1() reads were batched in the Phase-1 pre-pass; consult that
        // map first and only fall back to a per-pool RPC on a batch miss (so the gate's
        // accept/reject decision is identical to the per-pool version, just cheaper).
        if !runs_token_pair_gate(d.family) {
            // Skip — discovery provided the token pair authoritatively.
        } else {
            let pair = match tokenpair_by_addr.get(&d.address).copied() {
                Some(p) => Ok(p),
                None => match (
                    fetch_token0(rpc_client, &d.address, block_tag),
                    fetch_token1(rpc_client, &d.address, block_tag),
                ) {
                    (Ok(t0), Ok(t1)) => Ok((t0, t1)),
                    (Err(e), _) | (_, Err(e)) => Err(e),
                },
            };
            match pair {
                Ok((t0, t1)) => {
                    let canonical_match =
                        (t0 == d.token0 && t1 == d.token1) || (t0 == d.token1 && t1 == d.token0);
                    if !canonical_match {
                        warn!(pool = ?d.address, "token-pair mismatch vs discovery; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                }
                Err(e) => {
                    warn!(pool = ?d.address, ?e, "token-pair fetch failed; skipping");
                    snapshot_fail += 1;
                    continue;
                }
            }
        }
        // Snapshot.
        let snapshot = match d.family {
            PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => {
                // Camelot V2 pairs can be solidly-style STABLE pools (`stableSwap()==true`), whose
                // invariant is x³y+xy³=k, NOT constant-product. Pricing a stable pair with the V2
                // constant-product kernel is a real misquote (over-quotes), so skip it (fail-closed)
                // rather than register it wrong. Fail-safe: a `None` (plain Uni/Sushi pair, or RPC
                // error) is treated as volatile → unchanged behaviour. (AMM review: Camelot V2.)
                if matches!(d.family, PoolKind::CamelotV2)
                    && engine_rpc::fetch_camelot_is_stable(rpc_client, &d.address, block_tag)
                        == Some(true)
                {
                    info!(pool = ?d.address, "skipping Camelot V2 STABLE pair (constant-product kernel would misprice it)");
                    snapshot_fail += 1;
                    continue;
                }
                // Phase-1 batched state first; fall back to a per-pool getReserves on miss.
                let reserves = match state_by_addr.get(&d.address) {
                    Some(PoolStateUpdate::V2 {
                        reserve0,
                        reserve1,
                        block_timestamp_last,
                    }) => Ok((*reserve0, *reserve1, *block_timestamp_last)),
                    _ => match fetch_v2_reserves(rpc_client, &d.address, block_tag) {
                        Ok(s) => Ok((s.reserve0, s.reserve1, s.block_timestamp_last)),
                        Err(e) => Err(e),
                    },
                };
                match reserves {
                    Ok((reserve0, reserve1, block_timestamp_last)) => SnapshotState::V2(V2State {
                        reserves: V2Reserves256 {
                            reserve0,
                            reserve1,
                            block_timestamp_last,
                        },
                    }),
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "v2 snapshot failed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                }
            }
            PoolKind::UniswapV3 | PoolKind::SushiV3 | PoolKind::PancakeV3 => {
                // Phase-1 batched slot0+liquidity first; fall back per-pool on miss.
                let (sqrt_price_x96, tick, liquidity): (U256, i32, u128) = match state_by_addr
                    .get(&d.address)
                {
                    Some(PoolStateUpdate::V3 {
                        sqrt_price_x96,
                        tick,
                        liquidity,
                    }) => (*sqrt_price_x96, *tick, *liquidity),
                    _ => {
                        let slot0 = match fetch_v3_slot0(rpc_client, &d.address, block_tag) {
                            Ok(s) => s,
                            Err(e) => {
                                warn!(pool = ?d.address, ?e, "v3 slot0 failed; skipping");
                                snapshot_fail += 1;
                                continue;
                            }
                        };
                        let liquidity = match fetch_v3_liquidity(rpc_client, &d.address, block_tag)
                        {
                            Ok(l) => l,
                            Err(e) => {
                                warn!(pool = ?d.address, ?e, "v3 liquidity failed; skipping");
                                snapshot_fail += 1;
                                continue;
                            }
                        };
                        (slot0.sqrt_price_x96, slot0.tick, liquidity)
                    }
                };
                // Phase 1: cheap state only (slot0 + liquidity). The tick window is the
                // expensive per-pool fetch and is deferred to Phase 2 (survivors only),
                // so a pool dropped by the §5.5 floor never pays for it.
                SnapshotState::V3(V3State {
                    sqrt_price_x96,
                    tick,
                    liquidity,
                    fee_pips: d.fee_pips,
                    tick_spacing: d.tick_spacing,
                    initialised_ticks: Vec::new(),
                })
            }
            PoolKind::UniswapV4 => {
                // Hookless V4: same CL math as V3, read via the StateView lens by poolId.
                let Some(pool_id) = v4_pool_id_by_addr.get(&d.address).copied() else {
                    warn!(pool = ?d.address, "v4 poolId side-map miss; skipping");
                    snapshot_fail += 1;
                    continue;
                };
                let state_view = match parse_address(&cfg.discovery.v4_state_view) {
                    Ok(a) => a,
                    Err(e) => {
                        warn!(?e, addr = %cfg.discovery.v4_state_view, "v4 state_view malformed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                };
                // Phase-1 batched cheap state (getSlot0 + getLiquidity) first; fall back
                // to the per-pool StateView reads on a batch MISS. Decode is identical
                // (the batched helper reuses the per-pool decode), so this only changes
                // I/O — the registration decision is unchanged.
                let (slot0, liquidity) = match v4_state_by_addr.get(&d.address).copied() {
                    Some(cheap) => (cheap.slot0, cheap.liquidity),
                    None => {
                        let slot0 =
                            match fetch_v4_slot0(rpc_client, &state_view, &pool_id, block_tag) {
                                Ok(s) => s,
                                Err(e) => {
                                    warn!(pool = ?d.address, ?e, "v4 getSlot0 failed; skipping");
                                    snapshot_fail += 1;
                                    continue;
                                }
                            };
                        let liquidity = match fetch_v4_liquidity(
                            rpc_client,
                            &state_view,
                            &pool_id,
                            block_tag,
                        ) {
                            Ok(l) => l,
                            Err(e) => {
                                warn!(pool = ?d.address, ?e, "v4 getLiquidity failed; skipping");
                                snapshot_fail += 1;
                                continue;
                            }
                        };
                        (slot0, liquidity)
                    }
                };
                // Phase 1: cheap state only (getSlot0 + getLiquidity). The StateView tick
                // window (getTickBitmap + per-tick getTickInfo) is the costly part and is
                // deferred to Phase 2 for survivors only.
                // V4 lpFee is dynamic-capable; use the discovery fee where static, else
                // the live lpFee from slot0.
                let fee_pips = if d.fee_pips == 0 || d.fee_pips > 1_000_000 {
                    slot0.lp_fee
                } else {
                    d.fee_pips
                };
                SnapshotState::V3(V3State {
                    sqrt_price_x96: slot0.sqrt_price_x96,
                    tick: slot0.tick,
                    liquidity,
                    fee_pips,
                    tick_spacing: d.tick_spacing,
                    initialised_ticks: Vec::new(),
                })
            }
            PoolKind::AlgebraV19 => {
                // Algebra V1.9 (Camelot V3) uses `globalState()` instead of `slot0()`
                // and stores a dynamic fee at the pool. `liquidity()` and `ticks(int24)`
                // selectors match Uniswap V3 exactly so we can reuse those readers.
                let gs = match fetch_algebra_global_state(rpc_client, &d.address, block_tag) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "algebra globalState failed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                };
                let liquidity = match fetch_v3_liquidity(rpc_client, &d.address, block_tag) {
                    Ok(l) => l,
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "algebra liquidity failed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                };
                // Algebra spacing is MUTABLE (admin setTickSpacing): the discovery value is
                // creation-time and can be stale (observed live: created 60, now 10 — ticks off
                // the stale grid were invisible → 28/220 parity mismatches). Read it live;
                // fail-closed to skipping the pool rather than registering a wrong grid.
                let live_spacing = match engine_rpc::fetch_algebra_tick_spacing(
                    rpc_client, &d.address, block_tag,
                ) {
                    Ok(ts) => {
                        if ts != d.tick_spacing {
                            info!(
                                pool = ?d.address,
                                discovery = d.tick_spacing,
                                live = ts,
                                "algebra tickSpacing re-spaced since creation — using LIVE value"
                            );
                        }
                        ts
                    }
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "algebra tickSpacing read failed; skipping pool (fail-closed: a wrong grid over-quotes)");
                        snapshot_fail += 1;
                        continue;
                    }
                };
                // Phase 1: cheap state only. Tick window deferred to Phase 2 (survivors).
                SnapshotState::V3(V3State {
                    sqrt_price_x96: gs.sqrt_price_x96,
                    tick: gs.tick,
                    liquidity,
                    // Use the live Algebra fee — discovery stored 0 because Algebra fees
                    // are dynamic and only resolvable from on-chain state.
                    fee_pips: gs.fee_pips,
                    tick_spacing: live_spacing,
                    initialised_ticks: Vec::new(),
                })
            }
            PoolKind::KyberSwapElastic => {
                // KyberSwap Elastic uses `getPoolState()` instead of `slot0()` and
                // `getLiquidityState()` instead of `liquidity()`. Same `ticks(int24)`
                // ABI as V3, so we still reuse `fetch_v3_tick_window`.
                let slot0 =
                    match engine_rpc::fetch_kyber_pool_state(rpc_client, &d.address, block_tag) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(pool = ?d.address, ?e, "kyber getPoolState failed; skipping");
                            snapshot_fail += 1;
                            continue;
                        }
                    };
                // WI-02: Kyber activation is policy-sealed (explicit KYBER_ENABLE opt-in —
                // the 2026-07-02 silent default-on was corrected as an artifact-source
                // drift). When the family is active the engine prices via the Elastic kernel
                // quote path (pipeline::quote_one_hop), which must price against TOTAL active
                // liquidity `baseL + reinvestL` (reinvested fees are live liquidity) —
                // feeding baseL alone understates liquidity and was the historical
                // mis-pricing root cause.
                let liquidity_res = if policy
                    .active_families
                    .contains(PoolKind::KyberSwapElastic)
                {
                    engine_rpc::fetch_kyber_liquidity_total(rpc_client, &d.address, block_tag)
                } else {
                    engine_rpc::fetch_kyber_liquidity(rpc_client, &d.address, block_tag)
                };
                let liquidity = match liquidity_res {
                    Ok(l) => l,
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "kyber getLiquidityState failed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                };
                // Phase 1: cheap state only. Tick window deferred to Phase 2 (survivors).
                // [AUD-012] Discovery stored the RAW `swapFeeUnits` (Kyber FEE_UNITS = 1e5) in
                // `fee_pips`; the kernel prices in ppm (1e6) — convert here (10×), else the fee
                // is under-charged 10× and the quote OVER-states output by ~0.9×fee.
                SnapshotState::V3(V3State {
                    sqrt_price_x96: slot0.sqrt_price_x96,
                    tick: slot0.tick,
                    liquidity,
                    fee_pips: engine_rpc::kyber_fee_units_to_ppm(d.fee_pips),
                    tick_spacing: d.tick_spacing,
                    initialised_ticks: Vec::new(),
                })
            }
            PoolKind::DodoV2 => {
                // Discovery doesn't carry the DODO sub-class — default to DVM and let
                // a follow-up refinement bucket the others. Math is identical across
                // DVM/DPP/DSP except for adjusted_target behaviour (DPP/DSP carry
                // explicit B0/Q0 targets — pulled from getPMMStateForCall).
                let head = match block_tag {
                    BlockTag::Number(n) => n,
                    _ => 0,
                };
                // Phase-1 batched DODO cheap state first; fall back to the per-pool
                // 4-call read on a batch MISS. The batched decode reuses the per-pool
                // `decode_dodo_state` byte-for-byte (a DODO mis-decode historically
                // produced phantom profit), so the registered DodoState is identical.
                match dodo_by_addr.get(&d.address) {
                    Some(s) => SnapshotState::Dodo(s.clone()),
                    None => match engine_rpc::fetch_dodo_state(
                        rpc_client,
                        &d.address,
                        engine_amm::dodo_v2::DodoPoolClass::Dvm,
                        block_tag,
                        head,
                    ) {
                        Ok(s) => SnapshotState::Dodo(s),
                        Err(e) => {
                            warn!(pool = ?d.address, ?e, "dodo snapshot failed; skipping");
                            snapshot_fail += 1;
                            continue;
                        }
                    },
                }
            }
            PoolKind::MaverickV2 => {
                let head = match block_tag {
                    BlockTag::Number(n) => n,
                    _ => 0,
                };
                let mut state = match engine_rpc::fetch_maverick_state_basic(
                    rpc_client, &d.address, block_tag, head,
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "maverick basic state failed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                };
                // Lens-based tick window fetch. If the operator hasn't configured
                // a lens address, leave the tick window empty — kernel will return
                // TickWindowExhausted on any quote (silent reject, expected).
                let lens_addr_str = cfg.discovery.maverick_lens.trim();
                if !lens_addr_str.is_empty() {
                    if let Ok(lens_addr) = parse_address(lens_addr_str) {
                        let tick_radius = std::env::var("MAVERICK_TICK_RADIUS")
                            .ok()
                            .and_then(|s| s.parse::<i32>().ok())
                            .unwrap_or(engine_amm::maverick_v2::DEFAULT_TICK_RADIUS);
                        match engine_rpc::fetch_maverick_lens(
                            rpc_client,
                            &lens_addr,
                            &d.address,
                            tick_radius,
                            block_tag,
                        ) {
                            Ok(lens) => {
                                state.ticks = lens.ticks;
                                state.active_tick = lens.active_tick;
                                state.sqrt_price_d18 = lens.sqrt_price_d18;
                                // Fees (fee_zto/fee_otz) come from `fetch_maverick_state_basic`
                                // via `fee(bool)` — the lens does NOT return them (the old
                                // `WLiquidity` variant that claimed to was a phantom function).
                                // The lens DOES give us the protocol fee ratio from getState().
                                state.protocol_fee_ratio_d3 = lens.protocol_fee_ratio_d3;
                                state.tick_radius = tick_radius;
                            }
                            Err(e) => {
                                warn!(pool = ?d.address, ?e, "maverick lens fetch failed; ticks empty");
                            }
                        }
                    }
                }
                SnapshotState::Maverick(state)
            }
            PoolKind::SolidlyV3 => {
                // Solidly stable pools use V2-style reserves (token0/token1 reserves +
                // block timestamp). We reuse the V2 reserves snapshot and dispatch to the
                // stable-curve math via the PoolKind tag at evaluator dispatch time.
                // Phase-1 batched reserves first (Solidly maps to PriceFetchKind::V2);
                // fall back to a per-pool getReserves on a batch miss.
                let reserves = match state_by_addr.get(&d.address) {
                    Some(PoolStateUpdate::V2 {
                        reserve0,
                        reserve1,
                        block_timestamp_last,
                    }) => Ok((*reserve0, *reserve1, *block_timestamp_last)),
                    _ => match fetch_v2_reserves(rpc_client, &d.address, block_tag) {
                        Ok(s) => Ok((s.reserve0, s.reserve1, s.block_timestamp_last)),
                        Err(e) => Err(e),
                    },
                };
                match reserves {
                    Ok((reserve0, reserve1, block_timestamp_last)) => SnapshotState::V2(V2State {
                        reserves: V2Reserves256 {
                            reserve0,
                            reserve1,
                            block_timestamp_last,
                        },
                    }),
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "solidly stable snapshot failed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                }
            }
            PoolKind::BalancerStable => 'bal: {
                // Snapshot via the Vault. Stable pools resolve through getAmplificationParameter;
                // a pool that fails that is most likely **Weighted**. When BALANCER_WEIGHTED_ENABLE=1
                // we fall back to the weighted snapshot and register as BalancerWeighted; otherwise
                // we skip (default OFF — the weighted quote kernel is reference-vector-tested but
                // not yet validated against ≥3 live weighted quotes, and we never route real size
                // through unvalidated quote math: same discipline as Maverick). The `'bal` label
                // lets the weighted fallback yield the arm's `SnapshotState` directly.
                //
                // Batched resolution (so the registered set matches the per-pool path):
                //  * HIT → batched view. MISS+resolved → non-stable (try weighted). MISS+Err →
                //    per-pool Vault read, then weighted on failure.
                macro_rules! try_weighted_or_skip {
                    () => {{
                        if env_flag_opt_in("BALANCER_WEIGHTED_ENABLE") {
                            match engine_rpc::fetch_balancer_weighted(
                                rpc_client,
                                &engine_rpc::BALANCER_VAULT_ARBITRUM,
                                &d.address,
                                block_tag,
                            ) {
                                Ok(slot) => break 'bal SnapshotState::BalancerWeighted(slot),
                                Err(e) => {
                                    warn!(pool = ?d.address, ?e, "balancer pool neither stable nor weighted-decodable; skipping");
                                    snapshot_fail += 1;
                                    continue 'snap;
                                }
                            }
                        } else {
                            snapshot_fail += 1;
                            continue 'snap;
                        }
                    }};
                }
                let view = match balancer_by_addr.get(&d.address).cloned() {
                    Some(v) => v,
                    None if balancer_batched.contains(&d.address) => try_weighted_or_skip!(),
                    None => match engine_rpc::fetch_balancer_stable(
                        rpc_client,
                        &engine_rpc::BALANCER_VAULT_ARBITRUM,
                        &d.address,
                        block_tag,
                    ) {
                        Ok(v) => v,
                        Err(_) => try_weighted_or_skip!(),
                    },
                };
                use engine_amm::balancer_stable::{BalancerStableState, MAX_TOKENS};
                let mut kbal = [0u128; MAX_TOKENS];
                let mut ksf = [alloy_primitives::U256::ZERO; MAX_TOKENS];
                let mut ktok = [[0u8; 20]; MAX_TOKENS];
                let mut n = 0usize;
                for idx in 0..view.tokens.len() {
                    if Some(idx) == view.bpt_index {
                        continue;
                    }
                    if n >= MAX_TOKENS {
                        break;
                    }
                    kbal[n] = view.balances[idx].try_into().unwrap_or(u128::MAX);
                    ksf[n] = view.scaling_factors[idx];
                    ktok[n] = view.tokens[idx];
                    n += 1;
                }
                if n < 2 {
                    snapshot_fail += 1;
                    continue 'snap;
                }
                let kernel = BalancerStableState {
                    n,
                    balances: kbal,
                    scaling_factors: ksf,
                    amp: view.amp,
                    swap_fee_1e18: view.swap_fee_1e18,
                };
                SnapshotState::BalancerStable(engine_state::BalancerStableSlot {
                    kernel,
                    tokens: ktok,
                    pool_id: view.pool_id,
                })
            }
            PoolKind::CurveStableSwapNg => {
                // Curve StableSwap-NG plain pool: per-pool contract reads (coins/balances/
                // A_precise/fee/stored_rates). `n_coins` is unknown from discovery (it
                // carries only the registered pair), so probe up to 3 coins; the reader
                // trims to the live coin count. The static-fee kernel does not model the
                // NG offpeg dynamic fee — accurate for balanced stable pairs, slightly
                // optimistic on heavily-imbalanced pools (documented in fetch_curve_state).
                let view = match engine_rpc::fetch_curve_state(rpc_client, &d.address, 3, block_tag)
                {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "curve snapshot failed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                };
                if view.kernel.n_coins < 2 {
                    snapshot_fail += 1;
                    continue;
                }
                SnapshotState::Curve(engine_state::CurveSlot::new(view.kernel, view.tokens))
            }
            PoolKind::CurveCryptoSwap => {
                // Curve CryptoSwap-NG. Try the 3-coin Tricrypto-NG reader first; on failure (e.g.
                // `coins(2)` reverts → a 2-coin TwoCrypto-NG pool) fall back to the 2-coin reader.
                // Either kernel is integer-exact; the slot's `quote_amount_out` dispatches by coin
                // count. Both readers return the coin addresses aligned to the kernel's indices.
                match engine_rpc::fetch_cryptoswap_state(rpc_client, &d.address, block_tag) {
                    Ok((kernel, tokens)) => {
                        SnapshotState::CurveCrypto(engine_state::CryptoCurveSlot {
                            kernel,
                            twocrypto_kernel: None,
                            tokens,
                            n_coins: 3,
                        })
                    }
                    Err(_) => {
                        match engine_rpc::fetch_twocrypto_state(rpc_client, &d.address, block_tag) {
                            Ok((tk, coins2)) => {
                                let mut tokens = [[0u8; 20]; 3]; // MAX_CRYPTO_COINS
                                tokens[0] = coins2[0];
                                tokens[1] = coins2[1];
                                SnapshotState::CurveCrypto(engine_state::CryptoCurveSlot {
                                    kernel: engine_state::CryptoCurveSlot::placeholder_kernel(),
                                    twocrypto_kernel: Some(tk),
                                    tokens,
                                    n_coins: 2,
                                })
                            }
                            Err(e) => {
                                warn!(pool = ?d.address, ?e, "curve-crypto snapshot failed (3- and 2-coin); skipping");
                                snapshot_fail += 1;
                                continue;
                            }
                        }
                    }
                }
            }
            PoolKind::TraderJoeLb => {
                // Trader Joe LB: fetch the full kernel LBState (active bin + ±window
                // bins, static + variable fee params) via the v2.1/v2.2 read selectors.
                match engine_rpc::lb_snapshot::fetch_lb_state(
                    rpc_client, &d.address, block_tag, head_block,
                ) {
                    Ok(state) => SnapshotState::Lb(state),
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "lb snapshot failed; skipping");
                        snapshot_fail += 1;
                        continue;
                    }
                }
            }
            _ => {
                snapshot_fail += 1;
                continue;
            }
        };
        let venue = VenueId(
            venue_id_for_family
                .get(&pool_kind_to_family(d.family).to_string())
                .copied()
                .unwrap_or(0),
        );
        validated.push(PoolWithSnapshot { venue, d, snapshot });
    }
    info!(
        validated = validated.len(),
        codehash_fail,
        snapshot_fail,
        "§5.4 gates 2+3 complete (Validated + SnapshotReady; phase-1 cheap state — V3/V4/Algebra/Kyber tick windows backfilled for survivors in phase-2 below)"
    );

    // C4: flush recorded codehashes to `research/dex/<family>/runtime.hash`. After this run the
    // operator audits the artifacts (git-diffable, one hex hash per line), then flips
    // `enforce_codehash=true` to make the gate reject any non-recorded bytecode. Only writes in
    // record-pass mode (non-enforce + CODEHASH_RECORD!=0).
    if let Some(rec) = codehash_recorder.take() {
        let dir = std::path::Path::new(&artifact_dir);
        let total: usize = engine_discovery::RECORDED_FAMILIES
            .iter()
            .map(|&f| rec.count(f))
            .sum();
        let files = rec.write_dir(dir);
        info!(
            files,
            distinct_hashes = total,
            dir = %artifact_dir,
            "codehash artifacts recorded (audit + set enforce_codehash=true to enforce)"
        );
    }

    // Liquidity floor. Dune verification of the engine's candidates showed it was
    // routing through near-dead V2/V3 pools (1-143 trades/week, stale on-chain
    // prices) and producing real-but-UNCAPTURABLE 1-4% "arbs" — the pools simply
    // lack the depth to execute the quoted size. Drop any V2/V3 pool whose
    // on-chain notional TVL is below the floor, computed decimal-correctly from
    // the configured tokens' rough USD prices. Non-V2/V3 families (DODO/Maverick)
    // are kept (their dedicated evaluators gate them separately).
    // Operator-adjustable liquidity floor (`POOL_NOTIONAL_FLOOR_USD`, default 100k). Lower it to
    // admit more (thinner) pools into the resident set — the primary lever for "monitor more
    // pools". The stable/PMM floor (Balancer/Curve/DODO) tracks it down but is capped at 25k.
    // TRADE-OFF: a lower floor admits thinner pools → more marginal/uncapturable candidates
    // (the MIN_NET_PROFIT/ROI gates still filter them, and the kernels now fail-closed on dust).
    let min_pool_notional_usd: f64 = std::env::var("POOL_NOTIONAL_FLOOR_USD")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(100_000.0);
    // The stable/PMM floor (Balancer/Curve/DODO) is INDEPENDENT of the volatile floor and stays
    // at its protective $25k by default: it specifically defeats the stale-rate-provider Balancer
    // dust phantom (a $12-TVL pool implying a $3,871 mark → ~1.85x phantom). Lowering the volatile
    // floor to admit more x*y=k pools must NOT drag this down. Separately tunable if needed.
    let stable_pool_floor_usd: f64 = std::env::var("POOL_STABLE_FLOOR_USD")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(25_000.0);

    // On-chain token→ETH pricing. Replaces the prior hardcoded `symbol => USD` map
    // (stale prices, hand-guessed LINK/GMX, fixed 2058 anchor). Derive each registered
    // token's price *in ETH* from the pools we just snapshotted, then carry it through
    // both (a) the per-token Q96 ETH-price for profit valuation and (b) the §5.5 USD
    // liquidity floor (ETH price × the on-chain ETH/USD anchor). See `token_pricing`.
    let decimals_by_addr: ahash::AHashMap<[u8; 20], u8> = cfg
        .tokens
        .iter()
        .filter_map(|t| parse_address(&t.address).ok().map(|a| (a, t.decimals)))
        .collect();
    let symbol_by_addr: ahash::AHashMap<[u8; 20], String> = cfg
        .tokens
        .iter()
        .filter_map(|t| {
            parse_address(&t.address)
                .ok()
                .map(|a| (a, t.symbol.to_ascii_uppercase()))
        })
        .collect();

    // Project each snapshotted pool into the address-keyed price graph input. V2/V3 feed
    // the spot graph directly; Curve StableSwap-NG and Balancer stable pools feed it via
    // a small `get_dy` / `calc_out_given_in` probe (so e.g. crvUSD, whose depth is
    // Curve-only, marks correctly). DODO/Maverick/LB still price through shared V2/V3.
    let price_pools: Vec<crate::token_pricing::PricePool> = validated
        .iter()
        // WI-02 P1-08: pricing anchors are rebuilt from ACTIVE families only — an
        // inactive family's pools never contribute a token price anchor.
        .filter(|p| crate::runtime_policy::anchor_pool_admitted(policy, p.d.family))
        .filter_map(|p| {
            let d0 = decimals_by_addr.get(&p.d.token0).copied().unwrap_or(18);
            let d1 = decimals_by_addr.get(&p.d.token1).copied().unwrap_or(18);
            match &p.snapshot {
                SnapshotState::V2(s) => Some(crate::token_pricing::PricePool::V2 {
                    token0: p.d.token0,
                    token1: p.d.token1,
                    dec0: d0,
                    dec1: d1,
                    reserve0: s.reserves.reserve0,
                    reserve1: s.reserves.reserve1,
                }),
                SnapshotState::V3(s) => Some(crate::token_pricing::PricePool::V3 {
                    token0: p.d.token0,
                    token1: p.d.token1,
                    dec0: d0,
                    dec1: d1,
                    sqrt_price_x96: u256_to_f64(s.sqrt_price_x96),
                    liquidity: s.liquidity,
                }),
                SnapshotState::Curve(slot) => {
                    let mut decimals = [0u8; engine_amm::curve::MAX_COINS];
                    for k in 0..slot.kernel.n_coins.min(decimals.len()) {
                        decimals[k] = decimals_by_addr.get(&slot.tokens[k]).copied().unwrap_or(18);
                    }
                    Some(crate::token_pricing::PricePool::Curve {
                        tokens: slot.tokens,
                        decimals,
                        kernel: slot.kernel,
                    })
                }
                SnapshotState::BalancerStable(slot) => {
                    let mut decimals = [0u8; engine_amm::balancer_stable::MAX_TOKENS];
                    for k in 0..slot.kernel.n.min(decimals.len()) {
                        decimals[k] = decimals_by_addr.get(&slot.tokens[k]).copied().unwrap_or(18);
                    }
                    Some(crate::token_pricing::PricePool::Balancer {
                        tokens: slot.tokens,
                        decimals,
                        kernel: slot.kernel,
                    })
                }
                _ => None,
            }
        })
        .collect();
    let all_token_addrs: Vec<[u8; 20]> = decimals_by_addr.keys().copied().collect();

    // USD-anchor addresses (≈ $1) and the small static bootstrap anchors used ONLY for
    // hubs / unrouted tokens. We key these by configured symbol so they track whatever
    // tokens the config carries; the on-chain pool prices override them whenever a real
    // route exists. ETH-equivalent anchors are stated as fractions of ETH (filled in
    // after the ETH/USD anchor is known) so they stay internally consistent.
    let usd_anchor_addrs: Vec<[u8; 20]> = symbol_by_addr
        .iter()
        .filter(|(_, s)| {
            matches!(
                s.as_str(),
                "USDC" | "USDC.E" | "USDT0" | "USD₮0" | "USDT" | "DAI" | "USDE" | "GHO" | "AXLUSDC"
            )
        })
        .map(|(a, _)| *a)
        .collect();

    // Chainlink snapshot (optional). Batch latestRoundData()+decimals() for every
    // configured feed in one Multicall3 call. The WETH/USD feed becomes the
    // authoritative ETH/USD anchor; per-token feeds become high-confidence marks that
    // override + cross-check the pool oracle below. A reverting / implausible feed
    // resolves to None and we fall back to pool pricing.
    let mut chainlink_eth_per_token: ahash::AHashMap<[u8; 20], f64> = ahash::AHashMap::default();
    let mut chainlink_eth_usd: Option<f64> = None;
    if !cfg.chainlink_feeds.is_empty() {
        let mut feed_addrs: Vec<[u8; 20]> = Vec::new();
        let mut feed_meta: Vec<([u8; 20], bool)> = Vec::new(); // (token, is_usd_quote)
        for f in &cfg.chainlink_feeds {
            let (Ok(tok), Ok(feed)) = (parse_address(&f.token), parse_address(&f.feed)) else {
                warn!(token = %f.token, feed = %f.feed, "chainlink feed: bad address; skipping");
                continue;
            };
            feed_addrs.push(feed);
            feed_meta.push((tok, !f.quote.eq_ignore_ascii_case("eth")));
        }
        match engine_rpc::fetch_chainlink_marks(
            rpc_client,
            &MULTICALL3_ARBITRUM,
            &feed_addrs,
            block_tag,
        ) {
            Ok(marks) => {
                // Stale-feed guard (fail-closed): a feed whose latestRoundData.updatedAt is older
                // than CHAINLINK_MAX_AGE_SECS (24h ≥ Arbitrum ETH/USD heartbeat) is treated as
                // unavailable, so we never anchor pricing on an answer the oracle stopped updating.
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let max_age = engine_rpc::CHAINLINK_MAX_AGE_SECS;
                // First pass: the ETH/USD anchor (WETH-token, usd-quote feed).
                for (i, m) in marks.iter().enumerate() {
                    let (tok, is_usd) = feed_meta[i];
                    if tok == v4_weth && is_usd {
                        if let Some(mark) = m {
                            match mark.value_if_fresh(now_secs, max_age) {
                                Some(v) if (100.0..=1_000_000.0).contains(&v) => {
                                    chainlink_eth_usd = Some(v);
                                }
                                None => warn!(
                                    token = ?tok,
                                    age_secs = now_secs.saturating_sub(mark.updated_at),
                                    "chainlink ETH/USD feed is stale; falling back to pool oracle"
                                ),
                                _ => {}
                            }
                        }
                    }
                }
                let cl_ethusd = chainlink_eth_usd.unwrap_or(0.0);
                // Second pass: per-token ETH marks.
                for (i, m) in marks.iter().enumerate() {
                    let (tok, is_usd) = feed_meta[i];
                    let Some(mark) = m else {
                        warn!(token = ?tok, "chainlink feed unavailable; using pool mark");
                        continue;
                    };
                    // Fail-closed staleness guard: skip a feed whose answer is older than the
                    // heartbeat-derived max-age and fall back to the pool mark for this token.
                    let Some(token_value) = mark.value_if_fresh(now_secs, max_age) else {
                        warn!(
                            token = ?tok,
                            age_secs = now_secs.saturating_sub(mark.updated_at),
                            "chainlink token feed is stale; using pool mark"
                        );
                        continue;
                    };
                    let eth_px = if is_usd {
                        if cl_ethusd <= 0.0 {
                            continue; // need ETH/USD to convert a USD feed
                        }
                        token_value / cl_ethusd
                    } else {
                        token_value // token/ETH directly
                    };
                    if eth_px.is_finite() && eth_px > 0.0 {
                        chainlink_eth_per_token.insert(tok, eth_px);
                    }
                }
                info!(
                    feeds = feed_addrs.len(),
                    resolved = chainlink_eth_per_token.len(),
                    eth_usd = chainlink_eth_usd
                        .map(|v| format!("{:.2}", v))
                        .unwrap_or_else(|| "none".into()),
                    "chainlink marks snapshotted"
                );
            }
            Err(e) => warn!(?e, "chainlink snapshot failed; using pool pricing only"),
        }
    }

    // ETH/USD anchor: prefer the Chainlink feed; else the deepest WETH/USDC-class pool;
    // else the 2058 constant.
    let eth_usd = chainlink_eth_usd.unwrap_or_else(|| {
        crate::token_pricing::derive_eth_usd(&price_pools, &usd_anchor_addrs, 2058.0)
    });

    // Bootstrap anchors for hub tokens (only used when a hub has NO direct WETH pool, so
    // a token→hub→WETH chain can still resolve). USD-pegged hubs anchor to 1/eth_usd ETH;
    // WBTC anchors to a coarse BTC≈30× ETH so it can bridge if it lacks a direct WETH pool.
    let mut hub_anchors: Vec<([u8; 20], f64)> = Vec::new();
    for (addr, sym) in &symbol_by_addr {
        let anchor_eth = match sym.as_str() {
            "USDC" | "USDC.E" | "USDT0" | "USD₮0" | "USDT" | "DAI" | "USDE" | "GHO" | "AXLUSDC" => {
                1.0 / eth_usd
            }
            "WBTC" => 30.0, // coarse BTC/ETH; overridden by any real WBTC pool
            _ => continue,
        };
        hub_anchors.push((*addr, anchor_eth));
    }

    // Derive eth-per-full-token for every registered token.
    let mut token_eth_price = crate::token_pricing::derive_token_eth_prices(
        &price_pools,
        &all_token_addrs,
        &decimals_by_addr,
        &hub_anchors,
    );

    // Cross-check / override with Chainlink marks where present. Chainlink is the
    // higher-confidence source; a material divergence from the pool mark flags a
    // dislocated/thin on-chain market (e.g. a deprecated token still quoting its old
    // peg) and is logged before we adopt the Chainlink value.
    for (tok, &cl_eth) in &chainlink_eth_per_token {
        let sym = symbol_by_addr.get(tok).map(|s| s.as_str()).unwrap_or("?");
        if let Some(&pool_eth) = token_eth_price.get(tok) {
            if pool_eth > 0.0 {
                let div_bps = ((cl_eth - pool_eth).abs() / pool_eth * 10_000.0) as i64;
                if div_bps > 200 {
                    warn!(
                        token = sym,
                        pool_eth = format!("{:.6}", pool_eth),
                        chainlink_eth = format!("{:.6}", cl_eth),
                        divergence_bps = div_bps,
                        "chainlink/pool mark divergence; adopting chainlink"
                    );
                }
            }
        }
        token_eth_price.insert(*tok, cl_eth);
    }

    // Build the USD price map the liquidity floor consumes: (decimals, usd_price).
    // usd_price = eth_per_token * eth_usd. Unpriced tokens are simply absent (treated
    // as $0 by the floor and unpriced by valuation — fail closed for that base).
    let token_px: ahash::AHashMap<[u8; 20], (u8, f64)> = token_eth_price
        .iter()
        .filter_map(|(addr, &eth_px)| {
            let dec = decimals_by_addr.get(addr).copied()?;
            Some((*addr, (dec, eth_px * eth_usd)))
        })
        .collect();

    // Log the derived prices for the major tokens (dry-run visibility / drift checks).
    {
        let mut sorted: Vec<(&[u8; 20], &f64)> = token_eth_price.iter().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
        let anchor_src = if chainlink_eth_usd.is_some() {
            "chainlink ETH/USD feed"
        } else {
            "deepest WETH/USDC pool"
        };
        info!(
            eth_usd = format!("{:.2}", eth_usd),
            anchor = anchor_src,
            priced = token_eth_price.len(),
            of = all_token_addrs.len(),
            "token pricing derived"
        );
        for (addr, eth_px) in sorted.iter() {
            let sym = symbol_by_addr.get(*addr).map(|s| s.as_str()).unwrap_or("?");
            info!(
                token = sym,
                eth_price = format!("{:.6}", eth_px),
                usd_price = format!("{:.2}", **eth_px * eth_usd),
                "derived token price"
            );
        }
    }

    // Wire per-token ETH-price factors so profit accounting (denominated in ETH-wei)
    // can convert a cycle's gross from its base-token raw units. Without this, a
    // non-WETH-funded cycle (ARB, USDC, …) is mis-valued because raw token units are
    // not 1:1 with wei. q96 = eth_per_full_token * 10^(18-dec) * 2^96. Tokens with no
    // derived price stay at ZERO → evaluators fail closed.
    //
    // WI-02 P1-08: the set of tokens that received a REAL anchor is tracked so an
    // unpriced token excludes its pools BEFORE routes are built (route_pool_eligible).
    let mut priced_token_ids: ahash::AHashSet<engine_state::TokenId> = ahash::AHashSet::default();
    {
        for (addr, &eth_px) in token_eth_price.iter() {
            let Some(dec) = decimals_by_addr.get(addr).copied() else {
                continue;
            };
            if eth_px <= 0.0 || dec > 18 {
                continue;
            }
            let Some(&id) = token_id_by_addr.get(addr) else {
                continue;
            };
            // Fixed-point: q96 = round(eth_px * 1e12) * 10^(18-dec) * 2^96 / 1e12.
            // 1e12 picosecond-of-ETH precision keeps small LST/stable ratios exact
            // without f64 in the published factor.
            let eth_pico = (eth_px * 1e12) as u128;
            if eth_pico == 0 {
                continue;
            }
            let scale10 = U256::from(10u64).pow(U256::from(18 - dec as u32));
            let q96 = U256::from(eth_pico) * (U256::from(1u64) << 96) * scale10
                / U256::from(1_000_000_000_000u128);
            ub.set_token_eth_price_q96(id, q96);
            priced_token_ids.insert(id);
        }
    }
    let pool_notional_usd = |p: &PoolWithSnapshot| -> f64 {
        let (d0, p0) = token_px.get(&p.d.token0).copied().unwrap_or((18, 0.0));
        let (d1, p1) = token_px.get(&p.d.token1).copied().unwrap_or((18, 0.0));
        match &p.snapshot {
            SnapshotState::V2(s) => {
                let r0 = s.reserves.reserve0 as f64 / 10f64.powi(d0 as i32);
                let r1 = s.reserves.reserve1 as f64 / 10f64.powi(d1 as i32);
                r0 * p0 + r1 * p1
            }
            SnapshotState::V3(s) => {
                const Q96: f64 = 79_228_162_514_264_337_593_543_950_336.0;
                let sp = u256_to_f64(s.sqrt_price_x96);
                if sp <= 0.0 || s.liquidity == 0 {
                    return 0.0;
                }
                let l = s.liquidity as f64;
                let v0 = (l * Q96 / sp) / 10f64.powi(d0 as i32) * p0;
                let v1 = (l * sp / Q96) / 10f64.powi(d1 as i32) * p1;
                v0 + v1
            }
            SnapshotState::BalancerStable(slot) => {
                // Full pool TVL = Σ balance_k × price_k over all (non-BPT) tokens.
                let mut sum = 0.0;
                for k in 0..slot.kernel.n {
                    let (d, px) = token_px.get(&slot.tokens[k]).copied().unwrap_or((18, 0.0));
                    sum += (slot.kernel.balances[k] as f64 / 10f64.powi(d as i32)) * px;
                }
                sum
            }
            SnapshotState::Curve(slot) => {
                // Full pool TVL = Σ balance_k × price_k over all coins.
                let mut sum = 0.0;
                for k in 0..slot.kernel.n_coins {
                    let (d, px) = token_px.get(&slot.tokens[k]).copied().unwrap_or((18, 0.0));
                    sum += (slot.kernel.balances[k] as f64 / 10f64.powi(d as i32)) * px;
                }
                sum
            }
            SnapshotState::CurveCrypto(slot) => {
                // CryptoSwap TVL = Σ balance_k × price_k (coins differ in price — WBTC/WETH/USDT —
                // genuine multi-asset TVL). `balances()` dispatches to the active 2-/3-coin kernel.
                let bals = slot.balances();
                let mut sum = 0.0;
                for k in 0..slot.n_coins as usize {
                    let (d, px) = token_px.get(&slot.tokens[k]).copied().unwrap_or((18, 0.0));
                    sum += (u256_to_f64(bals[k]) / 10f64.powi(d as i32)) * px;
                }
                sum
            }
            SnapshotState::Dodo(s) => {
                // base_reserve in base-token units, quote_reserve in quote-token units;
                // map to the registered (token0, token1) prices by matching addresses.
                let base_is_t0 = s.base_token.as_slice() == p.d.token0.as_slice();
                let (bd, bpx) = if base_is_t0 { (d0, p0) } else { (d1, p1) };
                let (qd, qpx) = if base_is_t0 { (d1, p1) } else { (d0, p0) };
                let base = u256_to_f64(s.base_reserve) / 10f64.powi(bd as i32) * bpx;
                let quote = u256_to_f64(s.quote_reserve) / 10f64.powi(qd as i32) * qpx;
                base + quote
            }
            _ => f64::INFINITY,
        }
    };
    let before_floor = validated.len();
    let mut validated: Vec<PoolWithSnapshot> = validated
        .into_iter()
        .filter(|p| {
            // KyberSwapElastic is excluded from the live quoter (its V3-state snapshot
            // mis-prices — see pipeline `quote` comment), so it must never take a resident
            // slot. It also carries `SnapshotState::V3`, so registering it would hit
            // `add_v3_pool` (which only accepts UniV3/Sushi/Pancake/Algebra/V4) — a debug
            // panic / release mis-store. Dropping it here is correct AND fixes that crash,
            // which a lowered POOL_NOTIONAL_FLOOR_USD would otherwise surface.
            if matches!(p.d.family, PoolKind::KyberSwapElastic) {
                return false;
            }
            // Exempt amplified/PMM venues from the volatile-pool TVL floor. Balancer
            // stable (A=100–2000), DODO (PMM), and Maverick concentrate liquidity so a
            // modest-TVL pool is still low-impact near its operating point — the $100K
            // floor (tuned for x*y=k pools) wrongly drops them. Safety is handled
            // downstream: the kernel returns ZeroLiquidity on empty pools and the >30%
            // ROI phantom guard rejects degenerate state.
            // Balancer stable (amp-deep) and Maverick stay exempt. DODO PMM pools carry
            // real reserves, so a $25K floor cleanly drops the degenerate near-zero pools
            // that produced the prior phantom while admitting healthy DODO venues (the
            // liquid WETH/USDC DODO pool is ~$700K TVL).
            match &p.snapshot {
                // LB concentrates liquidity in discrete bins (like Maverick), so a
                // modest-TVL pool is still low-impact near its active bin — the x*y=k
                // $100K floor wrongly drops them, and we have no TVL computation for them
                // yet. Exempt; the kernel returns None on empty bins and the phantom
                // guard rejects degenerate state.
                SnapshotState::Maverick(_) | SnapshotState::Lb(_) => true,
                // Balancer stable / Curve StableSwap-NG / DODO are amplified-stable or
                // PMM (low impact at modest TVL), so the volatile $100K floor wrongly
                // drops them — but a $25K stable floor cleanly removes the deprecated
                // dust pools. This is load-bearing: an abandoned Balancer dust pool
                // ($12 TVL) with a STALE rate provider (e.g. WETH scaling implying
                // $3,871) otherwise produces a ~1.85x phantom quote that spams the >30%
                // ROI guard and crowds out real Balancer/Curve arbs. `pool_notional_usd`
                // values these decimal-correctly from the raw balances × our marks
                // (ignoring the stale scaling factor), so dust is dropped at boot.
                SnapshotState::BalancerStable(_)
                | SnapshotState::Curve(_)
                | SnapshotState::Dodo(_) => pool_notional_usd(p) >= stable_pool_floor_usd,
                _ => pool_notional_usd(p) >= min_pool_notional_usd,
            }
        })
        .collect();
    info!(
        before = before_floor,
        after = validated.len(),
        dropped = before_floor - validated.len(),
        min_notional_usd = min_pool_notional_usd as u64,
        stable_floor_usd = stable_pool_floor_usd as u64,
        "§5.5 liquidity floor applied (drop near-dead pools)"
    );

    // ── Phase 2 (expensive, SURVIVORS ONLY): fetch V3/V4 tick windows ──────────────
    //
    // Phase 1 snapshotted every validated pool with cheap state (V2 reserves / V3+V4
    // slot0+liquidity) and an EMPTY `initialised_ticks` window. The §5.5 floor above
    // then dropped the ~1,600 near-dead pools. Only NOW — for the ~280 survivors — do
    // we pay for the per-pool tick window (`ticks(int24)` / StateView `getTickInfo`),
    // which dominated the old ~9-minute boot when it was fetched for every discovered
    // pool before the floor. The kernel needs these ticks to quote, so a survivor
    // without a populated window can't produce candidates.
    //
    // V3-family survivors (Uniswap/Sushi/Pancake V3, Algebra, Kyber — all use the V3
    // `ticks(int24)` ABI) batch through `fetch_v3_windows_batched` (one aggregate3
    // multicall per chunk). V4 survivors read by poolId via the StateView lens, which
    // has no plain pool address, so they use per-pool `fetch_v4_tick_window` (there are
    // only ~30, so the sequential cost is negligible).
    {
        let tick_width = engine_tick_width_from_env();

        // Collect indices of V3-family (non-V4) survivors and their (addr, centre, spacing).
        let mut v3_idx: Vec<usize> = Vec::new();
        let mut v3_inputs: Vec<([u8; 20], i32, i32)> = Vec::new();
        // V4 + Kyber survivors handled per-pool below.
        let mut v4_idx: Vec<usize> = Vec::new();
        let mut kyber_idx: Vec<usize> = Vec::new();
        for (i, p) in validated.iter().enumerate() {
            if let SnapshotState::V3(s) = &p.snapshot {
                // Route by family via the tested pure classifier (so Algebra/Kyber can't
                // silently fall into the wrong loader on a future refactor).
                match phase2_window_route(p.d.family, true) {
                    Phase2WindowRoute::PerPoolV4 => v4_idx.push(i),
                    Phase2WindowRoute::PerPoolKyber => kyber_idx.push(i),
                    Phase2WindowRoute::BatchedV3 => {
                        v3_idx.push(i);
                        v3_inputs.push((p.d.address, s.tick, s.tick_spacing.max(1)));
                    }
                    Phase2WindowRoute::None => {}
                }
            }
        }

        // Batched V3-family windows, chunked to keep each aggregate3 a sane size.
        // Starting batch size (pools per aggregate3). ADAPTIVE below: a provider that rejects a
        // large request body (HTTP 413) or result set halves the batch and retries down to a
        // single pool, same discipline as the discovery getLogs fetcher — so the snapshot is
        // robust to ANY provider's body cap (observed 2026-07-10: lava 413s at 120 pools ×
        // tick_width sub-calls; per-CHUNK fallback to per-pool would take hours). Env override
        // `V3_WINDOW_BATCH_POOLS` seeds a smaller start to skip the first doomed attempts.
        let v3_window_batch: usize = std::env::var("V3_WINDOW_BATCH_POOLS")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(120);
        let mut v3_windows_populated = 0usize;
        let mut v3_ticks_total = 0usize;
        // Dashboard: this V3 tick-window fetch is the dominant snapshot RPC cost — drive the
        // "snapshot" progress bar off it (no-op unless DASHBOARD=1).
        if let Some(p) = crate::dashboard::progress() {
            p.complete(crate::dashboard::phase::DISCOVERY);
            p.set(crate::dashboard::phase::SNAPSHOT, 0, v3_inputs.len() as u64);
        }
        // Work stack of [start, len) ranges into v3_inputs. Seeded with v3_window_batch-sized
        // chunks; a failed batch splits in half (LIFO) instead of dropping to per-pool wholesale.
        let mut work: Vec<(usize, usize)> = Vec::new();
        {
            let mut s = 0usize;
            while s < v3_inputs.len() {
                let l = v3_window_batch.min(v3_inputs.len() - s);
                work.push((s, l));
                s += l;
            }
        }
        // Progress ticks off cumulative pools whose window has resolved (any outcome).
        let mut v3_done = 0usize;
        let mut apply_window = |vi: usize, win: Vec<engine_rpc::V3TickSnapshot>,
                                pop: &mut usize, tot: &mut usize| {
            if let SnapshotState::V3(s) = &mut validated[vi].snapshot {
                if !win.is_empty() {
                    *pop += 1;
                    *tot += win.len();
                }
                s.initialised_ticks = win
                    .into_iter()
                    .map(|t| TickInfo {
                        tick: t.tick,
                        liquidity_net: t.liquidity_net,
                    })
                    .collect();
            }
        };
        while let Some((start, len)) = work.pop() {
            let chunk = &v3_inputs[start..start + len];
            match fetch_v3_windows_batched(
                rpc_client,
                &MULTICALL3_ARBITRUM,
                chunk,
                tick_width,
                block_tag,
            ) {
                Ok(windows) => {
                    for (j, win) in windows.into_iter().enumerate() {
                        apply_window(
                            v3_idx[start + j],
                            win,
                            &mut v3_windows_populated,
                            &mut v3_ticks_total,
                        );
                    }
                    v3_done += len;
                }
                Err(e) if len > 1 => {
                    // Too-large body / result set: halve and retry both halves (LIFO).
                    let half = len / 2;
                    warn!(?e, start, len, new = half, "phase-2 tick-window batch too large — halving");
                    work.push((start + half, len - half));
                    work.push((start, half));
                }
                Err(e) => {
                    // Single pool still failed after halving — direct per-pool call (last resort),
                    // then leave the window empty if even that fails (fail-open, logged).
                    let (addr, centre, spacing) = v3_inputs[start];
                    match fetch_v3_tick_window(
                        rpc_client,
                        &addr,
                        centre,
                        spacing.max(1),
                        tick_width,
                        block_tag,
                    ) {
                        Ok(win) => apply_window(
                            v3_idx[start],
                            win,
                            &mut v3_windows_populated,
                            &mut v3_ticks_total,
                        ),
                        Err(e2) => {
                            warn!(pool = ?addr, ?e, ?e2, "phase-2 tick window failed even per-pool; window left empty");
                        }
                    }
                    v3_done += 1;
                }
            }
            if let Some(p) = crate::dashboard::progress() {
                p.set(
                    crate::dashboard::phase::SNAPSHOT,
                    v3_done as u64,
                    v3_inputs.len() as u64,
                );
            }
        }

        // Kyber Elastic survivors: per-pool LINKED-LIST walk ([AUD-012] — see PerPoolKyber).
        // 32 nodes per side covers ~1-ETH-deep quotes on the live WETH/USDC.e pool; an
        // exhausted walk only ever UNDER-quotes (kernel clamps at known liquidity).
        const KYBER_TICKS_PER_SIDE: usize = 32;
        let mut kyber_lists_populated = 0usize;
        for &vi in &kyber_idx {
            let addr = validated[vi].d.address;
            match engine_rpc::fetch_kyber_initialized_ticks(
                rpc_client,
                &addr,
                block_tag,
                KYBER_TICKS_PER_SIDE,
            ) {
                Ok(list) => {
                    if let SnapshotState::V3(s) = &mut validated[vi].snapshot {
                        if !list.is_empty() {
                            kyber_lists_populated += 1;
                        }
                        s.initialised_ticks = list
                            .into_iter()
                            .map(|(tick, liquidity_net)| TickInfo {
                                tick,
                                liquidity_net,
                            })
                            .collect();
                    }
                }
                Err(e) => {
                    warn!(pool = ?addr, ?e, "kyber initialized-tick list walk failed; ticks left empty");
                }
            }
        }
        if !kyber_idx.is_empty() {
            info!(
                kyber_survivors = kyber_idx.len(),
                kyber_lists_populated, "phase-2 Kyber linked-list tick walk complete"
            );
        }

        // V4 survivors: per-pool StateView window (bitmap-driven). Only ~30 of these.
        let v4_state_view = parse_address(&cfg.discovery.v4_state_view).ok();
        let mut v4_windows_populated = 0usize;
        let mut v4_ticks_total = 0usize;
        for &vi in &v4_idx {
            let addr = validated[vi].d.address;
            let (Some(pool_id), Some(state_view)) =
                (v4_pool_id_by_addr.get(&addr).copied(), v4_state_view)
            else {
                continue;
            };
            let centre = if let SnapshotState::V3(s) = &validated[vi].snapshot {
                s.tick
            } else {
                continue;
            };
            let spacing = if let SnapshotState::V3(s) = &validated[vi].snapshot {
                s.tick_spacing.max(1)
            } else {
                1
            };
            match fetch_v4_tick_window(
                rpc_client,
                &state_view,
                &pool_id,
                centre,
                spacing,
                tick_width,
                block_tag,
            ) {
                Ok(win) => {
                    if let SnapshotState::V3(s) = &mut validated[vi].snapshot {
                        if !win.is_empty() {
                            v4_windows_populated += 1;
                            v4_ticks_total += win.len();
                        }
                        s.initialised_ticks = win
                            .into_iter()
                            .map(|t| TickInfo {
                                tick: t.tick,
                                liquidity_net: t.liquidity_net,
                            })
                            .collect();
                    }
                }
                Err(e) => {
                    warn!(pool = ?addr, ?e, "phase-2 V4 tick window failed; window left empty");
                }
            }
        }
        info!(
            v3_survivors = v3_idx.len(),
            v3_windows_populated,
            v3_ticks_total,
            v4_survivors = v4_idx.len(),
            v4_windows_populated,
            v4_ticks_total,
            tick_width,
            "§5.4 gate 3 phase-2 complete (survivor tick windows populated)"
        );
    }

    // §6.1 route-winnability classification (hub→LST re-weight). Data-driven from
    // config symbols / risk_class so it needs no hardcoded address table. Used to seed
    // `route_winnability` per pool below; the boot score embeds it and the live scorer
    // carries it forward, while `realized_inclusion_rate` self-corrects once live.
    let token_role: ahash::AHashMap<[u8; 20], engine_discovery::TokenRole> = {
        let lst = ["wsteth", "weeth", "reth", "ezeth", "sfrxeth"];
        let mut m = ahash::AHashMap::default();
        for t in &cfg.tokens {
            let Ok(addr) = parse_address(&t.address) else {
                continue;
            };
            let sym = t.symbol.to_ascii_lowercase();
            let role = if sym == "weth" {
                engine_discovery::TokenRole::Weth
            } else if lst.contains(&sym.as_str()) {
                engine_discovery::TokenRole::Lst
            } else if t.risk_class == "centralised_stable" {
                engine_discovery::TokenRole::Stable
            } else {
                engine_discovery::TokenRole::Other
            };
            m.insert(addr, role);
        }
        m
    };

    // §6.1 initial score. Without live telemetry yet, seed `volume_ewma` from on-chain
    // liquidity/reserves (a strong cold-start proxy) and leave the EWMA terms at unit
    // baselines. The background scorer (§6.2) refines this online every 5–30 min.
    let mut scored: Vec<(PoolWithSnapshot, u64)> = validated
        .into_iter()
        .map(|p| {
            let mut inputs = PoolScoreInputs::unit_baseline();
            inputs.volume_ewma = match &p.snapshot {
                SnapshotState::V2(s) => v2_seed_volume(&s.reserves),
                SnapshotState::V3(s) => v3_seed_volume(s.liquidity),
                SnapshotState::Dodo(s) => {
                    // Seed off log2(base_reserve + quote_reserve). DODO targets/reserves
                    // are in native units, often 1e18, so this maps to the same scale as
                    // V2/V3 seeds.
                    let bytes =
                        (s.base_reserve.saturating_add(s.quote_reserve)).to_be_bytes::<32>();
                    let mut high = [0u8; 16];
                    high.copy_from_slice(&bytes[16..32]);
                    let v = u128::from_be_bytes(high);
                    v3_seed_volume(v)
                }
                SnapshotState::Maverick(s) => {
                    let bytes = (s.reserve0_d18.saturating_add(s.reserve1_d18)).to_be_bytes::<32>();
                    let mut high = [0u8; 16];
                    high.copy_from_slice(&bytes[16..32]);
                    let v = u128::from_be_bytes(high);
                    v3_seed_volume(v)
                }
                SnapshotState::BalancerStable(slot) => {
                    let total: u128 = (0..slot.kernel.n)
                        .map(|k| slot.kernel.balances[k])
                        .fold(0u128, |a, b| a.saturating_add(b));
                    v3_seed_volume(total)
                }
                SnapshotState::BalancerWeighted(slot) => {
                    // Seed off summed 1e18-scaled balances (downscaled to a u128 proxy). Weighted
                    // balances are already 1e18-domain; shift to ~native magnitude for the seed.
                    let total: u128 = (0..slot.n_tokens as usize)
                        .map(|k| {
                            let b: u128 = (slot.balances_d18[k]
                                / alloy_primitives::U256::from(1_000_000_000_000u64))
                            .try_into()
                            .unwrap_or(u128::MAX);
                            b
                        })
                        .fold(0u128, |a, b| a.saturating_add(b));
                    v3_seed_volume(total)
                }
                SnapshotState::Curve(slot) => {
                    let total: u128 = (0..slot.kernel.n_coins)
                        .map(|k| slot.kernel.balances[k])
                        .fold(0u128, |a, b| a.saturating_add(b));
                    v3_seed_volume(total)
                }
                SnapshotState::CurveCrypto(slot) => {
                    // Sum the U256 coin balances; downscale to a u128 magnitude proxy (low 128 bits —
                    // CryptoSwap balances are native-unit and fit). `balances()` dispatches 2-/3-coin.
                    let bals = slot.balances();
                    let sum = (0..slot.n_coins as usize)
                        .map(|k| bals[k])
                        .fold(alloy_primitives::U256::ZERO, |a, b| a.saturating_add(b));
                    let bytes = sum.to_be_bytes::<32>();
                    let mut high = [0u8; 16];
                    high.copy_from_slice(&bytes[16..32]);
                    v3_seed_volume(u128::from_be_bytes(high))
                }
                SnapshotState::Lb(s) => {
                    // Seed off the active-bin reserves (reserve_x + reserve_y). LB
                    // reserves are native-unit u128 like V2/V3, so reuse v3_seed_volume.
                    v3_seed_volume(s.reserve_x.saturating_add(s.reserve_y))
                }
            };
            let r0 = token_role
                .get(&p.d.token0)
                .copied()
                .unwrap_or(engine_discovery::TokenRole::Other);
            let r1 = token_role
                .get(&p.d.token1)
                .copied()
                .unwrap_or(engine_discovery::TokenRole::Other);
            inputs.route_winnability =
                engine_discovery::route_winnability_q32(r0, r1, cfg.colocated);
            let out = score_pool(&inputs);
            (p, out.pool_score)
        })
        .collect();
    // Sort by score descending — top-N become the hot tier (§6.2 / §5.5).
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    let hot_cap = cfg.discovery.hot_pool_cap.max(1);
    if scored.len() > hot_cap {
        info!(
            full = scored.len(),
            hot_cap, "§6.2 election: trimming to hot tier cap"
        );
        scored.truncate(hot_cap);
    }

    // Now actually register the elected pools in the universe.
    let mut skipped_pools = 0usize;
    // WI-02 boundary 3: pools excluded because their family is INACTIVE under the boot
    // RuntimePolicy (counted separately so the exclusion is visible, never silent).
    let mut policy_excluded_pools = 0usize;
    // [v4-native-eth fix 2026-07-01] V4 pools whose stored poolId the executor recompute can't
    // reproduce (native-ETH keys from pre-fix caches) — dropped, counted, summarized below.
    let mut v4_unexecutable_skipped = 0usize;
    let mut registered_by_family: ahash::AHashMap<&'static str, u32> = ahash::AHashMap::default();
    // Per-token resident tally (breadth observability: which of the 20 tokens have resident
    // pools at all). Keyed by token address; logged by symbol after the loop.
    let mut registered_by_token: ahash::AHashMap<[u8; 20], u32> = ahash::AHashMap::default();
    // §6.1 initial seed: parallel to the registered pool order, so PoolId.0 == seed idx.
    let mut volume_seeds: Vec<u64> = Vec::with_capacity(scored.len());
    for (p, score) in scored.iter() {
        // WI-02 boundary 3 (universe/state stores): an inactive family never registers a
        // store slot. Load-bearing beyond the discovery gate — the discovery CACHE seeds
        // pools from disk and would otherwise bypass the venue-loop policy filter.
        if !crate::runtime_policy::pool_admitted_to_universe(policy, p.d.family) {
            policy_excluded_pools += 1;
            skipped_pools += 1;
            continue;
        }
        let token0 = match token_id_by_addr.get(&p.d.token0) {
            Some(t) => *t,
            None => {
                skipped_pools += 1;
                continue;
            }
        };
        let token1 = match token_id_by_addr.get(&p.d.token1) {
            Some(t) => *t,
            None => {
                skipped_pools += 1;
                continue;
            }
        };
        let res: Result<engine_state::PoolId, _> = match &p.snapshot {
            SnapshotState::V2(s) => ub.add_v2_pool(
                p.d.address,
                p.d.family,
                token0,
                token1,
                p.venue,
                p.d.fee_bps,
                *s,
            ),
            SnapshotState::V3(s) => {
                // V3-family meta fee: use the snapshot's resolved fee where possible. For
                // Uniswap V3 family pools the discovery record carries the static tier in
                // `fee_pips`; for Algebra V1.9 the discovery record has 0 (fees are dynamic)
                // and the live fee lives in the snapshot. Falling back to snapshot keeps
                // the fee-floor pre-filter accurate on Algebra cycles.
                let fee_pips = if p.d.fee_pips == 0 {
                    s.fee_pips
                } else {
                    p.d.fee_pips
                };
                if p.d.family == PoolKind::UniswapV4 {
                    let Some(pool_id) = v4_pool_id_by_addr.get(&p.d.address).copied() else {
                        warn!(pool = ?p.d.address, "v4 poolId side-map miss during universe registration; skipping");
                        skipped_pools += 1;
                        continue;
                    };
                    // Native-ETH (or otherwise executor-irreproducible) poolId → the route would
                    // always revert "PoolId mismatch" on-chain; refuse registration so it stops
                    // generating doomed candidates. See v4_registration_admissible.
                    if !v4_registration_admissible(&p.d, &pool_id) {
                        v4_unexecutable_skipped += 1;
                        skipped_pools += 1;
                        continue;
                    }
                    ub.add_v4_pool(
                        p.d.address,
                        token0,
                        token1,
                        p.venue,
                        fee_pips,
                        p.d.tick_spacing,
                        pool_id,
                        s.clone(),
                    )
                } else {
                    ub.add_v3_pool(
                        p.d.address,
                        p.d.family,
                        token0,
                        token1,
                        p.venue,
                        fee_pips,
                        // The SNAPSHOT's spacing is the live truth (equal to discovery's for
                        // immutable-spacing families; the LIVE read for mutable Algebra).
                        s.tick_spacing,
                        s.clone(),
                    )
                }
            }
            SnapshotState::Dodo(s) => {
                ub.add_dodo_pool(p.d.address, token0, token1, p.venue, s.fee_ppm, s.clone())
            }
            SnapshotState::Maverick(s) => ub.add_maverick_pool(
                p.d.address,
                token0,
                token1,
                p.venue,
                // Maverick fees are in 1e18; convert to ppm for the route fee-floor
                // pre-filter (lossless because ppm is 1e6, 12 bits less precision).
                {
                    let bigger = s.fee_zto_d18.max(s.fee_otz_d18);
                    let ppm = bigger.saturating_mul(alloy_primitives::U256::from(1_000_000u64))
                        / alloy_primitives::U256::from(1_000_000_000_000_000_000u128);
                    let limbs = ppm.as_limbs();
                    limbs[0] as u32
                },
                s.clone(),
            ),
            SnapshotState::BalancerStable(slot) => ub.add_balancer_stable_pool(
                p.d.address,
                token0,
                token1,
                p.venue,
                // swap_fee_1e18 → ppm (e.g. 1e15 = 0.1% = 1000 ppm) for the fee floor.
                (slot.kernel.swap_fee_1e18 / 1_000_000_000_000u128) as u32,
                slot.clone(),
            ),
            SnapshotState::BalancerWeighted(slot) => ub.add_balancer_weighted_pool(
                p.d.address,
                token0,
                token1,
                p.venue,
                // swap_fee_d18 (1e18) → ppm for the §10.6 fee floor.
                (slot.swap_fee_d18 / alloy_primitives::U256::from(1_000_000_000_000u64))
                    .try_into()
                    .unwrap_or(u32::MAX),
                slot.clone(),
            ),
            SnapshotState::Curve(slot) => ub.add_curve_pool(
                p.d.address,
                p.d.family,
                token0,
                token1,
                p.venue,
                // Curve fee is in 1e10 units; store it verbatim. fast_path/to_bps convert
                // to bps via /1e6 (e.g. 1_000_000 = 1 bps) for the §10.6 fee floor.
                slot.kernel.fee.min(u32::MAX as u64) as u32,
                slot.clone(),
            ),
            SnapshotState::CurveCrypto(slot) => ub.add_curve_crypto_pool(
                p.d.address,
                p.d.family,
                token0,
                token1,
                p.venue,
                // CryptoSwap `mid_fee` is the base fee in the same 1e10 units as Curve's `fee`
                // (the §10.6 to_bps converts CurveCryptoSwap via /1e6 too). Store it verbatim.
                slot.kernel.mid_fee.try_into().unwrap_or(u32::MAX),
                slot.clone(),
            ),
            SnapshotState::Lb(s) => {
                // LB base fee (bps) for the §10.6 fee-floor pre-filter. The kernel's
                // base fee in 1e18 = base_factor * bin_step * 1e10, so:
                //   bps = baseFee_1e18 / 1e14 = base_factor * bin_step / 100.
                // (Variable fee decays with volatility and is resolved per-quote by the
                // kernel; only the static base anchors the pre-filter.) Clamp to >= 1.
                let fee_bps =
                    ((s.base_factor as u128 * s.bin_step as u128 * 10_000) / 1_000_000) as u32;
                let fee_bps = fee_bps.max(1);
                ub.add_lb_pool(
                    p.d.address,
                    token0,
                    token1,
                    p.venue,
                    fee_bps,
                    engine_state::lb_state::LbSlot {
                        kernel: s.clone(),
                        token0: p.d.token0,
                        token1: p.d.token1,
                    },
                )
            }
        };
        match res {
            Ok(_pool_id) => {
                volume_seeds.push(*score);
                *registered_by_family
                    .entry(pool_kind_to_family(p.d.family))
                    .or_insert(0u32) += 1;
                *registered_by_token.entry(p.d.token0).or_insert(0u32) += 1;
                *registered_by_token.entry(p.d.token1).or_insert(0u32) += 1;
            }
            Err(e) => {
                warn!(pool = ?p.d.address, ?e, "universe register failed; skipping");
                skipped_pools += 1;
            }
        }
    }
    info!(
        elected = scored.len(),
        skipped = skipped_pools,
        policy_excluded = policy_excluded_pools,
        v4_unexecutable_skipped,
        "§5.5 hot-tier registration complete"
    );
    // Per-family registered counts (observability: confirms each venue family is wired
    // end-to-end through discovery → snapshot → floor → registration).
    for (fam, n) in &registered_by_family {
        info!(family = %fam, count = n, "registered pools by family");
    }
    // Resident pools per token (legs that touch each token). A token with a low/zero count
    // can never produce candidates — that's the breadth gap.
    {
        let sym: ahash::AHashMap<[u8; 20], &str> = cfg
            .tokens
            .iter()
            .filter_map(|t| {
                parse_address(&t.address)
                    .ok()
                    .map(|a| (a, t.symbol.as_str()))
            })
            .collect();
        let mut by_tok: Vec<(&str, u32)> = registered_by_token
            .iter()
            .map(|(a, n)| (*sym.get(a).unwrap_or(&"?"), *n))
            .collect();
        by_tok.sort_by(|a, b| b.1.cmp(&a.1));
        for (s, n) in by_tok {
            info!(token = %s, resident_pool_legs = n, "RESIDENT_BY_TOKEN");
        }
    }

    // Fluid DEX pool registration. Two paths feed the same registrar:
    //   1) **Resolver-based discovery** (preferred) — calls
    //      `DexReservesResolver.getAllPoolAddresses()` (v1) and
    //      `DexResolver.getAllPermissionedDexes()` (v2) to enumerate every
    //      Fluid pool active on Arbitrum at `head_block`. Token-pair filter
    //      runs against the engine allowlist so off-chain noise is dropped.
    //   2) **Manual config list** (`fluid_pools`) — fallback for pools the
    //      operator wants to force-register regardless of resolver state
    //      (e.g. dynamic-fee pools the resolver doesn't enumerate yet).
    // Both paths register `PoolKind::FluidDex`. We attempt a block-pinned v1
    // resolver snapshot at startup so bounded replay has parent-block Fluid
    // state before the first dirty route is evaluated. If the resolver read
    // fails, keep the default-paused placeholder; the background resolver
    // poller / receipt replay can still atomically swap in real state later.
    let mut fluid_registered = 0usize;
    let mut fluid_skipped = 0usize;
    let mut fluid_seen: AHashSet<[u8; 20]> = AHashSet::default();
    // WI-02 boundary 1/3 for the Fluid registrar's three entry paths (resolver discovery,
    // cache seed, manual config list): an inactive FluidDex family registers nothing.
    let fluid_family_active =
        crate::runtime_policy::family_admitted_at_discovery(policy, PoolKind::FluidDex);
    if !fluid_family_active {
        info!("FluidDex INACTIVE under the boot RuntimePolicy; all Fluid registration paths skipped");
    }
    // Identities of every Fluid pool we register this boot, folded back into the discovery cache
    // (second save, below) so the next offline/cold-cache boot can re-seed Fluid without an RPC.
    let mut fluid_for_cache: Vec<DiscoveredPool> = Vec::new();

    // Wrapped-native (WETH) address for normalizing Fluid's native-ETH sentinel
    // legs. Resolved from the configured token list by symbol; if absent, ETH-
    // paired Fluid pools simply won't match the allowlist (non-ETH pools still
    // register).
    let native_wrapped: [u8; 20] = cfg
        .tokens
        .iter()
        .find(|t| t.symbol.eq_ignore_ascii_case("WETH"))
        .and_then(|t| parse_address(&t.address).ok())
        .unwrap_or([0u8; 20]);

    // 1) Resolver-based discovery. We only run this when an RPC endpoint is
    //    configured AND the operator supplied at least one resolver address.
    //    The discovery call is fail-open — a reverting / unreachable resolver
    //    is logged and skipped; UniV3/Camelot continue to operate.
    if let Some(rpc_client) = rpc.as_ref().filter(|_| fluid_family_active) {
        // v1/T1 — DexReservesResolver.
        if !cfg.discovery.fluid_v1_resolver.is_empty() {
            match parse_address(&cfg.discovery.fluid_v1_resolver) {
                Ok(resolver_addr) => {
                    let resolver = FluidResolverConfig {
                        address: resolver_addr,
                        family: PoolKind::FluidDex,
                        variant: FluidVariant::V1,
                        native_wrapped,
                    };
                    match discover_fluid_v1_pools(
                        rpc_client,
                        &resolver,
                        head_block,
                        &token_allowlist,
                    ) {
                        Ok(discovered) => {
                            info!(
                                count = discovered.len(),
                                "Fluid v1 resolver discovery returned pools"
                            );
                            for d in discovered {
                                if !fluid_seen.insert(d.address) {
                                    continue;
                                }
                                let t0 = match token_id_by_addr.get(&d.token0) {
                                    Some(t) => *t,
                                    None => {
                                        fluid_skipped += 1;
                                        continue;
                                    }
                                };
                                let t1 = match token_id_by_addr.get(&d.token1) {
                                    Some(t) => *t,
                                    None => {
                                        fluid_skipped += 1;
                                        continue;
                                    }
                                };
                                let snapshot = match crate::fluid_poller::refresh_one_v1_pool_at(
                                    rpc_client,
                                    &resolver_addr,
                                    &d.address,
                                    block_tag,
                                ) {
                                    Ok(state) => engine_state::FluidPoolSnapshot::V1(state),
                                    Err(e) => {
                                        warn!(
                                            pool = ?d.address,
                                            ?e,
                                            "fluid v1 startup snapshot failed; registering paused placeholder"
                                        );
                                        engine_state::FluidPoolSnapshot::default()
                                    }
                                };
                                match ub.add_fluid_pool(
                                    d.address,
                                    t0,
                                    t1,
                                    engine_state::VenueId(0),
                                    d.fee_pips,
                                    snapshot,
                                ) {
                                    Ok(_pool_id) => {
                                        fluid_registered += 1;
                                        volume_seeds.push(1u64 << 32);
                                        fluid_for_cache.push(d);
                                    }
                                    Err(e) => {
                                        warn!(pool = ?d.address, ?e, "fluid v1 resolver pool register failed");
                                        fluid_skipped += 1;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(?e, "Fluid v1 resolver discovery errored — proceeding with manual list only");
                        }
                    }
                }
                Err(e) => {
                    warn!(?e, addr = %cfg.discovery.fluid_v1_resolver, "Fluid v1 resolver address malformed; skipping");
                }
            }
        }
        // v2 D3/D4 — DexResolver. Returns 0 today on Arbitrum (no permissionless
        // v2 GA yet); the path is wired so it lights up when Fluid governance
        // flips the switch.
        if !cfg.discovery.fluid_v2_resolver.is_empty() {
            match parse_address(&cfg.discovery.fluid_v2_resolver) {
                Ok(resolver_addr) => {
                    let resolver = FluidResolverConfig {
                        address: resolver_addr,
                        family: PoolKind::FluidDex,
                        variant: FluidVariant::V2,
                        native_wrapped,
                    };
                    match discover_fluid_v2_pools(
                        rpc_client,
                        &resolver,
                        head_block,
                        &token_allowlist,
                    ) {
                        Ok(discovered) => {
                            for d in discovered {
                                if !fluid_seen.insert(d.address) {
                                    continue;
                                }
                                let t0 = match token_id_by_addr.get(&d.token0) {
                                    Some(t) => *t,
                                    None => {
                                        fluid_skipped += 1;
                                        continue;
                                    }
                                };
                                let t1 = match token_id_by_addr.get(&d.token1) {
                                    Some(t) => *t,
                                    None => {
                                        fluid_skipped += 1;
                                        continue;
                                    }
                                };
                                let snapshot = engine_state::FluidPoolSnapshot::default();
                                match ub.add_fluid_pool(
                                    d.address,
                                    t0,
                                    t1,
                                    engine_state::VenueId(0),
                                    d.fee_pips,
                                    snapshot,
                                ) {
                                    Ok(_pool_id) => {
                                        fluid_registered += 1;
                                        volume_seeds.push(1u64 << 32);
                                        fluid_for_cache.push(d);
                                    }
                                    Err(e) => {
                                        warn!(pool = ?d.address, ?e, "fluid v2 resolver pool register failed");
                                        fluid_skipped += 1;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(?e, "Fluid v2 resolver discovery errored");
                        }
                    }
                }
                Err(e) => {
                    warn!(?e, addr = %cfg.discovery.fluid_v2_resolver, "Fluid v2 resolver address malformed; skipping");
                }
            }
        }
    }

    // 1b) Offline / no-RPC fallback. The resolver path above only runs with a live RPC. On an
    //     offline boot (replay corpora, cold-cache certification, no egress) the resolver can't be
    //     reached, so Fluid would otherwise be entirely absent from the universe — no Fluid routes
    //     enumerated, the bot blind to every Fluid arb. Re-seed the Fluid pool identities the
    //     previous live boot folded into the discovery cache; register them with a paused/default
    //     snapshot (the receipt-log replay / resolver poller atomically swaps in real state later).
    if rpc.is_none() && fluid_family_active {
        if let Some(c) = &cache {
            let cached_fluid = c.fluid_pools.clone();
            if !cached_fluid.is_empty() {
                info!(
                    count = cached_fluid.len(),
                    "no RPC this boot — seeding Fluid pools from discovery cache"
                );
            }
            for d in cached_fluid {
                if d.family != PoolKind::FluidDex {
                    continue;
                }
                if !fluid_seen.insert(d.address) {
                    continue;
                }
                let t0 = match token_id_by_addr.get(&d.token0) {
                    Some(t) => *t,
                    None => {
                        fluid_skipped += 1;
                        continue;
                    }
                };
                let t1 = match token_id_by_addr.get(&d.token1) {
                    Some(t) => *t,
                    None => {
                        fluid_skipped += 1;
                        continue;
                    }
                };
                let snapshot = engine_state::FluidPoolSnapshot::default();
                match ub.add_fluid_pool(
                    d.address,
                    t0,
                    t1,
                    engine_state::VenueId(0),
                    d.fee_pips,
                    snapshot,
                ) {
                    Ok(_pool_id) => {
                        fluid_registered += 1;
                        volume_seeds.push(1u64 << 32);
                        fluid_for_cache.push(d);
                    }
                    Err(e) => {
                        warn!(pool = ?d.address, ?e, "fluid cache-seed register failed");
                        fluid_skipped += 1;
                    }
                }
            }
        }
    }

    // 2) Manual fluid_pools config list — addresses the operator wants to
    //    force-register regardless of resolver enumeration. Duplicate
    //    addresses (already pulled in via resolver) are silently skipped.
    //    WI-02: the manual list cannot override an INACTIVE family policy.
    let manual_fluid_pools: &[engine_config::FluidPoolEntry] = if fluid_family_active {
        &cfg.fluid_pools
    } else {
        &[]
    };
    for fp in manual_fluid_pools {
        let addr = match parse_address(&fp.address) {
            Ok(a) => a,
            Err(_) => {
                warn!(pool = %fp.address, "fluid pool: malformed address; skipping");
                fluid_skipped += 1;
                continue;
            }
        };
        if !fluid_seen.insert(addr) {
            continue;
        }
        let t0_addr = match parse_address(&fp.token0) {
            Ok(a) => a,
            Err(_) => {
                fluid_skipped += 1;
                continue;
            }
        };
        let t1_addr = match parse_address(&fp.token1) {
            Ok(a) => a,
            Err(_) => {
                fluid_skipped += 1;
                continue;
            }
        };
        let token0 = match token_id_by_addr.get(&t0_addr) {
            Some(t) => *t,
            None => {
                warn!(pool = %fp.address, "fluid pool: token0 not in allowlist; skipping");
                fluid_skipped += 1;
                continue;
            }
        };
        let token1 = match token_id_by_addr.get(&t1_addr) {
            Some(t) => *t,
            None => {
                warn!(pool = %fp.address, "fluid pool: token1 not in allowlist; skipping");
                fluid_skipped += 1;
                continue;
            }
        };
        let venue = engine_state::VenueId(0);
        let snapshot = rpc
            .as_ref()
            .and_then(|rpc_client| {
                parse_address(&cfg.discovery.fluid_v1_resolver)
                    .ok()
                    .map(|resolver| (rpc_client, resolver))
            })
            .and_then(|(rpc_client, resolver)| {
                match crate::fluid_poller::refresh_one_v1_pool_at(
                    rpc_client, &resolver, &addr, block_tag,
                ) {
                    Ok(state) => Some(engine_state::FluidPoolSnapshot::V1(state)),
                    Err(e) => {
                        warn!(
                            pool = %fp.address,
                            ?e,
                            "manual fluid v1 startup snapshot failed; registering paused placeholder"
                        );
                        None
                    }
                }
            })
            .unwrap_or_else(engine_state::FluidPoolSnapshot::default);
        match ub.add_fluid_pool(addr, token0, token1, venue, fp.fee_pips, snapshot) {
            Ok(_pool_id) => {
                fluid_registered += 1;
                volume_seeds.push(1u64 << 32);
                fluid_for_cache.push(DiscoveredPool {
                    address: addr,
                    family: PoolKind::FluidDex,
                    token0: t0_addr,
                    token1: t1_addr,
                    fee_pips: fp.fee_pips,
                    fee_bps: 0,
                    tick_spacing: 0,
                    created_block: head_block,
                });
            }
            Err(e) => {
                warn!(pool = %fp.address, ?e, "fluid pool register failed; skipping");
                fluid_skipped += 1;
            }
        }
    }

    if fluid_registered > 0 || fluid_skipped > 0 {
        info!(
            registered = fluid_registered,
            skipped = fluid_skipped,
            resolver_v1 = %cfg.discovery.fluid_v1_resolver,
            resolver_v2 = %cfg.discovery.fluid_v2_resolver,
            "Fluid DEX pool registration complete (resolver + manual; default-paused snapshots)"
        );
    }

    // Second cache save: fold the Fluid pool identities discovered this boot back into the
    // on-disk discovery cache so the next offline/cold-cache boot (no RPC) can re-seed them via
    // the 1b fallback above. This is what closes the "fluid_pools: [] in the cache ⇒ Fluid-blind
    // offline" gap. Non-fatal — a write failure just means the next offline boot lacks Fluid.
    if let (Some(p), Some(mut c)) = (cache_path.as_ref(), discovery_cache_to_resave.take()) {
        if !fluid_for_cache.is_empty() {
            c.fluid_pools = fluid_for_cache.clone();
            match c.save(p) {
                Ok(()) => info!(
                    fluid_cached = c.fluid_pools.len(),
                    path = %p.display(),
                    "discovery cache re-saved with Fluid pools"
                ),
                Err(e) => {
                    warn!(error = %e, "failed to re-save discovery cache with Fluid (non-fatal)")
                }
            }
        }
    }

    let telemetry = std::sync::Arc::new(engine_discovery::PoolTelemetry::new(volume_seeds));

    // 5. Enumerate routes per topology config.
    let topology = cfg.topology.clone();
    // Tier-stepped boot progress: one tick per enabled tier (plus a live status line) so the
    // boot bar moves through a long deep-hop enumeration instead of sitting at 0%, which
    // reads as a hang on the console dashboard.
    let tier_total = (topology.enable_two_hop as u64)
        + (topology.enable_triangle as u64)
        + (4u8..=8).filter(|&k| topology.enabled(k)).count() as u64;
    let enum_tick = |done: u64, status: String| {
        if let Some(p) = crate::dashboard::progress() {
            p.set(crate::dashboard::phase::ENUMERATE, done, tier_total.max(1));
            p.set_status(status);
        }
    };
    if let Some(p) = crate::dashboard::progress() {
        p.complete(crate::dashboard::phase::SNAPSHOT);
    }
    enum_tick(0, String::new());
    // WI-02 boundary 4 (route graph): per-pool enumeration eligibility = the pool's family
    // is ACTIVE under the boot policy AND both its tokens carry a REAL price anchor
    // (P1-08: an unpriced token is excluded BEFORE routes are built — no route may be
    // sized/valued through it).
    let route_pool_eligibility: Vec<bool> = {
        let pools = ub.pools();
        (0..pools.len())
            .map(|i| {
                let m = pools.meta(engine_state::PoolId(i as u32));
                crate::runtime_policy::route_pool_eligible(
                    policy,
                    m.kind,
                    priced_token_ids.contains(&m.token0),
                    priced_token_ids.contains(&m.token1),
                )
            })
            .collect()
    };
    let route_pools_excluded = route_pool_eligibility.iter().filter(|e| !**e).count();
    if route_pools_excluded > 0 {
        info!(
            excluded = route_pools_excluded,
            of = route_pool_eligibility.len(),
            "WI-02 route boundary: pools excluded from enumeration (inactive family or unpriced token)"
        );
    }
    // PB-PATHS: the enumerated route TABLE is a pure function of the pool set + token set +
    // topology config + WI-02 policy/eligibility. Skip the costly DFS enumeration by loading
    // a fingerprint-gated compiled artifact; rebuild + persist on any miss. The score-ordered
    // CSR below is NOT cached (it uses live volume seeds) and is always rebuilt fresh.
    // P1-09: the policy record hash + eligibility mask are folded into the fingerprint, so a
    // route artifact compiled under a DIFFERENT family policy can never be loaded.
    let route_graph_fp = route_graph_fingerprint(
        ub.pools(),
        cfg.tokens.len(),
        &topology,
        &policy.record_hash,
        &route_pool_eligibility,
    );
    let route_cache_path = route_graph_cache_path();
    let force_route_rebuild = std::env::var("REBUILD_ROUTE_GRAPH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let route_build_t0 = std::time::Instant::now();
    let loaded_route_table = if force_route_rebuild {
        None
    } else {
        engine_routes::compiled::CompiledRouteGraph::load(&route_cache_path, route_graph_fp)
            .map(|g| g.into_route_table())
    };
    let route_table = if let Some(rt) = loaded_route_table {
        info!(
            routes = rt.len(),
            fp = route_graph_fp,
            load_ms = route_build_t0.elapsed().as_millis() as u64,
            path = %route_cache_path.display(),
            "PB-PATHS: loaded compiled route table from cache (enumeration skipped)"
        );
        if let Some(p) = crate::dashboard::progress() {
            p.complete(crate::dashboard::phase::ENUMERATE);
        }
        rt
    } else {
        let mut route_table = RouteTable::new();
        let mut tier_done = 0u64;
        let mut two_hop_routes = 0usize;
        let mut triangle_routes = 0usize;
        // WI-02: each tier requires BOTH the topology switch and the policy route-shape bit
        // (default = all shapes = byte-identical); enumeration is policy/eligibility-gated.
        let shapes = &policy.active_route_shapes;
        if topology.enable_two_hop && shapes.contains_index(0) {
            enum_tick(tier_done, "enumerate 2-hop cycles".to_string());
            enumerate_two_hop_routes_gated(ub.pools(), &route_pool_eligibility, &mut route_table);
            two_hop_routes = route_table.len();
            tier_done += 1;
        }
        if topology.enable_triangle && shapes.contains_index(1) {
            enum_tick(
                tier_done,
                format!("enumerate triangles · {} routes so far", route_table.len()),
            );
            let before = route_table.len();
            enumerate_triangles_gated(ub.pools(), &route_pool_eligibility, &mut route_table);
            triangle_routes = route_table.len() - before;
            tier_done += 1;
        }
        let mut deep_counts: [usize; 5] = [0; 5]; // 4..=8 hops
        let mut deep_capped: [bool; 5] = [false; 5];
        for (idx, k) in (4usize..=8).enumerate() {
            if !topology.enabled(k as u8) || !shapes.contains_index(k - 2) {
                continue;
            }
            enum_tick(
                tier_done,
                format!(
                    "enumerate {k}-hop cycles · {} routes so far",
                    route_table.len()
                ),
            );
            let before = route_table.len();
            // Hard ceiling: per-tier cap is added to the running route_table; the cap
            // applies to the *total* routes in the table, so we offset by current size.
            let cap = before.saturating_add(topology.max_routes_per_deep_tier);
            let hit_cap = enumerate_k_hop_cycles_gated(
                ub.pools(),
                &route_pool_eligibility,
                k,
                cap,
                &mut route_table,
            );
            deep_counts[idx] = route_table.len() - before;
            deep_capped[idx] = hit_cap;
            tier_done += 1;
        }
        enum_tick(
            tier_done,
            format!("{} routes enumerated", route_table.len()),
        );
        info!(
            two_hop = two_hop_routes,
            triangle = triangle_routes,
            four_hop = deep_counts[0],
            four_hop_capped = deep_capped[0],
            five_hop = deep_counts[1],
            five_hop_capped = deep_capped[1],
            six_hop = deep_counts[2],
            six_hop_capped = deep_capped[2],
            seven_hop = deep_counts[3],
            seven_hop_capped = deep_capped[3],
            eight_hop = deep_counts[4],
            eight_hop_capped = deep_capped[4],
            total = route_table.len(),
            "routes enumerated"
        );

        // 6. Mark every enumerated route as hot for Phase-1 boot. Promotion / demotion runs
        //    continuously on the background core once live.
        for id in 0..route_table.len() {
            route_table.get_mut(engine_state::RouteId(id as u32)).hot = true;
        }
        // PB-PATHS: persist the freshly enumerated + hot-marked table for the next boot (best-effort).
        match engine_routes::compiled::CompiledRouteGraph::new(route_table.clone())
            .save(&route_cache_path, route_graph_fp)
        {
            Ok(()) => info!(
                routes = route_table.len(),
                fp = route_graph_fp,
                enumerate_and_persist_ms = route_build_t0.elapsed().as_millis() as u64,
                path = %route_cache_path.display(),
                "PB-PATHS: compiled route table persisted"
            ),
            Err(e) => warn!(error = %e, "PB-PATHS: failed to persist route table (non-fatal)"),
        }
        route_table
    };

    // 7. Build CSR pool → hot route index, *score-ordered* + *dispatch-filtered*.
    //
    // Plan §6.2 pre-cull: route_score = sum of touched-pool initial volume seeds,
    // sort descending so cap-N picks highest-value routes per dirty event.
    //
    // CRITICAL FILTER: enumerated routes whose family combination has no live
    // evaluator (e.g. mixed-family triangle: V3+V3+V2 — evaluate_triangle only
    // handles V2/V2/V2 and silently returns) are OMITTED from the CSR. Without
    // this, dispatch-unsupported triangles dominate the cap (53k+ triangles vs
    // 1.7k two-hop in the 7-token universe) and crowd out the V3/V3 two-hop
    // routes that ARE evaluable. Result before this filter: sim_loop firing 27
    // times across 200s with zero V3/V3 evaluator entries — every route in cap
    // was an unsupported triangle.
    let n_pools = scored.len();
    // Enumeration done; CSR/readiness build is the final phase.
    if let Some(p) = crate::dashboard::progress() {
        p.complete(crate::dashboard::phase::ENUMERATE);
        p.set(crate::dashboard::phase::READY, 0, 1);
        p.set_status("build score-ordered CSR · freeze universe".to_string());
    }
    let pool_table = ub.pools();
    let mut route_scores: Vec<u32> = vec![0u32; route_table.len()];
    let mut omitted_unsupported: usize = 0;
    let mut admitted: usize = 0;
    let mut csr = CsrBuilder::new(pool_table.len() as u32);
    for r in route_table.all() {
        if !r.hot {
            continue;
        }
        let fp = RouteFastPath::from_route(r, pool_table);
        if RouteDispatch::from_u8(fp.dispatch) == RouteDispatch::Unsupported {
            omitted_unsupported += 1;
            continue;
        }
        // [detect FIX 2026-07-07] Hop-count-NEUTRAL route score (see `mean_route_heat`): the MEAN
        // per-pool heat, not the SUM. Summing biased the CSR toward deeper routes and starved 2-hop.
        let hop_scores: Vec<u64> = r
            .hops
            .iter()
            .filter(|h| (h.pool.0 as usize) < n_pools)
            .map(|h| scored[h.pool.0 as usize].1)
            .collect();
        route_scores[r.id.0 as usize] = mean_route_heat(&hop_scores);
        for h in &r.hops {
            csr.push(h.pool.0, r.id);
        }
        admitted += 1;
    }
    let pool_to_hot_routes = csr.build_score_ordered(&route_scores);
    info!(
        n_routes_total = route_table.len(),
        admitted, omitted_unsupported, n_pools, "CSR built (score-ordered, dispatch-filtered)"
    );

    // 8. Freeze + SEAL the universe (WI-06 §3.3): `seal()` yields the single
    // `StateWriterPermit` (the sole-writer cradle born in `UniverseBuilder::new`). The
    // universe is `Arc`-wrapped so the `StatePublisher` (built by the caller) can share the
    // SAME resident stores the readers hold; the permit travels to the state thread.
    // `into_parts()` hands out the universe ALREADY `Arc`-wrapped (WI-06 F1 totality) so no
    // owned `&mut Universe` ever exists here to have a resident store replaced before publication.
    let (universe, state_writer_permit) = ub.seal().into_parts();

    // B4: hint the kernel to back the (now-frozen) route table with 2 MB huge pages. The
    // Vec capacity is settled by this point; subsequent reads come from the hot scoring scan
    // (`cycle_marginal_product` over `seen` routes) which benefits from collapsed TLB
    // footprint. Best-effort: false if THP is denied (cgroup / sysctl) — we keep going.
    let thp_routes_ok = route_table.madvise_hugepage();
    info!(
        thp_routes_ok,
        "route table madvise(MADV_HUGEPAGE) — best-effort huge-page backing"
    );

    if let Some(p) = crate::dashboard::progress() {
        p.complete(crate::dashboard::phase::READY);
        p.set_status(String::new()); // boot done — clear the sub-step line
    }
    Ok((
        DetectionContext {
            universe,
            route_table,
            pool_to_hot_routes,
            selectors: SelectorRegistry::canonical(),
            telemetry,
            // WI-02 boundary 5/6: the boot-sealed dense active-family array the quote and
            // submit-encode paths consult (one indexed load; no env read on the hot path).
            family_quote_active: policy.active_families.dense(),
        },
        state_writer_permit,
    ))
}

fn parse_codehash(s: &str) -> Option<[u8; 32]> {
    if s.is_empty() || s.starts_with("REPLACE_WITH_") {
        return None;
    }
    let trimmed = s.trim_start_matches("0x");
    if trimmed.len() != 64 {
        return None;
    }
    let mut buf = [0u8; 32];
    faster_hex::hex_decode(trimmed.as_bytes(), &mut buf).ok()?;
    Some(buf)
}

/// Convert `PoolKind` back to the string used in config (`uniswap_v3`, `sushi_v2`, …).
/// Used to index the `venue_id_for_family` map by family during discovery.
fn pool_kind_to_family(k: PoolKind) -> &'static str {
    match k {
        PoolKind::UniswapV2 => "uniswap_v2",
        PoolKind::SushiV2 => "sushi_v2",
        PoolKind::CamelotV2 => "camelot_v2",
        PoolKind::UniswapV3 => "uniswap_v3",
        PoolKind::SushiV3 => "sushi_v3",
        PoolKind::PancakeV3 => "pancake_v3",
        PoolKind::AlgebraV19 => "algebra_v19",
        PoolKind::CurveStableSwapNg => "curve_stableswap_ng",
        PoolKind::CurveCryptoSwap => "curve_cryptoswap",
        PoolKind::TraderJoeLb => "trader_joe_lb",
        PoolKind::MaverickV2 => "maverick_v2",
        PoolKind::FluidDex => "fluid_dex",
        PoolKind::DodoV2 => "dodo_v2",
        PoolKind::KyberSwapElastic => "kyberswap_elastic",
        PoolKind::SolidlyV3 => "solidly_v3",
        PoolKind::UniswapV4 => "uniswap_v4",
        PoolKind::BalancerStable => "balancer_v2",
        PoolKind::BalancerWeighted => "balancer_weighted",
    }
}

/// Seed `volume_ewma` for V2 pools from log2 of the geometric mean of the two reserves,
/// scaled into Q32.32. This is a cold-start proxy: pools with deeper reserves get a
/// higher score on day-0. The background scorer replaces it with observed flow within
/// the first §6.2 cadence window.
/// Full-range `U256 → f64` (lossy beyond 53 bits, fine for a notional proxy).
fn u256_to_f64(x: alloy_primitives::U256) -> f64 {
    let l = x.as_limbs();
    const P64: f64 = 18_446_744_073_709_551_616.0; // 2^64
    ((l[3] as f64 * P64 + l[2] as f64) * P64 + l[1] as f64) * P64 + l[0] as f64
}

fn v2_seed_volume(r: &V2Reserves256) -> u64 {
    let r0 = r.reserve0;
    let r1 = r.reserve1;
    let bits = bit_len(r0).saturating_add(bit_len(r1)) / 2;
    // Scale: 80 bits ≈ deep V2 pool ($1B TVL); shift to Q32.32 with 1.0 = 32 bits.
    ((bits as u64).saturating_mul(1u64 << 32)).saturating_mul(1)
}

/// Seed `volume_ewma` for V3 pools from `liquidity` (`u128`).
fn v3_seed_volume(liquidity: u128) -> u64 {
    let bits = (128 - liquidity.leading_zeros()) as u64;
    bits.saturating_mul(1u64 << 32)
}

fn bit_len(x: u128) -> u32 {
    128u32.saturating_sub(x.leading_zeros())
}

fn family_to_kind(family: &str) -> Option<PoolKind> {
    match family {
        "uniswap_v2" => Some(PoolKind::UniswapV2),
        "sushi_v2" => Some(PoolKind::SushiV2),
        "camelot_v2" => Some(PoolKind::CamelotV2),
        "uniswap_v3" => Some(PoolKind::UniswapV3),
        "sushi_v3" => Some(PoolKind::SushiV3),
        "pancake_v3" => Some(PoolKind::PancakeV3),
        "camelot_v3" | "algebra_v19" => Some(PoolKind::AlgebraV19),
        "curve_stableswap_ng" => Some(PoolKind::CurveStableSwapNg),
        "curve_cryptoswap" => Some(PoolKind::CurveCryptoSwap),
        "trader_joe_lb" => Some(PoolKind::TraderJoeLb),
        "maverick_v2" => Some(PoolKind::MaverickV2),
        "fluid_dex" => Some(PoolKind::FluidDex),
        "dodo_v2" | "dodo_dvm" | "dodo_dpp" | "dodo_dsp" => Some(PoolKind::DodoV2),
        "kyberswap_elastic" => Some(PoolKind::KyberSwapElastic),
        "solidly_v3" | "chronos_v2" | "ramses_v2" | "velodrome_v2" => Some(PoolKind::SolidlyV3),
        "uniswap_v4" => Some(PoolKind::UniswapV4),
        "balancer_v2" => Some(PoolKind::BalancerStable),
        "balancer_weighted" => Some(PoolKind::BalancerWeighted),
        _ => None,
    }
}

fn parse_address(s: &str) -> Result<[u8; 20], StartupError> {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.len() != 40 {
        return Err(StartupError::Address(s.into()));
    }
    let mut buf = [0u8; 20];
    faster_hex::hex_decode(trimmed.as_bytes(), &mut buf)
        .map_err(|_| StartupError::Address(s.into()))?;
    Ok(buf)
}

/// Decide the outcome of the §5.4 on-chain ERC-20 `decimals()` attestation. Config `decimals` drive
/// ALL amount math (liquidity-floor TVL, profit valuation, q96 pricing), so a CONFIRMED mismatch in
/// live-submit mode is fatal (silent financial loss); detection-only warns and continues. Pure so
/// the fail-closed policy is unit-tested without RPC. [Codex-F9]
fn enforce_decimal_attestation(
    live_submit: bool,
    mismatches: usize,
    checked: usize,
    expected: usize,
) -> Result<()> {
    let unchecked = expected.saturating_sub(checked);
    if mismatches == 0 && unchecked == 0 {
        info!(
            checked,
            "§5.4 token decimal attestation passed (all on-chain decimals match config)"
        );
        return Ok(());
    }
    if live_submit && unchecked > 0 {
        anyhow::bail!(
            "§5.4 token decimal attestation FAILED in live-submit mode: {unchecked} of {expected} \
             tokens were not checked on-chain — refusing to boot (amount math depends on verified \
             token decimals). Fix RPC/token decimals() access or disable live_submit."
        );
    }
    if live_submit {
        // Fail-closed: config decimals drive ALL amount math; a confirmed mismatch would silently
        // mis-scale every quote/profit/TVL computation and could realize a loss in live submission.
        anyhow::bail!(
            "§5.4 token decimal attestation FAILED in live-submit mode: {mismatches} of {checked} \
             tokens have config decimals != on-chain decimals — refusing to boot (amount math would \
             be wrong). Fix the token `decimals` in engine.json or disable live_submit."
        );
    }
    warn!(
        mismatches,
        checked,
        expected,
        unchecked,
        "§5.4 token decimal attestation: mismatch(es) or unchecked token(s) detected — see warnings above \
         (detection-only mode: continuing, but live-submit would fail-closed)"
    );
    Ok(())
}

/// [v4-native-eth fix 2026-07-01] Registration guard: a V4 pool is registrable only if the
/// EXECUTOR's PoolKey recompute (WETH-tokenized, sorted — `MantisExecutor._v4PoolKey`)
/// reproduces the stored poolId. Pre-fix discovery caches contain native-ETH pools (stored id
/// hashed over `currency0 = address(0)`) that can never execute — drop them here, the single
/// choke point both cached and freshly-discovered pools flow through.
fn v4_registration_admissible(d: &DiscoveredPool, stored_pool_id: &[u8; 32]) -> bool {
    engine_discovery::v4_pool_id_for(d.token0, d.token1, d.fee_pips, d.tick_spacing)
        == *stored_pool_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_native_pool_rejected_at_registration_guard() {
        // Real Arbitrum vector (session-2 verification 2026-07-01): the cached native-ETH/USDC
        // pool carries token0=WETH/token1=USDC after discovery mapping, but its STORED poolId is
        // the keccak of the currency0=address(0) key — the executor recompute can never match,
        // so registration must refuse it. A stored id that DOES recompute stays admissible.
        let mut weth = [0u8; 20];
        faster_hex::hex_decode(b"82af49447d8a07e3bd95bd0d56f35241523fbab1", &mut weth).unwrap();
        let mut usdc = [0u8; 20];
        faster_hex::hex_decode(b"af88d065e77c8cc2239327c5edb3a432268e5831", &mut usdc).unwrap();
        let d = DiscoveredPool {
            address: [0x86u8; 20],
            family: PoolKind::UniswapV4,
            token0: weth,
            token1: usdc,
            fee_pips: 500,
            fee_bps: 0,
            tick_spacing: 10,
            created_block: 1,
        };
        let mut stored_native = [0u8; 32];
        faster_hex::hex_decode(
            b"864abca0a6202dba5b8868772308da953ff125b0f95015adbf89aaf579e903a8",
            &mut stored_native,
        )
        .unwrap();
        assert!(
            !v4_registration_admissible(&d, &stored_native),
            "stored native-ETH poolId must be refused (executor can never reproduce it)"
        );
        let consistent = engine_discovery::v4_pool_id_for(weth, usdc, 500, 10);
        assert!(v4_registration_admissible(&d, &consistent));
    }

    #[test]
    fn decimal_attestation_fails_closed_only_in_live_mode() {
        // No mismatch → Ok in every mode.
        assert!(enforce_decimal_attestation(true, 0, 5, 5).is_ok());
        assert!(enforce_decimal_attestation(false, 0, 5, 5).is_ok());
        // Mismatch, detection-only → Ok (warn loudly + keep running).
        assert!(enforce_decimal_attestation(false, 2, 5, 5).is_ok());
        // Mismatch, LIVE-SUBMIT → MUST fail closed (config decimals drive amount math).
        let err = enforce_decimal_attestation(true, 1, 5, 5)
            .expect_err("live-submit must reject a confirmed decimals mismatch");
        assert!(
            err.to_string().contains("decimal"),
            "error should mention decimals: {err}"
        );
    }

    #[test]
    fn decimal_attestation_fails_live_when_no_tokens_were_checked() {
        let err = enforce_decimal_attestation(true, 0, 0, 5)
            .expect_err("live-submit must reject a run with zero successful decimal attestations");
        assert!(
            err.to_string().contains("decimal"),
            "error should mention decimals: {err}"
        );
    }

    #[test]
    fn decimal_attestation_fails_live_when_any_token_was_unchecked() {
        assert!(enforce_decimal_attestation(false, 0, 4, 5).is_ok());
        let err = enforce_decimal_attestation(true, 0, 4, 5)
            .expect_err("live-submit must reject partial decimal attestation");
        assert!(
            err.to_string().contains("not checked"),
            "error should mention unchecked tokens: {err}"
        );
    }

    #[test]
    fn startup_discovery_sanitizer_dedupes_cached_balancer_rows_by_address() {
        let addr = [0x42; 20];
        let mut pools = vec![
            DiscoveredPool {
                address: addr,
                family: PoolKind::BalancerStable,
                token0: [1; 20],
                token1: [2; 20],
                fee_pips: 0,
                fee_bps: 0,
                tick_spacing: 0,
                created_block: 10,
            },
            DiscoveredPool {
                address: addr,
                family: PoolKind::BalancerStable,
                token0: [1; 20],
                token1: [2; 20],
                fee_pips: 0,
                fee_bps: 0,
                tick_spacing: 0,
                created_block: 10,
            },
            DiscoveredPool {
                address: [0x42; 20],
                family: PoolKind::UniswapV3,
                token0: [1; 20],
                token1: [2; 20],
                fee_pips: 500,
                fee_bps: 0,
                tick_spacing: 10,
                created_block: 10,
            },
        ];

        let removed = dedupe_startup_discovered_pools(&mut pools);

        assert_eq!(removed, 1);
        assert_eq!(pools.len(), 2);
        assert_eq!(
            pools
                .iter()
                .filter(|p| p.family == PoolKind::BalancerStable && p.address == addr)
                .count(),
            1
        );
        assert!(
            pools
                .iter()
                .any(|p| p.family == PoolKind::UniswapV3 && p.address == addr),
            "non-Balancer rows with the same address must not be removed"
        );
    }

    #[test]
    fn no_rpc_endpoint_returns_error() {
        // Discovery is mandatory; an offline boot must fail loudly rather than register
        // zero pools silently.
        let cfg = EngineConfig::default_arbitrum_one();
        let policy = crate::runtime_policy::resolve_runtime_policy_with(&|_| None, || {
            panic!("parity proof must not be consulted")
        })
        .expect("dev default policy");
        let err = build_detection_from_config(&cfg, None, BlockTag::Latest, &policy).err();
        assert!(err.is_some(), "expected error when RPC is absent");
        let msg = err.unwrap().to_string();
        assert!(
            msg.contains("RPC") || msg.contains("rpc"),
            "expected RPC error, got: {msg}"
        );
    }

    #[test]
    fn env_flag_opt_in_defaults_off() {
        // Unset → off; only explicit truthy values turn it on.
        std::env::remove_var("TEST_OPT_IN_FLAG");
        assert!(!env_flag_opt_in("TEST_OPT_IN_FLAG"));
        std::env::set_var("TEST_OPT_IN_FLAG", "1");
        assert!(env_flag_opt_in("TEST_OPT_IN_FLAG"));
        std::env::set_var("TEST_OPT_IN_FLAG", "0");
        assert!(!env_flag_opt_in("TEST_OPT_IN_FLAG"));
        std::env::remove_var("TEST_OPT_IN_FLAG");
    }

    #[test]
    fn phase2_v3_family_pools_route_to_batched_window() {
        // Every SPACING-window V3-ABI family (incl. Algebra V1.9 / Camelot V3, which reuses the
        // V3 state shape) must batch through the V3 tick-window fetch in phase-2 — the
        // regression guard that Algebra survivors keep getting their ticks.
        for fam in [
            PoolKind::UniswapV3,
            PoolKind::SushiV3,
            PoolKind::PancakeV3,
            PoolKind::AlgebraV19,
        ] {
            assert_eq!(
                phase2_window_route(fam, true),
                Phase2WindowRoute::BatchedV3,
                "{fam:?} must batch its phase-2 V3 window",
            );
        }
    }

    #[test]
    fn phase2_v4_routes_per_pool() {
        assert_eq!(
            phase2_window_route(PoolKind::UniswapV4, true),
            Phase2WindowRoute::PerPoolV4
        );
    }

    #[test]
    fn phase2_kyber_routes_to_linked_list_walk() {
        // [AUD-012] Kyber Elastic keeps initialized ticks in an on-chain LINKED LIST
        // (`nearestCurrentTick` → `initializedTicks(t)`), NOT at spacing multiples: a window
        // scan loads 0 ticks whenever liquidity sits far from the current tick (proven live —
        // 130k-tick gap). Kyber must walk the list per pool, never the batched window.
        assert_eq!(
            phase2_window_route(PoolKind::KyberSwapElastic, true),
            Phase2WindowRoute::PerPoolKyber
        );
    }

    #[test]
    fn phase2_non_tick_families_need_no_window() {
        // Families that either have no tick window or fetch it fully in phase-1 (LB) must be
        // excluded from phase-2 — otherwise they'd be mis-fed into the V3 batch.
        for fam in [
            PoolKind::UniswapV2,
            PoolKind::SushiV2,
            PoolKind::CamelotV2,
            PoolKind::SolidlyV3, // V2-reserves-backed despite the name
            PoolKind::DodoV2,
            PoolKind::CurveStableSwapNg,
            PoolKind::CurveCryptoSwap,
            PoolKind::MaverickV2,
            PoolKind::FluidDex,
            PoolKind::BalancerStable,
            PoolKind::TraderJoeLb, // full bin window already in phase-1
        ] {
            assert_eq!(
                phase2_window_route(fam, false),
                Phase2WindowRoute::None,
                "{fam:?} must not request a phase-2 V3/V4 window",
            );
        }
    }

    #[test]
    fn phase2_requires_v3_snapshot_flag() {
        // A family that COULD be V3-ABI but wasn't stored as a V3 snapshot must not route to a
        // window fetch (defensive: the snapshot variant is the source of truth).
        assert_eq!(
            phase2_window_route(PoolKind::UniswapV3, false),
            Phase2WindowRoute::None
        );
        assert_eq!(
            phase2_window_route(PoolKind::UniswapV4, false),
            Phase2WindowRoute::None
        );
    }

    #[test]
    fn mean_route_heat_is_hop_count_neutral_no_2hop_starvation() {
        // Q32.32 pool scores for a "hot" and a "cold" pool.
        let hot = 100u64 << 32;
        let cold = 5u64 << 32;
        // (a) equally-hot routes of different length TIE — under the old SUM a 3-hop (300) beat a
        //     2-hop (200) purely for having more hops, which starved 2-hop from the collector.
        assert_eq!(
            mean_route_heat(&[hot, hot]),
            mean_route_heat(&[hot, hot, hot]),
            "hop count must not change the score when per-pool heat is equal"
        );
        // (b) a hot 2-hop must BEAT a 4-hop padded with cold pools — under the old SUM the 4-hop
        //     (hot+hot+cold+cold = 210) outscored the 2-hop (200); 2-hop is ~74% of landed arbs.
        assert!(
            mean_route_heat(&[hot, hot]) > mean_route_heat(&[hot, hot, cold, cold]),
            "a hot 2-hop must outrank a cold-padded 4-hop"
        );
        // (c) mean is the integer average of the per-pool heat.
        assert_eq!(mean_route_heat(&[hot, hot]), 100);
        assert_eq!(mean_route_heat(&[hot, cold]), 52); // (100+5)/2 = 52 (integer)
        // (d) empty route → 0 (defensive).
        assert_eq!(mean_route_heat(&[]), 0);
    }
}
