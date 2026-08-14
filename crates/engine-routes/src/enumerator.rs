//! Route enumeration per §5.3.
//!
//! Builds two- and three-hop directed cycles for a small token universe and a pool list.
//! Each enumerated route is canonicalised so rotations collapse to a single id; reverse
//! direction is a distinct route by construction.

use arrayvec::ArrayVec;

use engine_state::{PoolId, PoolKind, PoolMeta, PoolTable, TokenId};

use crate::canonical::canonicalise_route;
use crate::table::{Hop, Route, RouteTable, StrategyKind, MAX_HOPS};
use ahash::AHashSet;

/// WI-02: per-pool enumeration eligibility (boot `RuntimePolicy` family gate + priced-token
/// gate, computed by the caller). FAIL-CLOSED on a mask/table length mismatch: a pool with
/// no eligibility entry never enters a route.
#[inline]
fn pool_eligible_at(mask: &[bool], i: usize) -> bool {
    mask.get(i).copied().unwrap_or(false)
}

/// Enumerate every two-hop directed cycle of the form `A -> B -> A` through any pair of
/// pools `(p, q)` with `p != q` such that they share a token. Hot-bool is `false` by
/// default; the caller flips it after pool-tier promotion.
pub fn enumerate_two_hop_routes(pool_table: &PoolTable, out: &mut RouteTable) {
    let all = vec![true; pool_table.len()];
    enumerate_two_hop_routes_gated(pool_table, &all, out);
}

/// WI-02 policy-gated two-hop enumeration: a pool whose index is `false` in `pool_eligible`
/// (inactive family under the boot `RuntimePolicy`, or an unpriced token) never enters a
/// route. The ungated [`enumerate_two_hop_routes`] delegates here with an all-true mask, so
/// the two paths cannot diverge.
pub fn enumerate_two_hop_routes_gated(
    pool_table: &PoolTable,
    pool_eligible: &[bool],
    out: &mut RouteTable,
) {
    let n = pool_table.len();
    let mut seen: AHashSet<crate::canonical::RouteCanonKey> = AHashSet::default();
    for i in 0..n {
        if !pool_eligible_at(pool_eligible, i) {
            continue;
        }
        let p1 = pool_table.meta(PoolId(i as u32));
        // Only V2 and V3 pools in Phase 1.
        if !is_phase1_supported(p1.kind) {
            continue;
        }
        for j in 0..n {
            if i == j {
                continue;
            }
            if !pool_eligible_at(pool_eligible, j) {
                continue;
            }
            let p2 = pool_table.meta(PoolId(j as u32));
            if !is_phase1_supported(p2.kind) {
                continue;
            }
            // Find a common token between p1 and p2; that is the cycle "midpoint" (B).
            // The "base" token A is the *other* token in p1.
            let pairs = pair_meeting(p1, p2);
            for (a, b, dir1, dir2) in pairs {
                let mut hops: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
                hops.push(Hop::new(PoolId(i as u32), dir1));
                hops.push(Hop::new(PoolId(j as u32), dir2));
                let mut tokens: ArrayVec<TokenId, { MAX_HOPS + 1 }> = ArrayVec::new();
                tokens.push(a);
                tokens.push(b);
                tokens.push(a);
                let key = canonicalise_route(&hops, &tokens);
                if !seen.insert(key) {
                    continue;
                }
                let r = Route {
                    id: engine_state::RouteId(0),
                    strategy: StrategyKind::TwoHop,
                    hop_count: 2,
                    hops,
                    tokens,
                    family_signature: family_signature_for(p1.kind, p2.kind, None),
                    calldata_template_id: calldata_template_id_for(p1.kind, p2.kind, None),
                    gas_template_id: 0,
                    l1_size_bucket_template: 0,
                    amount_grid_id: 0,
                    risk_flags: 0,
                    readiness_mask: 0,
                    route_heat: 0,
                    hot: false,
                };
                out.insert(r);
            }
        }
    }
}

