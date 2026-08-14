//! DETECTION COMPLETENESS — the CSR pool→route index must return EVERY route that
//! touches a dirty pool (no profitable route can ever be missed because the index
//! dropped it).
//!
//! Owner's concern: "every profitable cyclic route reachable from a dirty pool must be
//! found and evaluated." The hot path's first step (pipeline.rs `eval_and_drain`) is a
//! single CSR read — `pool_to_hot_routes.slice_for(dirty_pool)` — to enumerate the
//! candidate routes. If that index ever omits a route that contains the dirty pool, that
//! route is NEVER scored, culled, sized, or evaluated: it is silently invisible. So the
//! index must be PROVABLY complete.
//!
//! This test is 100% offline (pure enumeration + index build — no chain state, no RPC):
//!   1. It builds a route universe from a small set of REAL Arbitrum pool identities
//!      (decoded straight out of `engine/discovery_cache.json`: real addresses, real
//!      token pairs, real families, real fee tiers) wired through the SAME builder the
//!      production startup uses (`UniverseBuilder::add_v2_pool` / `add_v3_pool`).
//!   2. It enumerates every cyclic route with the engine's OWN enumerator
//!      (`enumerate_two_hop_routes` + `enumerate_triangles` + `enumerate_k_hop_cycles`),
//!      exactly as `startup.rs` step 5.
//!   3. It builds the CSR pool→route index the SAME way `startup.rs` step 7 does
//!      (`csr.push(h.pool.0, r.id)` for every hop of every route).
//!   4. THE INVARIANT: for every pool, the set returned by `csr.slice_for(pool)` is
//!      EXACTLY the set of route ids whose hop list contains that pool — verified
//!      against an independent brute-force scan of the full route table. Membership is
//!      bidirectional (no missing routes, no spurious routes).
//!
//! Because the CSR is what the dirty-event path reads to find candidate routes, an exact
//! membership match is the structural guarantee that "no route touching the dirty pool is
//! missed."

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use engine_routes::{
    enumerate_k_hop_cycles, enumerate_triangles, enumerate_two_hop_routes, CsrBuilder, RouteTable,
};
use engine_state::{
    PoolId, PoolKind, RiskClass, RouteId, TokenId, UniverseBuilder, V2Reserves256, V2State,
    V3State, VenueId,
};
use serde_json::Value;

/// Repo root (mirrors the pattern used by the other `*_static` corpus tests).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// One real pool decoded from the discovery cache.
#[derive(Clone, Debug)]
struct RealPool {
    address: [u8; 20],
    kind: PoolKind,
    token0: [u8; 20],
    token1: [u8; 20],
    fee: u32,
    tick_spacing: i32,
}

/// Map the discovery cache's `family` string (the PoolKind variant name, e.g. "UniswapV3")
/// to a `PoolKind`. Only the byte-exact constant-product / sqrt-price families are admitted
/// here — those are the ones whose spot-product rank the cull reasons about, and they are
/// the families this completeness test scaffolds. Anything else returns `None` and is
/// skipped (kept out of the structural subset).
fn family_str_to_v2_v3_kind(family: &str) -> Option<PoolKind> {
    match family {
        "UniswapV2" => Some(PoolKind::UniswapV2),
        "SushiV2" => Some(PoolKind::SushiV2),
        "CamelotV2" => Some(PoolKind::CamelotV2),
        "UniswapV3" => Some(PoolKind::UniswapV3),
        "SushiV3" => Some(PoolKind::SushiV3),
        "PancakeV3" => Some(PoolKind::PancakeV3),
        "AlgebraV19" => Some(PoolKind::AlgebraV19),
        _ => None,
    }
}

fn value_address(value: &Value, field: &str) -> [u8; 20] {
    let array = value
        .get(field)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("discovery cache pool missing {field} array: {value}"));
    assert_eq!(array.len(), 20, "discovery cache {field} must be 20 bytes");
    let mut out = [0u8; 20];
    for (idx, byte) in array.iter().enumerate() {
        out[idx] = byte
            .as_u64()
            .unwrap_or_else(|| panic!("discovery cache {field}[{idx}] not u64: {value}"))
            as u8;
    }
    out
}

fn value_u32(value: &Value, field: &str) -> u32 {
    value
        .get(field)
        .and_then(Value::as_u64)
        .map(|v| v as u32)
        .unwrap_or(0)
}

