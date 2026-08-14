//! Sim-sweep micro-bench.
//!
//! Constructs a synthetic 4-token / 8-V3-pool universe (modelled after the live 4-token
//! Arbitrum config), enumerates routes, builds the CSR pool → route index, and measures
//! how long `engine::pipeline::run_one_sim_sweep` takes to evaluate every route touching
//! a dirty pool. Compares 1-pool, 2-pool, and 4-pool dirty events.
//!
//! Plan §16 budget (compute-only, c8g.2xlarge):
//!  * Negative filters (≤ 50 routes) p95 = 30 µs
//!  * Simulation (≤ 5 survivors, 2-hop V3/V3) p95 = 120 µs
//!  * **Combined per-dirty-event budget ≈ 150 µs p95.**

use std::sync::{atomic::AtomicU64, Arc};

use alloy_primitives::U256;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

use engine::pipeline::{run_one_sim_sweep, Candidate, DetectionContext};
use engine_amm::TickInfo;
use engine_detect::SelectorRegistry;
use engine_gas::GasSnapshot;
use engine_metrics::ReasonRing;
use engine_routes::{enumerate_triangles, enumerate_two_hop_routes, CsrBuilder, RouteTable};
use engine_state::{PoolId, PoolKind, RiskClass, TokenId, UniverseBuilder, V3State, VenueId};

/// Build a synthetic universe matching the live 4-token soak shape:
/// * 4 tokens (WETH, USDC, USDt0, WBTC)
/// * 8 V3 pools across 6 token pairs (mix of fee tiers)
/// * Routes enumerated: 2-hop + triangle
fn build_universe() -> Arc<DetectionContext> {
    let mut ub = UniverseBuilder::new();
    // 4 tokens (addresses unimportant for the sim path; ids 0..3).
    let mut token_addrs = [[0u8; 20]; 4];
    for (i, t) in token_addrs.iter_mut().enumerate() {
        t[0] = i as u8 + 1; // distinct first bytes
    }
    let mut tokens = Vec::new();
    for addr in token_addrs.iter() {
        tokens.push(ub.add_token(*addr, 18, RiskClass::Permissionless));
    }
    let _ = tokens; // ids are 0..3 in insertion order

    // Synthetic V3 pool state: sqrt_price near 1:1, ample liquidity, no tick window.
    // The grid sweep will run `v3_get_amount_out` per bucket; with empty tick window the
    // kernel uses single-tick math which is what we want to measure here.
    let make_v3 = |sqrt_x96_off: u64| V3State {
        sqrt_price_x96: U256::from(1u128 << 96).saturating_add(U256::from(sqrt_x96_off)),
        tick: 0,
        liquidity: 1_000_000_000_000_000_000u128, // 1e18 wei of L
        fee_pips: 500,                            // 0.05 %
        tick_spacing: 10,
        initialised_ticks: synthetic_ticks(),
    };

    // 8 V3 pools: 2 on WETH/USDC, 2 on WETH/USDt0, 1 on WETH/WBTC, 1 on USDC/USDt0,
    // 1 on USDC/WBTC, 1 on USDt0/WBTC. Two pools per top-volume pair lets us see
    // cross-venue arb routes in the enumeration.
    let pairs = [
        (TokenId(0), TokenId(1), 2u32),
        (TokenId(0), TokenId(2), 2),
        (TokenId(0), TokenId(3), 1),
        (TokenId(1), TokenId(2), 1),
        (TokenId(1), TokenId(3), 1),
        (TokenId(2), TokenId(3), 1),
    ];
    let mut pool_idx = 0u32;
    for &(t0, t1, n) in &pairs {
        for _ in 0..n {
            let mut addr = [0u8; 20];
            addr[0] = 0x80;
            addr[1..5].copy_from_slice(&pool_idx.to_be_bytes());
            let state = make_v3(pool_idx as u64);
            let _ = ub
                .add_v3_pool(
                    addr,
                    PoolKind::UniswapV3,
                    t0,
                    t1,
                    VenueId(0),
                    500,
                    10,
                    state,
                )
                .expect("add_v3_pool");
            pool_idx += 1;
        }
    }

    let universe = ub.build();

    // Routes: 2-hop + triangle.
    let mut route_table = RouteTable::new();
    enumerate_two_hop_routes(&universe.pools, &mut route_table);
    enumerate_triangles(&universe.pools, &mut route_table);

    // Mark all routes hot.
    for id in 0..route_table.len() {
        route_table.get_mut(engine_state::RouteId(id as u32)).hot = true;
    }

    // Build CSR.
    let mut csr = CsrBuilder::new(universe.pools.len() as u32);
    for r in route_table.all() {
        if !r.hot {
            continue;
        }
        for h in &r.hops {
            csr.push(h.pool.0, r.id);
        }
    }
    let pool_to_hot_routes = csr.build();
    eprintln!(
        "synthetic universe: pools={} routes={} hot_routes={}",
        universe.pools.len(),
        route_table.len(),
        route_table.hot_ids().len()
    );

    let n_pools = universe.pools.len();
    Arc::new(DetectionContext {
        universe: Arc::new(universe),
        route_table,
        pool_to_hot_routes,
        selectors: SelectorRegistry::canonical(),
        telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![
            1u64 << 32;
            n_pools
        ])),
        family_quote_active: engine::runtime_policy::default_active_families().dense(),
    })
}