/// Enumerate every `k`-hop directed cycle for `k ∈ 4..=8` over `pool_table`. Each cycle
/// starts and ends at the same base token, visits `k` distinct pools, and is
/// canonicalised so rotations and exact dupes collapse to a single entry. Truncates at
/// `max_routes` to bound startup memory and sim cost — plan §5.3 mandates per-tier caps
/// because the raw search space at 8 hops is in the hundreds of thousands.
///
/// Returns `true` iff the cap was hit (further enumeration was skipped). The caller
/// records this in the startup log so operators know when to raise the cap.
pub fn enumerate_k_hop_cycles(
    pool_table: &PoolTable,
    k: usize,
    max_routes: usize,
    out: &mut RouteTable,
) -> bool {
    let all = vec![true; pool_table.len()];
    enumerate_k_hop_cycles_gated(pool_table, &all, k, max_routes, out)
}

/// WI-02 policy-gated k-hop enumeration: an ineligible pool never enters the adjacency
/// graph, so no cycle can route through it. The ungated [`enumerate_k_hop_cycles`]
/// delegates here with an all-true mask.
pub fn enumerate_k_hop_cycles_gated(
    pool_table: &PoolTable,
    pool_eligible: &[bool],
    k: usize,
    max_routes: usize,
    out: &mut RouteTable,
) -> bool {
    assert!(
        (4..=MAX_HOPS).contains(&k),
        "k must be 4..=MAX_HOPS, got {k}"
    );
    let n = pool_table.len();
    // Adjacency: for each token, the list of (pool_idx, other_token) edges.
    let mut adj: Vec<Vec<(u32, TokenId)>> = Vec::new();
    for i in 0..n {
        if !pool_eligible_at(pool_eligible, i) {
            continue;
        }
        let p = pool_table.meta(PoolId(i as u32));
        if !is_phase1_supported(p.kind) {
            continue;
        }
        let t0 = p.token0.0 as usize;
        let t1 = p.token1.0 as usize;
        let need = t0.max(t1) + 1;
        if adj.len() < need {
            adj.resize(need, Vec::new());
        }
        adj[t0].push((i as u32, p.token1));
        adj[t1].push((i as u32, p.token0));
    }
    let mut seen: AHashSet<crate::canonical::RouteCanonKey> = AHashSet::default();
    // Per-hop scratch.
    let mut pool_stack: Vec<u32> = Vec::with_capacity(k);
    let mut dir_stack: Vec<u8> = Vec::with_capacity(k);
    let mut tok_stack: Vec<TokenId> = Vec::with_capacity(k + 1);
    let mut visited_pools = AHashSet::default();
    for base in 0..adj.len() {
        if adj[base].is_empty() {
            continue;
        }
        if out.len() >= max_routes {
            return true;
        }
        let base_tok = TokenId(base as u16);
        tok_stack.clear();
        tok_stack.push(base_tok);
        dfs_kcycle(
            pool_table,
            &adj,
            base_tok,
            base_tok,
            k,
            &mut pool_stack,
            &mut dir_stack,
            &mut tok_stack,
            &mut visited_pools,
            &mut seen,
            max_routes,
            out,
        );
    }
    out.len() >= max_routes
}