fn is_v2_kind(k: PoolKind) -> bool {
    matches!(
        k,
        PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2
    )
}

/// Load every V2/V3 pool from the real discovery cache whose BOTH tokens are in `hub`.
/// This yields a dense cyclic subgraph over a handful of real Arbitrum hub tokens (WETH,
/// USDC, WBTC, ARB), so route enumeration produces thousands of real 2/3/4-hop cycles —
/// stressing every pool's CSR membership.
fn load_real_pools_over_hub(hub: &BTreeSet<[u8; 20]>) -> Vec<RealPool> {
    // Worktree-portable: read the COMMITTED hub-token V2/V3 golden subset (the exact pools this
    // hub filter selects, extracted verbatim from the untracked engine/discovery_cache.json) so a
    // fresh checkout / clean-environment gate runs this completeness proof with no hand-copied
    // artifact. The loader's hub + V2/V3 family filter below is preserved unchanged.
    let bytes = std::fs::read(
        repo_root().join("codex/campaign_baseline_wi01/discovery_cache_golden.json"),
    )
    .expect("read discovery cache golden fixture");
    let json: Value = serde_json::from_slice(&bytes).expect("parse discovery cache json");
    let pools = json
        .get("pools")
        .and_then(Value::as_array)
        .expect("discovery cache pools array");

    let mut out = Vec::new();
    for p in pools {
        let family = p
            .get("family")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("pool missing family: {p}"));
        let Some(kind) = family_str_to_v2_v3_kind(family) else {
            continue;
        };
        let token0 = value_address(p, "token0");
        let token1 = value_address(p, "token1");
        if !hub.contains(&token0) || !hub.contains(&token1) {
            continue;
        }
        if token0 == token1 {
            continue; // degenerate; never happens in the cache but guard anyway
        }
        let fee = if is_v2_kind(kind) {
            // V2 fee is in bps; cache stores it in fee_bps (0 ⇒ canonical 30 bps).
            let fb = value_u32(p, "fee_bps");
            if fb == 0 {
                30
            } else {
                fb
            }
        } else {
            value_u32(p, "fee_pips")
        };
        out.push(RealPool {
            address: token0_addr_unused_guard(p, kind, token0, token1, fee),
            kind,
            token0,
            token1,
            fee,
            tick_spacing: value_u32(p, "tick_spacing") as i32,
        });
    }
    out
}

/// Pull the pool's real address (kept in its own fn purely to keep `load_real_pools_over_hub`
/// readable; the extra args document that address selection is independent of kind/tokens/fee).
fn token0_addr_unused_guard(
    p: &Value,
    _kind: PoolKind,
    _t0: [u8; 20],
    _t1: [u8; 20],
    _fee: u32,
) -> [u8; 20] {
    value_address(p, "address")
}

/// The four real Arbitrum hub tokens (highest V2/V3 pool counts in the cache):
/// WETH, USDC, WBTC, ARB. Returned as a set of 20-byte addresses.
fn hub_tokens() -> BTreeSet<[u8; 20]> {
    let mut s = BTreeSet::new();
    for hexs in [
        "82af49447d8a07e3bd95bd0d56f35241523fbab1", // WETH
        "af88d065e77c8cc2239327c5edb3a432268e5831", // USDC (native)
        "fd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9", // WBTC
        "912ce59144191c1204e64559fe8253a0e49e6548", // ARB
    ] {
        let mut a = [0u8; 20];
        faster_hex::hex_decode(hexs.as_bytes(), &mut a).unwrap();
        s.insert(a);
    }
    s
}

