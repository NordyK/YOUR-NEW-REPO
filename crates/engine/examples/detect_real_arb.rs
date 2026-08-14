//! FULL-PIPELINE detector proof on REAL pre-state (runbook v1 Phase D2 / G — detector-generated).
//!
//! Unlike the kernel harness (engine-rpc/examples/gate3_detector.rs), this builds a real
//! `DetectionContext` from the two flagship pools' EXACT pre-state at block 465686728 and runs
//! the engine's OWN production sweep `engine::pipeline::run_one_sim_sweep` — the same function the
//! live `sim_loop` calls. It exercises CSR pool→route fan-out → `evaluate_route` → the live
//! `optimize_cycle_amount` sizer → canonical net-profit + EV gate → candidate sink. If a candidate
//! is emitted, the FULL detection path (not just the math kernel) discovered the real arb.
//!
//! Run: ARBITRUM_RPC_URL=… cargo run -p engine --example detect_real_arb --release
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::U256;
use engine::pipeline::{run_one_sim_sweep, Candidate, DetectionContext};
use engine_amm::TickInfo;
use engine_detect::SelectorRegistry;
use engine_gas::GasSnapshot;
use engine_metrics::ReasonRing;
use engine_routes::{enumerate_two_hop_routes, CsrBuilder, RouteTable};
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use engine_rpc::{fetch_v3_liquidity, fetch_v3_slot0, fetch_v3_tick_window};
use engine_state::{PoolKind, RiskClass, UniverseBuilder, V3State, VenueId};

const PRE: u64 = 465686728;
const PANCAKE: &str = "4bfc22a4da7f31f8a912a79a7e44a822398b4390"; // WBTC/WETH fee100 spacing1
const UNI: &str = "2f5e87c9312fa29aed5c179e456625d79015299c"; // WBTC/WETH fee500 spacing10
const WBTC: [u8; 20] = hexlit("2f2a2543b76a4166549f7aab2e75bef0aefc5b0f");
const WETH: [u8; 20] = hexlit("82af49447d8a07e3bd95bd0d56f35241523fbab1");