#[allow(clippy::too_many_arguments)]
fn dfs_kcycle(
    pool_table: &PoolTable,
    adj: &[Vec<(u32, TokenId)>],
    base: TokenId,
    current: TokenId,
    remaining: usize,
    pool_stack: &mut Vec<u32>,
    dir_stack: &mut Vec<u8>,
    tok_stack: &mut Vec<TokenId>,
    visited_pools: &mut AHashSet<u32>,
    seen: &mut AHashSet<crate::canonical::RouteCanonKey>,
    max_routes: usize,
    out: &mut RouteTable,
) {
    if remaining == 0 {
        // Cycle must close back at base; the last edge was already enforced before
        // descending into the leaf, so check token identity here.
        if current != base {
            return;
        }
        if out.len() >= max_routes {
            return;
        }
        let k = pool_stack.len();
        let mut hops: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
        for i in 0..k {
            hops.push(Hop::new(PoolId(pool_stack[i]), dir_stack[i]));
        }
        let mut toks: ArrayVec<TokenId, { MAX_HOPS + 1 }> = ArrayVec::new();
        for t in tok_stack.iter() {
            toks.push(*t);
        }
        let key = crate::canonical::canonicalise_route(&hops, &toks);
        if !seen.insert(key) {
            return;
        }
        // Family signature: pack the first three families (matches existing helper
        // shape); deeper hops are summarised by the count.
        let m0 = pool_table.meta(PoolId(pool_stack[0]));
        let m1 = pool_table.meta(PoolId(pool_stack[1]));
        let m2_opt = if k >= 3 {
            Some(pool_table.meta(PoolId(pool_stack[2])).kind)
        } else {
            None
        };
        let strategy = match k {
            4 => StrategyKind::FourHop,
            5 => StrategyKind::FiveHop,
            6 => StrategyKind::SixHop,
            7 => StrategyKind::SevenHop,
            8 => StrategyKind::EightHop,
            _ => unreachable!(),
        };
        let r = Route {
            id: engine_state::RouteId(0),
            strategy,
            hop_count: k as u8,
            hops,
            tokens: toks,
            family_signature: family_signature_for(m0.kind, m1.kind, m2_opt),
            calldata_template_id: calldata_template_id_for(m0.kind, m1.kind, m2_opt),
            // Deep tiers reuse triangle gas/L1 template baselines; the simulator
            // accounts for per-hop incremental gas at evaluation time.
            gas_template_id: 1,
            l1_size_bucket_template: 1,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot: false,
        };
        out.insert(r);
        return;
    }
    if out.len() >= max_routes {
        return;
    }
    let cur_idx = current.0 as usize;
    if cur_idx >= adj.len() {
        return;
    }
    for &(pool_idx, other) in adj[cur_idx].iter() {
        if !visited_pools.insert(pool_idx) {
            continue;
        }
        // Direction: 0 if we leave via token0→token1 (i.e. current == token0).
        let meta = pool_table.meta(PoolId(pool_idx));
        let dir: u8 = if meta.token0 == current { 0 } else { 1 };
        // Mid-cycle pruning rules:
        // * pool can never repeat (enforced via `visited_pools` set above);
        // * base token can only appear at start and end — never as an internal vertex
        //   (an internal base hit decomposes the cycle into two shorter ones, which the
        //   shorter tiers already evaluate);
        // * non-base token revisits via a *different* pool are allowed — this is the
        //   cross-venue case the plan §5.3 calls out (e.g. WETH→USDC[poolA]→WBTC→
        //   USDC[poolB]→WETH is a valid 4-hop cycle, with USDC visited via two pools).
        let next_tok = other;
        if remaining > 1 && next_tok == base {
            visited_pools.remove(&pool_idx);
            continue;
        }
        pool_stack.push(pool_idx);
        dir_stack.push(dir);
        tok_stack.push(next_tok);
        dfs_kcycle(
            pool_table,
            adj,
            base,
            next_tok,
            remaining - 1,
            pool_stack,
            dir_stack,
            tok_stack,
            visited_pools,
            seen,
            max_routes,
            out,
        );
        pool_stack.pop();
        dir_stack.pop();
        tok_stack.pop();
        visited_pools.remove(&pool_idx);
    }
}

/// Enumerate three-hop triangles `A -> B -> C -> A` over the pool list.
pub fn enumerate_triangles(pool_table: &PoolTable, out: &mut RouteTable) {
    let all = vec![true; pool_table.len()];
    enumerate_triangles_gated(pool_table, &all, out);
}