/// Build a universe from the real-pool subset. Reserves/liquidity are NOT in the discovery
/// cache (they need the RPC state snapshot), so we seed each pool with non-degenerate state
/// — this completeness test is PURELY structural (route graph + index membership), and the
/// index is a function of the enumerated hop lists only, never of pool state. The real
/// data that matters here is the pool IDENTITY (address / token pair / family / fee), which
/// is exactly what determines route adjacency and thus CSR membership.
fn build_universe(pools: &[RealPool]) -> (engine_state::Universe, BTreeMap<[u8; 20], TokenId>) {
    let mut ub = UniverseBuilder::new();
    // Register every token that appears, deterministically (sorted) so ids are stable.
    let mut token_addrs: BTreeSet<[u8; 20]> = BTreeSet::new();
    for p in pools {
        token_addrs.insert(p.token0);
        token_addrs.insert(p.token1);
    }
    let mut tok_id: BTreeMap<[u8; 20], TokenId> = BTreeMap::new();
    for (i, a) in token_addrs.iter().enumerate() {
        // 6-dp for the USDC-shaped one, else 18 — decimals are irrelevant to enumeration,
        // this just keeps the builder honest.
        let decimals = if i == 0 { 18 } else { 18 };
        let id = ub.add_token(*a, decimals, RiskClass::Permissionless);
        tok_id.insert(*a, id);
    }
    // Seed plausible reserves: a constant-product pool with a 1:1-ish balance. Values are
    // structural scaffolding only.
    let v2_seed = V2State {
        reserves: V2Reserves256 {
            reserve0: 1_000_000_000_000_000_000_000,
            reserve1: 1_000_000_000_000_000_000_000,
            block_timestamp_last: 0,
        },
    };
    let v3_seed = |fee_pips: u32, tick_spacing: i32| V3State {
        sqrt_price_x96: alloy_primitives::U256::from(1u128) << 96, // price = 1.0
        tick: 0,
        liquidity: 1_000_000_000_000_000_000u128,
        fee_pips,
        tick_spacing,
        initialised_ticks: vec![],
    };
    for p in pools {
        let t0 = tok_id[&p.token0];
        let t1 = tok_id[&p.token1];
        if is_v2_kind(p.kind) {
            // Some cache rows can collide on address if the same pool appears under two
            // pairs; ignore duplicate-address insert errors (the registry rejects them).
            let _ = ub.add_v2_pool(p.address, p.kind, t0, t1, VenueId(0), p.fee, v2_seed);
        } else {
            let _ = ub.add_v3_pool(
                p.address,
                p.kind,
                t0,
                t1,
                VenueId(0),
                p.fee,
                p.tick_spacing.max(1),
                v3_seed(p.fee, p.tick_spacing.max(1)),
            );
        }
    }
    (ub.build(), tok_id)
}

/// Enumerate the full route table exactly as `startup.rs` step 5 does (2-hop + triangles
/// + a capped 4-hop tier). The 4-hop cap keeps the test fast while still exercising deep
/// routes; 2-hop + triangles alone already touch every pool densely.
fn enumerate_all(universe: &engine_state::Universe) -> RouteTable {
    let mut rt = RouteTable::new();
    enumerate_two_hop_routes(&universe.pools, &mut rt);
    enumerate_triangles(&universe.pools, &mut rt);
    // 4-hop with a generous-but-bounded cap (same call shape as startup, which offsets the
    // cap by current table size).
    let cap = rt.len().saturating_add(20_000);
    let _hit = enumerate_k_hop_cycles(&universe.pools, 4, cap, &mut rt);
    rt
}

/// Build the production CSR the way `startup.rs` step 7 builds it: push (pool, route_id)
/// for every hop of every route, then finalise. (We use the plain `build()` — the
/// score-ordering in `build_score_ordered` only permutes the per-pool slice ORDER, never
/// its membership, which is what this test asserts.)
fn build_csr(rt: &RouteTable, n_pools: usize) -> engine_routes::Csr {
    let mut csr = CsrBuilder::new(n_pools as u32);
    for r in rt.all() {
        for h in r.hops.iter().take(r.hop_count as usize) {
            csr.push(h.pool.0, r.id);
        }
    }
    csr.build()
}

/// Independent ground truth: brute-force scan EVERY route, collecting for each pool the set
/// of route ids whose hop list contains that pool.
fn brute_force_pool_to_routes(rt: &RouteTable, n_pools: usize) -> Vec<BTreeSet<RouteId>> {
    let mut map: Vec<BTreeSet<RouteId>> = vec![BTreeSet::new(); n_pools];
    for r in rt.all() {
        for h in r.hops.iter().take(r.hop_count as usize) {
            map[h.pool.0 as usize].insert(r.id);
        }
    }
    map
}