fn synthetic_ticks() -> Vec<TickInfo> {
    // Small initialized-tick window centered at 0: ticks at multiples of tick_spacing (10).
    // 10 ticks total, 5 below and 5 above, with modest liquidity_net.
    let mut v = Vec::with_capacity(10);
    for k in -5i32..0 {
        v.push(TickInfo {
            tick: k * 10,
            liquidity_net: 100_000_000_000i128, // 1e11
        });
    }
    for k in 0i32..5 {
        v.push(TickInfo {
            tick: (k + 1) * 10,
            liquidity_net: -100_000_000_000i128,
        });
    }
    v
}

fn bench_sim_sweep(c: &mut Criterion) {
    let detection = build_universe();
    let reasons = Arc::new(ReasonRing::new());
    let candidates_count = Arc::new(AtomicU64::new(0));
    let gas_snap = GasSnapshot::default();

    let sink: crossbeam_queue::ArrayQueue<Candidate> = crossbeam_queue::ArrayQueue::new(4096);

    let mut group = c.benchmark_group("sim_sweep");

    // Single dirty pool.
    let pools_1 = vec![PoolId(0)];
    group.bench_function("1_dirty_pool", |b| {
        b.iter(|| {
            run_one_sim_sweep(
                black_box(&detection),
                black_box(&pools_1),
                &candidates_count,
                &reasons,
                &sink,
                &gas_snap,
            )
        })
    });

    // Two dirty pools (multi-pool dirty event, e.g. router multicall).
    let pools_2 = vec![PoolId(0), PoolId(1)];
    group.bench_function("2_dirty_pools", |b| {
        b.iter(|| {
            run_one_sim_sweep(
                black_box(&detection),
                black_box(&pools_2),
                &candidates_count,
                &reasons,
                &sink,
                &gas_snap,
            )
        })
    });

    // Four dirty pools (worst case in our universe).
    let pools_4 = vec![PoolId(0), PoolId(1), PoolId(2), PoolId(3)];
    group.bench_function("4_dirty_pools", |b| {
        b.iter(|| {
            run_one_sim_sweep(
                black_box(&detection),
                black_box(&pools_4),
                &candidates_count,
                &reasons,
                &sink,
                &gas_snap,
            )
        })
    });

    group.finish();
    eprintln!(
        "candidates emitted across all bench cases: {}",
        candidates_count.load(std::sync::atomic::Ordering::Relaxed)
    );
}

criterion_group!(benches, bench_sim_sweep);
criterion_main!(benches);