/// WI-02 policy-gated triangle enumeration; see [`enumerate_two_hop_routes_gated`]. The
/// ungated [`enumerate_triangles`] delegates here with an all-true mask.
pub fn enumerate_triangles_gated(
    pool_table: &PoolTable,
    pool_eligible: &[bool],
    out: &mut RouteTable,
) {
    let n = pool_table.len();
    let mut seen: AHashSet<crate::canonical::RouteCanonKey> = AHashSet::default();
    for i in 0..n {
        if !pool_eligible_at(pool_eligible, i) {
            continue;
        }
        let p1 = pool_table.meta(PoolId(i as u32));
        if !is_phase1_supported(p1.kind) {
            continue;
        }
        for j in 0..n {
            if i == j {
                continue;
            }
            if !pool_eligible_at(pool_eligible, j) {
                continue;
            }
            let p2 = pool_table.meta(PoolId(j as u32));
            if !is_phase1_supported(p2.kind) {
                continue;
            }
            // p1 token-set ∩ p2 token-set = {midpoint B} OR shared start.
            for k in 0..n {
                if k == i || k == j {
                    continue;
                }
                if !pool_eligible_at(pool_eligible, k) {
                    continue;
                }
                let p3 = pool_table.meta(PoolId(k as u32));
                if !is_phase1_supported(p3.kind) {
                    continue;
                }
                // Need: p1 (A->B), p2 (B->C), p3 (C->A) such that no pool is repeated and the
                // three tokens are distinct.
                let cycles = triangle_cycles(p1, p2, p3);
                for (a, b, c, d1, d2, d3) in cycles {
                    let mut hops: ArrayVec<Hop, MAX_HOPS> = ArrayVec::new();
                    hops.push(Hop::new(PoolId(i as u32), d1));
                    hops.push(Hop::new(PoolId(j as u32), d2));
                    hops.push(Hop::new(PoolId(k as u32), d3));
                    let mut tokens: ArrayVec<TokenId, { MAX_HOPS + 1 }> = ArrayVec::new();
                    tokens.push(a);
                    tokens.push(b);
                    tokens.push(c);
                    tokens.push(a);
                    let key = canonicalise_route(&hops, &tokens);
                    if !seen.insert(key) {
                        continue;
                    }
                    let r = Route {
                        id: engine_state::RouteId(0),
                        strategy: StrategyKind::Triangle,
                        hop_count: 3,
                        hops,
                        tokens,
                        family_signature: family_signature_for(p1.kind, p2.kind, Some(p3.kind)),
                        calldata_template_id: calldata_template_id_for(
                            p1.kind,
                            p2.kind,
                            Some(p3.kind),
                        ),
                        gas_template_id: 1,
                        l1_size_bucket_template: 1,
                        amount_grid_id: 0,
                        risk_flags: 0,
                        readiness_mask: 0,
                        route_heat: 0,
                        hot: false,
                    };
                    out.insert(r);
                }
            }
        }
    }
}

fn is_phase1_supported(k: PoolKind) -> bool {
    matches!(
        k,
        PoolKind::UniswapV2
            | PoolKind::SushiV2
            | PoolKind::CamelotV2
            | PoolKind::UniswapV3
            | PoolKind::SushiV3
            | PoolKind::PancakeV3
            | PoolKind::AlgebraV19
            | PoolKind::UniswapV4
            | PoolKind::FluidDex
            | PoolKind::KyberSwapElastic
            | PoolKind::DodoV2
            | PoolKind::SolidlyV3
            | PoolKind::MaverickV2
            | PoolKind::BalancerStable
            | PoolKind::BalancerWeighted
            | PoolKind::CurveStableSwapNg
            | PoolKind::CurveCryptoSwap
            | PoolKind::TraderJoeLb
    )
}

fn family_signature_for(a: PoolKind, b: PoolKind, c: Option<PoolKind>) -> u16 {
    let lo = a as u16;
    let mid = (b as u16) << 4;
    let hi = c.map(|k| (k as u16) << 8).unwrap_or(0);
    lo | mid | hi
}

