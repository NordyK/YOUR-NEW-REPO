//! Route-ranking spot-rate cache micro-bench.
//!
//! Measures the per-event ranking SCAN — the ~77% route fan-out cost that dominates
//! `sim_ns` — two ways over an IDENTICAL route set, bucketed per AMM family:
//!   * `cached`       — one `Relaxed` atomic load per hop from the prewarmed
//!                      `PoolSpotCache` (the new hot path),
//!   * `direct_chase` — the old behaviour: recompute every hop's spot rate from
//!                      universe state on every route (pool meta chase + state load +
//!                      `u256→f64` + fee).
//!
//! Both arms produce bit-identical scores (proven in `spot_cache` unit tests); this only
//! measures the cost. The cache's per-event refresh is one `compute_pool_spot` per dirty
//! pool (≈1/event), amortised away by the scan and excluded from the timed loop here.

use std::sync::Arc;

use alloy_primitives::U256;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use engine::spot_cache::{
    build_prewarmed_cache, compute_pool_spot, cycle_marginal_product_hops, fold_marginal_product,
};
use engine_amm::TickInfo;
use engine_routes::{enumerate_triangles, enumerate_two_hop_routes, Route, RouteTable};
use engine_state::{
    PoolKind, RiskClass, TokenId, Universe, UniverseBuilder, V2Reserves256, V2State, V3State,
    VenueId,
};

/// Synthetic multi-family universe: 4 tokens, with V2, V3, and exotic (SolidlyV3,
/// V2-reserves-backed but ranked INFINITY) pools spread across the pairs so the
/// enumerated route set spans every dispatch class.
fn build_universe() -> Universe {
    let mut ub = UniverseBuilder::new();
    for i in 0..4u8 {
        let mut a = [0u8; 20];
        a[0] = i + 1;
        ub.add_token(a, 18, RiskClass::Permissionless);
    }
    let ticks = || {
        let mut v = Vec::with_capacity(10);
        for k in -5i32..0 {
            v.push(TickInfo {
                tick: k * 10,
                liquidity_net: 100_000_000_000i128,
            });
        }
        for k in 0i32..5 {
            v.push(TickInfo {
                tick: (k + 1) * 10,
                liquidity_net: -100_000_000_000i128,
            });
        }
        v
    };
    let v3 = |off: u64| V3State {
        sqrt_price_x96: U256::from(1u128 << 96).saturating_add(U256::from(off)),
        tick: 0,
        liquidity: 1_000_000_000_000_000_000u128,
        fee_pips: 500,
        tick_spacing: 10,
        initialised_ticks: ticks(),
    };
    let v2 = |r0: u128, r1: u128| V2State {
        reserves: V2Reserves256 {
            reserve0: r0,
            reserve1: r1,
            block_timestamp_last: 0,
        },
    };

    fn mk_addr(tag: u8, idx: u32) -> [u8; 20] {
        let mut a = [0u8; 20];
        a[0] = tag;
        a[1..5].copy_from_slice(&idx.to_be_bytes());
        a
    }
    let tok = |t: u16| TokenId(t);
    let mut p = 0u32;
    // V3 across all pairs (two on the top pairs for cross-venue routes).
    for &(t0, t1, n) in &[
        (0u16, 1u16, 2u32),
        (0, 2, 2),
        (0, 3, 1),
        (1, 2, 1),
        (1, 3, 1),
        (2, 3, 1),
    ] {
        for _ in 0..n {
            p += 1;
            ub.add_v3_pool(
                mk_addr(0x30, p),
                PoolKind::UniswapV3,
                tok(t0),
                tok(t1),
                VenueId(0),
                500,
                10,
                v3(p as u64),
            )
            .unwrap();
        }
    }
    // V2 across most pairs (cross-family with the V3s above).
    for &(t0, t1) in &[(0u16, 1u16), (0, 2), (1, 2), (1, 3), (2, 3)] {
        p += 1;
        ub.add_v2_pool(
            mk_addr(0x20, p),
            PoolKind::UniswapV2,
            tok(t0),
            tok(t1),
            VenueId(0),
            30,
            v2(1_000_000, 2_000_000),
        )
        .unwrap();
    }
    // Exotic (SolidlyV3): V2-store-backed, ranked INFINITY → exercises the exotic arm.
    for &(t0, t1) in &[(0u16, 3u16), (2, 3)] {
        p += 1;
        ub.add_v2_pool(
            mk_addr(0x50, p),
            PoolKind::SolidlyV3,
            tok(t0),
            tok(t1),
            VenueId(0),
            5,
            v2(1_000_000, 1_000_000),
        )
        .unwrap();
    }
    ub.build()
}

