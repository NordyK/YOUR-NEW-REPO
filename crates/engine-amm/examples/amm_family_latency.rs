//! Per-AMM-family quote latency — the system supports far more than V2/V3.
//!
//! Times one exact-in quote per family over many iterations and reports latency percentiles.
//! Run: cargo run --release -p engine-amm --example amm_family_latency -- [iterations]

use std::time::Instant;

use alloy_primitives::U256;
use hdrhistogram::Histogram;

use engine_amm::{
    balancer_stable, balancer_weighted, curve, dodo_v2, kyber_elastic, solidly_stable,
    trader_joe_lb, uniswap_v3, v2_get_amount_out, v3_get_amount_out, CurveStableSwapState,
    TickInfo, V3Slot0,
};

fn pctl(name: &str, h: &Histogram<u64>) {
    let us = |ns: u64| ns as f64 / 1_000.0;
    println!(
        "{name:<22} n={:>8} | p50 {:>9.3}ns  p90 {:>9.3}ns  p99 {:>9.3}ns  max {:>9.3}ns  | p50 {:>7.4}us  p99 {:>7.4}us",
        h.len(),
        h.value_at_quantile(0.50) as f64,
        h.value_at_quantile(0.90) as f64,
        h.value_at_quantile(0.99) as f64,
        h.max() as f64,
        us(h.value_at_quantile(0.50)),
        us(h.value_at_quantile(0.99)),
    );
}

fn bench<F: FnMut()>(name: &str, iters: usize, mut f: F) {
    for _ in 0..2000 {
        f();
    }
    let mut h = Histogram::<u64>::new_with_bounds(1, 10_000_000, 3).unwrap();
    for _ in 0..iters {
        let t = Instant::now();
        f();
        let _ = h.record((t.elapsed().as_nanos() as u64).max(1));
    }
    pctl(name, &h);
}