fn calldata_template_id_for(a: PoolKind, b: PoolKind, c: Option<PoolKind>) -> u32 {
    use PoolKind::*;
    match (a, b, c) {
        (UniswapV2 | SushiV2 | CamelotV2, UniswapV2 | SushiV2 | CamelotV2, None) => 0,
        (UniswapV2 | SushiV2 | CamelotV2, UniswapV3 | SushiV3, None) => 1,
        (UniswapV3 | SushiV3, UniswapV2 | SushiV2 | CamelotV2, None) => 2,
        (UniswapV3 | SushiV3, UniswapV3 | SushiV3, None) => 3,
        (
            UniswapV2 | SushiV2 | CamelotV2,
            UniswapV3 | SushiV3,
            Some(UniswapV2 | SushiV2 | CamelotV2),
        ) => 4,
        (UniswapV3 | SushiV3, UniswapV2 | SushiV2 | CamelotV2, Some(UniswapV3 | SushiV3)) => 5,
        _ => 0,
    }
}

fn pair_meeting(p1: &PoolMeta, p2: &PoolMeta) -> Vec<(TokenId, TokenId, u8, u8)> {
    // Return all `(start_token A, midpoint B, dir1, dir2)` orderings.
    let mut out = Vec::new();
    // Case 1: A = p1.token0, B = p1.token1; need p2 to have B as one of its tokens.
    if p1.token1 == p2.token0 {
        out.push((p1.token0, p1.token1, 0, 0)); // p1 token0->token1; p2 token0->token1; second hop dir is wrong
    }
    if p1.token1 == p2.token1 {
        out.push((p1.token0, p1.token1, 0, 1));
    }
    if p1.token0 == p2.token0 {
        // A = p1.token1, B = p1.token0 (i.e. p1 swapped backwards).
        out.push((p1.token1, p1.token0, 1, 0));
    }
    if p1.token0 == p2.token1 {
        out.push((p1.token1, p1.token0, 1, 1));
    }
    // Each entry must close the cycle: after hop2 we must arrive at A again, not at the
    // unrelated token in p2. Filter accordingly.
    out.retain(|(a, b, _d1, d2)| {
        // hop2 input is B; if d2==0 we send token0->token1, so output is p2.token1; that must equal A.
        let out_token = if *d2 == 0 { p2.token1 } else { p2.token0 };
        let in_token = if *d2 == 0 { p2.token0 } else { p2.token1 };
        let _ = b;
        out_token == *a && in_token == *b
    });
    out
}