const fn hexlit(s: &str) -> [u8; 20] {
    let b = s.as_bytes();
    let mut out = [0u8; 20];
    let mut i = 0;
    while i < 20 {
        let hi = hexv(b[i * 2]);
        let lo = hexv(b[i * 2 + 1]);
        out[i] = hi * 16 + lo;
        i += 1;
    }
    out
}
const fn hexv(c: u8) -> u8 {
    if c >= b'0' && c <= b'9' {
        c - b'0'
    } else if c >= b'a' && c <= b'f' {
        c - b'a' + 10
    } else {
        0
    }
}
fn addr(s: &str) -> [u8; 20] {
    let s = s.trim_start_matches("0x");
    let mut o = [0u8; 20];
    for i in 0..20 {
        o[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    o
}

fn fetch_v3(
    rpc: &ArbitrumRpcClient,
    pool_hex: &str,
    fee: u32,
    spacing: i32,
    width: i32,
    blk: BlockTag,
) -> V3State {
    let pool = addr(pool_hex);
    let s0 = fetch_v3_slot0(rpc, &pool, blk).expect("slot0");
    let liq = fetch_v3_liquidity(rpc, &pool, blk).expect("liq");
    let win = fetch_v3_tick_window(rpc, &pool, s0.tick, spacing, width, blk).expect("ticks");
    let ticks: Vec<TickInfo> = win
        .iter()
        .map(|t| TickInfo {
            tick: t.tick,
            liquidity_net: t.liquidity_net,
        })
        .collect();
    eprintln!(
        "  pool 0x{pool_hex}: tick={} liq={} warm_ticks={}",
        s0.tick,
        liq,
        ticks.len()
    );
    V3State {
        sqrt_price_x96: s0.sqrt_price_x96,
        tick: s0.tick,
        liquidity: liq,
        fee_pips: fee,
        tick_spacing: spacing,
        initialised_ticks: ticks,
    }
}

fn main() {
    let rpc = ArbitrumRpcClient::new(std::env::var("ARBITRUM_RPC_URL").expect("ARBITRUM_RPC_URL"));
    let blk = BlockTag::Number(PRE);
    eprintln!("Loading REAL pre-state @ block {PRE} for both WBTC/WETH pools…");
    let pancake_state = fetch_v3(&rpc, PANCAKE, 100, 1, 400, blk);
    let uni_state = fetch_v3(&rpc, UNI, 500, 10, 120, blk);

    // Build the universe: WBTC(8dp) + WETH(18dp), both pools, real state.
    let mut ub = UniverseBuilder::new();
    let wbtc = ub.add_token(WBTC, 8, RiskClass::Permissionless);
    let weth = ub.add_token(WETH, 18, RiskClass::Permissionless);
    // eth-price (ETH-wei per 1 raw token unit) <<96. WETH identity; WBTC ≈ 36.5 WETH/BTC ⇒ 1 raw 8dp unit ≈ 3.65e11 ETH-wei.
    ub.set_token_eth_price_q96(weth, U256::from(1u128) << 96);
    ub.set_token_eth_price_q96(wbtc, U256::from(365_000_000_000u128) << 96);
    // token0=WBTC, token1=WETH on both pools (verified on-chain).
    let p_pan = ub
        .add_v3_pool(
            addr(PANCAKE),
            PoolKind::PancakeV3,
            wbtc,
            weth,
            VenueId(0),
            100,
            1,
            pancake_state,
        )
        .expect("add pancake");
    let p_uni = ub
        .add_v3_pool(
            addr(UNI),
            PoolKind::UniswapV3,
            wbtc,
            weth,
            VenueId(1),
            500,
            10,
            uni_state,
        )
        .expect("add uni");
    let universe = ub.build();

    // Enumerate 2-hop cycles, mark hot, build CSR.
    let mut rt = RouteTable::new();
    enumerate_two_hop_routes(&universe.pools, &mut rt);
    for id in 0..rt.len() {
        rt.get_mut(engine_state::RouteId(id as u32)).hot = true;
    }
    let mut csr = CsrBuilder::new(universe.pools.len() as u32);
    for r in rt.all() {
        for h in &r.hops {
            csr.push(h.pool.0, r.id);
        }
    }
    let pool_to_hot_routes = csr.build();
    eprintln!(
        "universe: pools={} routes={} (hot)",
        universe.pools.len(),
        rt.len()
    );

    let n = universe.pools.len();
    let detection = Arc::new(DetectionContext {
        universe: Arc::new(universe),
        route_table: rt,
        pool_to_hot_routes,
        selectors: SelectorRegistry::canonical(),
        telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![1u64 << 32; n])),
        family_quote_active: engine::runtime_policy::default_active_families().dense(),
    });

    // Run the engine's OWN production sweep with BOTH pools dirty (as a swap on either would trigger).
    let reasons = Arc::new(ReasonRing::new());
    let cand_count = Arc::new(AtomicU64::new(0));
    let gas = GasSnapshot::default();
    let sink: crossbeam_queue::ArrayQueue<Candidate> = crossbeam_queue::ArrayQueue::new(4096);
    let dirty = vec![p_pan, p_uni];
    run_one_sim_sweep(&detection, &dirty, &cand_count, &reasons, &sink, &gas);

    let n_cands = sink.len();
    println!("\n=== FULL-PIPELINE DETECTOR RESULT ===");
    println!(
        "candidates emitted by run_one_sim_sweep: {n_cands} (counter={})",
        cand_count.load(Ordering::Relaxed)
    );
    if n_cands > 0 {
        println!("GATE D2/G: PASS — the engine's production detection sweep (CSR fan-out → evaluate_route");
        println!(
            "           → optimize_cycle_amount sizer → net-profit + EV gate) emitted a candidate"
        );
        println!("           for the real WBTC/WETH arb at the exact pre-state. (profit/size = 16.30 WETH");
        println!("           → 0.0494 WETH, established to the wei by gate3_detector + the Solidity executor.)");
    } else {
        println!(
            "GATE D2/G: NO candidate emitted — net/EV gate or gas rejected it. counter={}",
            cand_count.load(Ordering::Relaxed)
        );
    }
}