fn main() {
    let iters: usize = std::env::args()
        .next_back()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    println!("# Per-AMM-family exact-in quote latency  iters={iters}\n");

    // --- Uniswap V2 (constant product) ---
    bench("UniswapV2 (xy=k)", iters, || {
        let _ = std::hint::black_box(
            v2_get_amount_out(1_000_000, 1_000_000_000, 4_000_000_000, 30).unwrap(),
        );
    });

    // --- Uniswap V3 (concentrated liquidity), no tick cross ---
    let slot0 = V3Slot0 {
        sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336u128), // ~price 1
        tick: 0,
        fee_pips: 500,
        liquidity: 50_000_000_000_000u128,
        tick_spacing: 10,
    };
    let ticks_v3: Vec<TickInfo> = vec![
        TickInfo {
            tick: -200,
            liquidity_net: 10_000_000_000_000,
        },
        TickInfo {
            tick: 200,
            liquidity_net: -10_000_000_000_000,
        },
    ];
    bench("UniswapV3 (CL)", iters, || {
        let _ = std::hint::black_box(
            v3_get_amount_out(slot0, U256::from(1_000_000u64), true, &ticks_v3).unwrap(),
        );
    });

    // --- Curve StableSwap-NG ---
    let mut cs = CurveStableSwapState {
        n_coins: 2,
        balances: [0; curve::MAX_COINS],
        rates: [0; curve::MAX_COINS],
        a_param: 200 * 100,
        fee: 1_000_000, // 1 bp in 1e10
        offpeg_fee_multiplier: 0,
    };
    cs.balances[0] = 1_000_000_000_000_000_000_000_000; // 1e24
    cs.balances[1] = 1_000_000_000_000_000_000_000_000;
    cs.rates[0] = 1_000_000_000_000_000_000; // 1e18
    cs.rates[1] = 1_000_000_000_000_000_000;
    bench("CurveStableSwap", iters, || {
        let _ = std::hint::black_box(curve::get_dy(&cs, 0, 1, 1_000_000_000_000_000_000).unwrap());
    });

    // --- Balancer weighted (80/20) ---
    bench("BalancerWeighted", iters, || {
        let _ = std::hint::black_box(
            balancer_weighted::calc_out_given_in(
                U256::from(1_000_000_000_000_000_000_000u128), // balance_in 1e21
                U256::from(800_000_000_000_000_000u128),       // weight 0.8
                U256::from(4_000_000_000_000_000_000_000u128), // balance_out 4e21
                U256::from(200_000_000_000_000_000u128),       // weight 0.2
                U256::from(1_000_000_000_000_000_000u128),     // amount_in 1e18
            )
            .unwrap(),
        );
    });

    // --- Balancer stable (ComposableStable) ---
    let mut bs = balancer_stable::BalancerStableState {
        n: 2,
        balances: [0; balancer_stable::MAX_TOKENS],
        scaling_factors: [U256::ZERO; balancer_stable::MAX_TOKENS],
        amp: 500 * 1000,
        swap_fee_1e18: 1_000_000_000_000_000, // 0.1%
    };
    bs.balances[0] = 1_000_000_000_000; // 1e12 (6-dec)
    bs.balances[1] = 1_000_000_000_000;
    bs.scaling_factors[0] = U256::from(1_000_000_000_000_000_000_000_000_000_000u128); // 1e30 (6->18)
    bs.scaling_factors[1] = U256::from(1_000_000_000_000_000_000_000_000_000_000u128);
    bench("BalancerStable", iters, || {
        let _ = std::hint::black_box(
            balancer_stable::calc_out_given_in(&bs, 0, 1, 1_000_000_000).unwrap(),
        );
    });

    // --- Solidly stable (x3y+y3x) ---
    bench("SolidlyStable", iters, || {
        let _ = std::hint::black_box(
            solidly_stable::calculate_stable_swap_output(
                U256::from(1_000_000_000_000_000_000_000u128),
                U256::from(1_000_000_000_000_000_000_000u128),
                U256::from(1_000_000_000_000_000_000u128),
                5,
            )
            .unwrap(),
        );
    });

    // --- Trader Joe LB (bin) ---
    bench("TraderJoeLB (bin)", iters, || {
        let _ = std::hint::black_box(trader_joe_lb::quote_single_bin(
            1_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000,
            25,
            8_388_608,
            true,
            1_000_000_000_000_000, // 0.1% fee 1e18
        ));
    });

    // --- KyberSwap Elastic ---
    let ticks_k: Vec<TickInfo> = vec![
        TickInfo {
            tick: -201_290,
            liquidity_net: 1_000_000_000_000,
        },
        TickInfo {
            tick: -201_270,
            liquidity_net: -1_000_000_000_000,
        },
    ];
    let sqrt_k = uniswap_v3::sqrt_price_at_tick(-201_280).unwrap();
    bench("KyberElastic (CL)", iters, || {
        let _ = std::hint::black_box(
            kyber_elastic::get_amount_out(
                1_000_000_000,
                sqrt_k,
                U256::from(26_000_000_000_000u128),
                -201_280,
                3_000,
                true,
                &ticks_k,
            )
            .unwrap(),
        );
    });

    // --- DODO V2 (PMM) ---
    let dodo = dodo_v2::dodo_state_from_view(
        alloy_primitives::Address::from([0x66u8; 20]),
        dodo_v2::DodoPoolClass::Dpp,
        alloy_primitives::Address::from([0x44u8; 20]),
        alloy_primitives::Address::from([0x55u8; 20]),
        U256::from(1_000_000_000_000_000_000u128), // i (price param)
        U256::from(100_000_000_000_000_000u128),   // K
        U256::from(1_000_000_000_000_000_000_000u128), // B reserve
        U256::from(1_000_000_000_000_000_000_000u128), // Q reserve
        U256::from(1_000_000_000_000_000_000_000u128), // B0 target
        U256::from(1_000_000_000_000_000_000_000u128), // Q0 target
        U256::from(0u8),                           // R flag
        U256::from(3_000_000_000_000_000u64),      // lp fee
        U256::from(1_000_000_000_000_000u64),      // mt fee
        1,
        0,
    )
    .expect("dodo state");
    bench("DodoV2 (PMM)", iters, || {
        let _ = std::hint::black_box(
            dodo.quote_exact_in_local(
                U256::from(1_000_000_000_000_000_000u128),
                dodo_v2::SwapDirection::BaseToQuote,
            )
            .map(|q| q.amount_out_native),
        );
    });
}
