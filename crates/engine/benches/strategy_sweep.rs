//! Per-strategy / per-hop sim-sweep latency bench.
//!
//! Companion to `sim_sweep.rs` (which sweeps by *number of dirty pools*). This one isolates the
//! end-to-end sim-sweep cost (filter cascade → sizing golden-section → canonical net profit) **per
//! routing strategy / hop count**: TwoHop (2), Triangle (3), FourHop (4). For each strategy we build
//! the same synthetic universe, enumerate routes of that hop-count, mark ONLY those hot, rebuild the
//! CSR, then time `run_one_sim_sweep` for a dirty pool that touches them. The reported time is the
//! realistic per-event compute for a dirty pool participating in routes of that hop count.

use std::sync::{atomic::AtomicU64, Arc};

use alloy_primitives::U256;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

use engine::pipeline::{run_one_sim_sweep, Candidate, DetectionContext};
use engine_amm::TickInfo;
use engine_detect::SelectorRegistry;
use engine_gas::GasSnapshot;
use engine_metrics::ReasonRing;
use engine_routes::{
    enumerate_k_hop_cycles, enumerate_triangles, enumerate_two_hop_routes, CsrBuilder, RouteTable,
};
use engine_state::{PoolId, PoolKind, RiskClass, TokenId, UniverseBuilder, V3State, VenueId};

fn synthetic_ticks() -> Vec<TickInfo> {
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
}

/// Which hop-strategy to make hot. `Hops(n)` keeps only n-hop routes hot.
#[derive(Clone, Copy)]
enum Strat {
    TwoHop,
    Triangle,
    FourHop,
}

/// Build a synthetic 4-token / 8-V3-pool universe, enumerate 2/3/4-hop routes, and mark hot ONLY
/// the routes whose hop-count matches `strat`. Returns the detection context.
fn build_for(strat: Strat) -> Arc<DetectionContext> {
    let mut ub = UniverseBuilder::new();
    let mut token_addrs = [[0u8; 20]; 4];
    for (i, t) in token_addrs.iter_mut().enumerate() {
        t[0] = i as u8 + 1;
    }
    for addr in token_addrs.iter() {
        ub.add_token(*addr, 18, RiskClass::Permissionless);
    }

    let make_v3 = |off: u64| V3State {
        sqrt_price_x96: U256::from(1u128 << 96).saturating_add(U256::from(off)),
        tick: 0,
        liquidity: 1_000_000_000_000_000_000u128,
        fee_pips: 500,
        tick_spacing: 10,
        initialised_ticks: synthetic_ticks(),
    };

    // Two pools per pair across all 6 token pairs → 12 pools. Ample connectivity for 4-hop cycles.
    let pairs = [
        (TokenId(0), TokenId(1)),
        (TokenId(0), TokenId(2)),
        (TokenId(0), TokenId(3)),
        (TokenId(1), TokenId(2)),
        (TokenId(1), TokenId(3)),
        (TokenId(2), TokenId(3)),
    ];
    let mut pool_idx = 0u32;
    for &(t0, t1) in &pairs {
        for _ in 0..2 {
            let mut addr = [0u8; 20];
            addr[0] = 0x80;
            addr[1..5].copy_from_slice(&pool_idx.to_be_bytes());
            let state = make_v3(pool_idx as u64);
            ub.add_v3_pool(
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

    let mut route_table = RouteTable::new();
    let want: u8 = match strat {
        Strat::TwoHop => {
            enumerate_two_hop_routes(&universe.pools, &mut route_table);
            2
        }
        Strat::Triangle => {
            enumerate_triangles(&universe.pools, &mut route_table);
            3
        }
        Strat::FourHop => {
            enumerate_k_hop_cycles(&universe.pools, 4, 4096, &mut route_table);
            4
        }
    };

    // Mark hot ONLY the routes with the target hop-count.
    let mut hot = 0usize;
    for id in 0..route_table.len() {
        let r = route_table.get_mut(engine_state::RouteId(id as u32));
        if r.hop_count == want {
            r.hot = true;
            hot += 1;
        } else {
            r.hot = false;
        }
    }

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
        "strategy {}-hop: pools={} routes={} hot={}",
        want,
        universe.pools.len(),
        route_table.len(),
        hot
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

fn bench_strategy_sweep(c: &mut Criterion) {
    let reasons = Arc::new(ReasonRing::new());
    let candidates_count = Arc::new(AtomicU64::new(0));
    let gas_snap = GasSnapshot::default();
    let sink: crossbeam_queue::ArrayQueue<Candidate> = crossbeam_queue::ArrayQueue::new(4096);
    let dirty = vec![PoolId(0)]; // one dirty pool — the realistic per-event trigger

    let mut group = c.benchmark_group("strategy_sweep");
    for (name, strat) in [
        ("2hop_TwoHop", Strat::TwoHop),
        ("3hop_Triangle", Strat::Triangle),
        ("4hop_FourHop", Strat::FourHop),
    ] {
        let detection = build_for(strat);
        group.bench_function(name, |b| {
            b.iter(|| {
                run_one_sim_sweep(
                    black_box(&detection),
                    black_box(&dirty),
                    &candidates_count,
                    &reasons,
                    &sink,
                    &gas_snap,
                )
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_strategy_sweep);
criterion_main!(benches);