#[test]
fn csr_returns_every_route_touching_each_dirty_pool_real_pool_universe() {
    let hub = hub_tokens();
    let pools = load_real_pools_over_hub(&hub);
    // Sanity: we actually loaded a meaningful slice of the REAL cache.
    assert!(
        pools.len() >= 60,
        "expected a dense real V2/V3 hub subset from discovery_cache.json, got {} pools",
        pools.len()
    );

    let (universe, _tok) = build_universe(&pools);
    let n_pools = universe.pools.len();
    assert!(
        n_pools >= 60,
        "universe should hold the real pool subset, got {n_pools}"
    );

    let rt = enumerate_all(&universe);
    assert!(
        rt.len() >= 1_000,
        "dense real hub subset should enumerate many cyclic routes, got {}",
        rt.len()
    );

    let csr = build_csr(&rt, n_pools);
    let ground_truth = brute_force_pool_to_routes(&rt, n_pools);

    // THE INVARIANT — exact bidirectional membership for EVERY pool.
    let mut pools_with_routes = 0usize;
    let mut total_memberships = 0usize;
    for pid in 0..n_pools {
        let csr_set: BTreeSet<RouteId> =
            csr.slice_for(PoolId(pid as u32)).iter().copied().collect();
        let truth = &ground_truth[pid];

        // No route that touches this pool is missing from the CSR ...
        let missing: Vec<_> = truth.difference(&csr_set).collect();
        assert!(
            missing.is_empty(),
            "CSR MISSED routes for pool {pid}: {} routes touch it by brute force but are \
             absent from slice_for() — these would never be scored/evaluated (money left on \
             the table). first few missing: {:?}",
            missing.len(),
            missing.iter().take(5).collect::<Vec<_>>()
        );
        // ... and the CSR invents no route that does not actually touch this pool.
        let spurious: Vec<_> = csr_set.difference(truth).collect();
        assert!(
            spurious.is_empty(),
            "CSR returned routes for pool {pid} that do NOT contain it: {:?}",
            spurious.iter().take(5).collect::<Vec<_>>()
        );

        // Exact set equality (covers both directions in one shot).
        assert_eq!(
            csr_set, *truth,
            "CSR membership != brute-force membership for pool {pid}"
        );

        if !truth.is_empty() {
            pools_with_routes += 1;
        }
        total_memberships += truth.len();
    }

    // Every pool in this dense subset participates in at least one cycle, and the index is
    // non-trivially populated (guards against a vacuous all-empty pass).
    assert_eq!(
        pools_with_routes, n_pools,
        "every pool in the dense real hub subset should appear in at least one route"
    );
    assert!(
        total_memberships >= rt.len(),
        "membership count ({total_memberships}) should be >= route count ({}) — each route \
         contributes >=2 pool memberships",
        rt.len()
    );

    eprintln!(
        "CSR completeness PROVEN: {n_pools} real pools, {} routes, {total_memberships} \
         pool→route memberships — slice_for() == brute-force for every pool.",
        rt.len()
    );
}

/// Targeted complement: pick the single highest-degree real pool (the most-shared hub pool)
/// and prove the CSR returns EXACTLY its brute-force route set. This is the literal
/// "dirty pool → every containing route" lookup the hot path performs, isolated to the
/// worst-case fan-out pool.
#[test]
fn csr_lookup_for_highest_degree_real_pool_is_exhaustive() {
    let hub = hub_tokens();
    let pools = load_real_pools_over_hub(&hub);
    let (universe, _tok) = build_universe(&pools);
    let n_pools = universe.pools.len();
    let rt = enumerate_all(&universe);
    let csr = build_csr(&rt, n_pools);
    let ground_truth = brute_force_pool_to_routes(&rt, n_pools);

    // Highest-degree pool = the one contained in the most routes.
    let (hot_pid, hot_count) = ground_truth
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.len()))
        .max_by_key(|(_, c)| *c)
        .expect("at least one pool");
    assert!(
        hot_count >= 100,
        "the busiest real hub pool should fan out to many routes, got {hot_count}"
    );

    let csr_set: BTreeSet<RouteId> = csr
        .slice_for(PoolId(hot_pid as u32))
        .iter()
        .copied()
        .collect();
    assert_eq!(
        csr_set, ground_truth[hot_pid],
        "the busiest dirty pool's CSR slice must be its complete route set ({hot_count} routes)"
    );
    // The CSR slice has no duplicates (CsrBuilder dedups) and matches the set size.
    assert_eq!(
        csr.slice_for(PoolId(hot_pid as u32)).len(),
        ground_truth[hot_pid].len(),
        "CSR slice length must equal the deduped membership count (no dropped/dup routes)"
    );

    eprintln!(
        "Busiest real pool (id {hot_pid}) fans out to {hot_count} routes — CSR returned all of \
         them exactly."
    );
}