fn triangle_cycles(
    p1: &PoolMeta,
    p2: &PoolMeta,
    p3: &PoolMeta,
) -> Vec<(TokenId, TokenId, TokenId, u8, u8, u8)> {
    // A -> B -> C -> A. Enumerate per-hop direction.
    let mut out = Vec::new();
    let p1_tokens = [(p1.token0, p1.token1, 0u8), (p1.token1, p1.token0, 1u8)];
    let p2_tokens = [(p2.token0, p2.token1, 0u8), (p2.token1, p2.token0, 1u8)];
    let p3_tokens = [(p3.token0, p3.token1, 0u8), (p3.token1, p3.token0, 1u8)];
    for &(a1, b1, d1) in &p1_tokens {
        for &(a2, b2, d2) in &p2_tokens {
            if b1 != a2 {
                continue;
            }
            for &(a3, b3, d3) in &p3_tokens {
                if b2 != a3 {
                    continue;
                }
                if b3 != a1 {
                    continue;
                }
                // Must have three distinct tokens.
                if a1 == b1 || a1 == b2 || b1 == b2 {
                    continue;
                }
                out.push((a1, b1, b2, d1, d2, d3));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_state::{StateSlot, TokenId, VenueId};

    fn dummy_pool(id_seed: u32, t0: u16, t1: u16, kind: PoolKind) -> PoolMeta {
        let mut addr = [0u8; 20];
        addr[..4].copy_from_slice(&id_seed.to_be_bytes());
        PoolMeta {
            address: addr,
            kind,
            token0: TokenId(t0),
            token1: TokenId(t1),
            pair: engine_state::PairId(0),
            venue: VenueId(0),
            fee_pips_or_bps: 30,
            tick_spacing: 0,
            state_slot: StateSlot(id_seed),
        }
    }

    #[test]
    fn enumerate_two_hop_pair() {
        let mut pt = PoolTable::new();
        // Two V2 pools on the same (token0=0, token1=1) pair.
        pt.insert(dummy_pool(0, 0, 1, PoolKind::UniswapV2));
        pt.insert(dummy_pool(1, 0, 1, PoolKind::SushiV2));
        let mut rt = RouteTable::new();
        enumerate_two_hop_routes(&pt, &mut rt);
        // Expect at least one route 0->1->0 over pool0 and pool1 (and its reverse direction).
        assert!(rt.len() >= 2);
        for r in rt.all() {
            assert_eq!(r.hop_count, 2);
            assert_eq!(r.tokens.first(), r.tokens.last());
        }
    }

    #[test]
    fn enumerate_two_hop_fluid_v4_pair() {
        let mut pt = PoolTable::new();
        pt.insert(dummy_pool(0, 0, 1, PoolKind::FluidDex));
        pt.insert(dummy_pool(1, 0, 1, PoolKind::UniswapV4));
        let mut rt = RouteTable::new();
        enumerate_two_hop_routes(&pt, &mut rt);

        assert!(
            rt.all().iter().any(|r| {
                r.hop_count == 2
                    && r.hops
                        .iter()
                        .take(2)
                        .any(|h| h.pool == engine_state::PoolId(0))
                    && r.hops
                        .iter()
                        .take(2)
                        .any(|h| h.pool == engine_state::PoolId(1))
            }),
            "expected Fluid+V4 pair to produce a two-hop route"
        );
    }

    #[test]
    fn enumerate_two_hop_includes_trader_joe_lb() {
        // TraderJoeLb is fully supported downstream (quote_one_hop arm, predict_lb_hop,
        // resync Lb, LB Swap-log handling) but was missing from is_phase1_supported, so
        // route enumeration skipped every LB pool — no LB arb could ever be detected.
        // An LB pool paired with a UniswapV3 pool on the same tokens must produce a
        // two-hop cross-DEX cycle through the LB pool.
        let mut pt = PoolTable::new();
        pt.insert(dummy_pool(0, 0, 1, PoolKind::TraderJoeLb));
        pt.insert(dummy_pool(1, 0, 1, PoolKind::UniswapV3));
        let mut rt = RouteTable::new();
        enumerate_two_hop_routes(&pt, &mut rt);
        assert!(
            rt.all().iter().any(|r| {
                r.hop_count == 2
                    && r.hops
                        .iter()
                        .take(2)
                        .any(|h| h.pool == engine_state::PoolId(0))
                    && r.hops
                        .iter()
                        .take(2)
                        .any(|h| h.pool == engine_state::PoolId(1))
            }),
            "expected TraderJoeLb+V3 pair to produce a two-hop route"
        );
    }

    #[test]
    fn enumerate_4_hop_closed_cycle() {
        // 4 tokens A-B-C-D-A with one pool per edge → exactly one 4-cycle in each
        // direction; expect at least 2 routes (forward + reverse).
        let mut pt = PoolTable::new();
        pt.insert(dummy_pool(0, 0, 1, PoolKind::UniswapV3)); // A-B
        pt.insert(dummy_pool(1, 1, 2, PoolKind::UniswapV3)); // B-C
        pt.insert(dummy_pool(2, 2, 3, PoolKind::UniswapV3)); // C-D
        pt.insert(dummy_pool(3, 3, 0, PoolKind::UniswapV3)); // D-A
        let mut rt = RouteTable::new();
        let hit_cap = enumerate_k_hop_cycles(&pt, 4, 1024, &mut rt);
        assert!(!hit_cap);
        assert!(
            !rt.is_empty(),
            "expected at least one 4-cycle, got {}",
            rt.len()
        );
        for r in rt.all() {
            assert_eq!(r.hop_count, 4);
            assert_eq!(r.tokens.first(), r.tokens.last());
            // No internal token repeats.
            let mut seen = std::collections::HashSet::new();
            for (i, t) in r.tokens.iter().enumerate() {
                if i == 0 || i == r.tokens.len() - 1 {
                    continue;
                }
                assert!(seen.insert(*t), "internal token repeated in {r:?}");
            }
        }
    }

    #[test]
    fn enumerate_5_hop_truncates_at_cap() {
        // Build a denser graph and cap at 2 routes; should truncate.
        let mut pt = PoolTable::new();
        // Five tokens 0..4, each connected to every other by one V3 pool.
        let mut id = 0u32;
        for a in 0u16..5 {
            for b in (a + 1)..5 {
                pt.insert(dummy_pool(id, a, b, PoolKind::UniswapV3));
                id += 1;
            }
        }
        let mut rt = RouteTable::new();
        let hit_cap = enumerate_k_hop_cycles(&pt, 5, 2, &mut rt);
        assert!(hit_cap, "expected cap hit when capping 5-hop at 2 routes");
        assert!(rt.len() <= 2);
    }

    /// Bench: enumeration cost at each hop tier for a 4-token V3 universe that mirrors
    /// the 4-token soak config (44 pools across 6 token-pairs). Printed via `--nocapture`.
    /// Cheap enough to live in the test suite; not a hard assertion (counts depend on
    /// canonicalisation behaviour and platform allocator).
    #[test]
    fn bench_topology_4token_44pool() {
        use std::time::Instant;
        let mut pt = PoolTable::new();
        let mut id = 0u32;
        let pair_counts: &[(u16, u16, usize)] = &[
            (0, 1, 11), // WETH/USDC
            (0, 2, 11), // WETH/USDT
            (0, 3, 10), // WETH/WBTC
            (1, 2, 4),  // USDC/USDT
            (1, 3, 4),  // USDC/WBTC
            (2, 3, 4),  // USDT/WBTC
        ];
        for &(t0, t1, n) in pair_counts {
            for _ in 0..n {
                pt.insert(dummy_pool(id, t0, t1, PoolKind::UniswapV3));
                id += 1;
            }
        }
        let pool_count = pt.len();
        eprintln!("\n=== topology bench: {pool_count} V3 pools, 4 tokens, 6 pairs ===");

        let t0 = Instant::now();
        let mut rt2 = RouteTable::new();
        enumerate_two_hop_routes(&pt, &mut rt2);
        eprintln!("  k=2: {:>7} routes in {:?}", rt2.len(), t0.elapsed());

        let t0 = Instant::now();
        let mut rt3 = RouteTable::new();
        enumerate_triangles(&pt, &mut rt3);
        eprintln!("  k=3: {:>7} routes in {:?}", rt3.len(), t0.elapsed());

        for k in 4..=8 {
            let t0 = Instant::now();
            let mut rt = RouteTable::new();
            let cap = 8192;
            let hit_cap = enumerate_k_hop_cycles(&pt, k, cap, &mut rt);
            eprintln!(
                "  k={k}: {:>7} routes in {:?}  (cap={cap}, hit_cap={hit_cap})",
                rt.len(),
                t0.elapsed()
            );
        }
    }

    #[test]
    fn enumerate_triangle_three_tokens() {
        let mut pt = PoolTable::new();
        pt.insert(dummy_pool(0, 0, 1, PoolKind::UniswapV2)); // A-B
        pt.insert(dummy_pool(1, 1, 2, PoolKind::UniswapV2)); // B-C
        pt.insert(dummy_pool(2, 2, 0, PoolKind::UniswapV2)); // C-A
        let mut rt = RouteTable::new();
        enumerate_triangles(&pt, &mut rt);
        assert!(!rt.is_empty());
        for r in rt.all() {
            assert_eq!(r.hop_count, 3);
            assert_eq!(r.tokens.first(), r.tokens.last());
        }
    }
}