/// Classify a route by the families of its hops, for the per-family bucket.
fn family_bucket(u: &Universe, r: &Route) -> &'static str {
    let mut any_v2 = false;
    let mut any_v3 = false;
    let mut any_exotic = false;
    for h in r.hops.iter().take(r.hop_count as usize) {
        match u.pools.meta(h.pool).kind {
            PoolKind::UniswapV2 | PoolKind::SushiV2 | PoolKind::CamelotV2 => any_v2 = true,
            PoolKind::UniswapV3
            | PoolKind::SushiV3
            | PoolKind::PancakeV3
            | PoolKind::AlgebraV19
            | PoolKind::UniswapV4 => any_v3 = true,
            _ => any_exotic = true,
        }
    }
    match (any_v2, any_v3, any_exotic) {
        (_, _, true) => "exotic",
        (true, true, false) => "mixed_v2_v3",
        (true, false, false) => "v2_only",
        (false, true, false) => "v3_only",
        _ => "other",
    }
}

/// The old chase: rank one route by recomputing each hop's spot rate from universe state.
#[inline]
fn direct_chase_score(u: &Universe, r: &Route) -> f64 {
    fold_marginal_product(r.hops.iter().take(r.hop_count as usize).map(|h| {
        let dir0 = (h.direction & 1) == 0;
        compute_pool_spot(u, h.pool)[usize::from(!dir0)]
    }))
}

fn bench_rank(c: &mut Criterion) {
    let u = build_universe();
    let mut rt = RouteTable::new();
    enumerate_two_hop_routes(&u.pools, &mut rt);
    enumerate_triangles(&u.pools, &mut rt);

    // Bucket the enumerated routes by family.
    let mut buckets: std::collections::BTreeMap<&'static str, Vec<Route>> = Default::default();
    for r in rt.all() {
        buckets
            .entry(family_bucket(&u, r))
            .or_default()
            .push(r.clone());
    }
    let all: Vec<Route> = rt.all().to_vec();
    let cache = Arc::new(build_prewarmed_cache(&u));
    eprintln!(
        "rank_cache universe: pools={} routes={} buckets={:?}",
        u.pools.len(),
        all.len(),
        buckets
            .iter()
            .map(|(k, v)| (*k, v.len()))
            .collect::<Vec<_>>()
    );

    let mut group = c.benchmark_group("rank_scan");
    // Per-family + the full set: cached reads vs the direct chase, identical routes.
    let mut sets: Vec<(&'static str, &[Route])> =
        buckets.iter().map(|(k, v)| (*k, v.as_slice())).collect();
    sets.push(("all", all.as_slice()));
    for (name, routes) in sets {
        group.bench_with_input(BenchmarkId::new("cached", name), &routes, |b, routes| {
            b.iter(|| {
                let mut acc = 0.0f64;
                for r in routes.iter() {
                    acc += cycle_marginal_product_hops(&cache, &r.hops[..r.hop_count as usize]);
                }
                black_box(acc)
            })
        });
        group.bench_with_input(
            BenchmarkId::new("direct_chase", name),
            &routes,
            |b, routes| {
                b.iter(|| {
                    let mut acc = 0.0f64;
                    for r in routes.iter() {
                        acc += direct_chase_score(black_box(&u), r);
                    }
                    black_box(acc)
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_rank);
criterion_main!(benches);
