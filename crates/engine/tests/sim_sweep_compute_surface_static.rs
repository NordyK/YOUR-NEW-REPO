use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::U256;
use crossbeam_queue::ArrayQueue;

use engine::pipeline::{run_one_sim_sweep, Candidate, DetectionContext};
use engine_gas::GasSnapshot;
use engine_metrics::ReasonRing;
use engine_routes::{enumerate_two_hop_routes, CsrBuilder, RouteTable};
use engine_state::{PoolId, PoolKind, RiskClass, UniverseBuilder, V2Reserves256, V2State, VenueId};

#[derive(Debug)]
struct SweepStats {
    candidate_counter: u64,
    sink_len: usize,
    touched_pool_candidate_counts: Vec<u64>,
    all_pool_candidate_counts: Vec<u64>,
}

fn profitable_v2_detection_context() -> Arc<DetectionContext> {
    let mut ub = UniverseBuilder::new();
    let weth = ub.add_token([0x82; 20], 18, RiskClass::Permissionless);
    let usdc = ub.add_token([0xaf; 20], 6, RiskClass::CentralisedStable);
    ub.set_token_eth_price_q96(weth, U256::from(1u128) << 96);
    ub.set_token_eth_price_q96(usdc, U256::from(250_000_000u128) << 96);

    ub.add_v2_pool(
        [0xa1; 20],
        PoolKind::UniswapV2,
        weth,
        usdc,
        VenueId(0),
        30,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 4_000_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .expect("add high-price V2 pool");
    ub.add_v2_pool(
        [0xa2; 20],
        PoolKind::UniswapV2,
        weth,
        usdc,
        VenueId(1),
        30,
        V2State {
            reserves: V2Reserves256 {
                reserve0: 1_000_000_000_000_000_000_000,
                reserve1: 3_880_000_000_000_000_000_000,
                block_timestamp_last: 0,
            },
        },
    )
    .expect("add low-price V2 pool");

    let universe = ub.build();
    let mut route_table = RouteTable::new();
    enumerate_two_hop_routes(&universe.pools, &mut route_table);
    assert!(
        route_table.len() >= 2,
        "synthetic V2 universe must enumerate both route directions"
    );

    for route_id in 0..route_table.len() {
        route_table
            .get_mut(engine_state::RouteId(route_id as u32))
            .hot = true;
    }

    let mut csr = CsrBuilder::new(universe.pools.len() as u32);
    for route in route_table.all() {
        for hop in &route.hops {
            csr.push(hop.pool.0, route.id);
        }
    }

    let n_pools = universe.pools.len();
    Arc::new(DetectionContext {
        universe: Arc::new(universe),
        route_table,
        pool_to_hot_routes: csr.build(),
        selectors: engine_detect::SelectorRegistry::canonical(),
        telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![
            1u64 << 32;
            n_pools
        ])),
        family_quote_active: engine::runtime_policy::default_active_families().dense(),
    })
}

fn run_sweep(detection: &Arc<DetectionContext>, dirty_pools: &[PoolId]) -> SweepStats {
    let candidates = Arc::new(AtomicU64::new(0));
    let reasons = Arc::new(ReasonRing::new());
    let sink: ArrayQueue<Candidate> = ArrayQueue::new(64);

    run_one_sim_sweep(
        detection,
        dirty_pools,
        &candidates,
        &reasons,
        &sink,
        &GasSnapshot::default(),
    );

    let touched_pool_candidate_counts = dirty_pools
        .iter()
        .map(|pool| detection.telemetry.snapshot(pool.0 as usize).1)
        .collect();
    let all_pool_candidate_counts = (0..detection.universe.pools.len())
        .map(|pool| detection.telemetry.snapshot(pool).1)
        .collect();
    SweepStats {
        candidate_counter: candidates.load(Ordering::Relaxed),
        sink_len: sink.len(),
        touched_pool_candidate_counts,
        all_pool_candidate_counts,
    }
}

#[test]
fn sim_sweep_dirty_pool_reaches_quote_sizing_and_candidate_sink() {
    let detection = profitable_v2_detection_context();
    let stats = run_sweep(&detection, &[PoolId(0)]);

    assert!(
        stats.candidate_counter >= 1,
        "dirty pool should reach candidate accounting; stats={stats:?}"
    );
    assert_eq!(
        stats.sink_len, stats.candidate_counter as usize,
        "candidate sink length and counter should agree"
    );
    assert!(
        stats
            .touched_pool_candidate_counts
            .iter()
            .all(|count| *count > 0),
        "touched pool telemetry should record candidate-producing routes; stats={stats:?}"
    );
    assert_eq!(
        stats.all_pool_candidate_counts,
        vec![1, 1],
        "candidate telemetry should be recorded for every pool in the two-hop route"
    );
}
