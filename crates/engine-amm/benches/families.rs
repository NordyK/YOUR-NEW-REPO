//! Per-family kernel quote benchmarks — the fast-lane unit cost for every "exotic" family that
//! has its own quote arm in `quote_one_hop`. The V2/V3/Curve-stable kernels already have dedicated
//! benches (`v2.rs`/`v3.rs`/`curve.rs`); this file covers the rest so the bench suite measures the
//! hot-path quote cost of EVERY live family, not just the byte-exact ones. States are the same
//! reference fixtures the kernels' unit tests use (so the inputs are realistic, not degenerate).

use std::collections::BTreeMap;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

// ---- DODO V2 (PMM) ----
fn bench_dodo(c: &mut Criterion) {
    use engine_amm::dodo_v2::{DodoPoolClass, DodoRState, DodoState};
    const D18: u128 = 1_000_000_000_000_000_000;
    let base = Address::from([0x22; 20]);
    let quote = Address::from([0x33; 20]);
    let state = DodoState {
        address: Address::from([0x11; 20]),
        pool_class: DodoPoolClass::Dvm,
        base_token: base,
        quote_token: quote,
        i: U256::from(2u8) * U256::from(D18),
        k: U256::from(D18 / 2),
        base_reserve: U256::from(100u64) * U256::from(D18),
        quote_reserve: U256::from(200u64) * U256::from(D18),
        base_target: U256::from(100u64) * U256::from(D18),
        quote_target: U256::from(200u64) * U256::from(D18),
        r_state: DodoRState::One,
        lp_fee_rate_d18: U256::from(3_000_000_000_000_000u64),
        mt_fee_rate_d18: U256::from(1_000_000_000_000_000u64),
        fee_ppm: 4_000,
        block_number: 1,
        log_index: 0,
    };
    let dx = U256::from(D18); // 1 base token
    c.bench_function("dodo_quote_base_to_quote", |b| {
        b.iter(|| {
            state
                .quote_for_tokens_exact_in(black_box(base), black_box(quote), black_box(dx))
                .unwrap()
        })
    });
}

// ---- Balancer V2 Stable (StableMath) ----
fn bench_balancer_stable(c: &mut Criterion) {
    use engine_amm::balancer_stable::{
        calc_out_given_in, BalancerStableState, AMP_PRECISION, MAX_TOKENS,
    };
    const ONE: u128 = 1_000_000_000_000_000_000;
    let bal = 1_000_000u128 * ONE;
    let mut balances = [0u128; MAX_TOKENS];
    balances[0] = bal;
    balances[1] = bal;
    let mut scaling = [U256::ZERO; MAX_TOKENS];
    scaling[0] = U256::from(ONE);
    scaling[1] = U256::from(ONE);
    let st = BalancerStableState {
        n: 2,
        balances,
        scaling_factors: scaling,
        amp: 1000 * AMP_PRECISION,
        swap_fee_1e18: 1_000_000_000_000_000, // 0.1%
    };
    let dx = 1000u128 * ONE;
    c.bench_function("balancer_stable_calc_out_given_in", |b| {
        b.iter(|| calc_out_given_in(black_box(&st), 0, 1, black_box(dx)).unwrap())
    });
}

// ---- Balancer V2 Weighted ----
fn bench_balancer_weighted(c: &mut Criterion) {
    use engine_amm::balancer_weighted::calc_out_given_in;
    let one = U256::from(1_000_000_000_000_000_000u64);
    // 80/20 pool, both balances 1e24 (1M tokens at 1e18).
    let bal_in = U256::from(1_000_000u64) * one;
    let bal_out = U256::from(1_000_000u64) * one;
    let w_in = U256::from(800_000_000_000_000_000u64); // 0.8
    let w_out = U256::from(200_000_000_000_000_000u64); // 0.2
    let amt = U256::from(1000u64) * one;
    c.bench_function("balancer_weighted_calc_out_given_in", |b| {
        b.iter(|| {
            calc_out_given_in(
                black_box(bal_in),
                black_box(w_in),
                black_box(bal_out),
                black_box(w_out),
                black_box(amt),
            )
            .unwrap()
        })
    });
}

// ---- Trader Joe Liquidity Book (bin-AMM) ----
fn bench_lb(c: &mut Criterion) {
    use engine_amm::trader_joe_lb::{simulate_lb_swap, BinData, LBState};
    let active = 8_388_608u32;
    let mut bin_map = BTreeMap::new();
    // Two neighbour bins above active hold X reserves (Y→X traverses upward).
    for off in 1..=2u32 {
        bin_map.insert(
            active + off,
            BinData {
                reserve_x: 1_000_000_000_000_000_000u128,
                reserve_y: 0,
            },
        );
    }
    let state = LBState {
        active_id: active,
        bin_step: 20,
        reserve_x: 1_000_000_000_000_000_000u128,
        reserve_y: 0,
        base_factor: 5000,
        filter_period: 0,
        decay_period: 0,
        reduction_factor: 0,
        variable_fee_control: 0,
        protocol_share: 0,
        max_volatility_accumulator: 0,
        volatility_accumulator: 0,
        volatility_reference: 0,
        id_reference: 0,
        time_of_last_update: 0,
        block_number: 0,
        log_index: 0,
        stale: false,
        bin_map: Arc::new(bin_map),
    };
    // Y→X swap that crosses into the neighbour bins (exercises the bin-traversal loop).
    let amt = U256::from(500_000_000_000_000_000u64);
    c.bench_function("lb_simulate_swap_multi_bin", |b| {
        b.iter(|| simulate_lb_swap(black_box(&state), black_box(amt), false))
    });
}

// ---- Solidly stable (x³y + xy³) decimal-normalised ----
fn bench_solidly(c: &mut Criterion) {
    use engine_amm::solidly_stable::calculate_stable_swap_output_normalized;
    // Cross-decimal stable pair (USDC 6dp ↔ DAI 18dp) — the path the normalised solver exists for.
    let r_in = U256::from(1_000_000_000_000u64); // 1e6 USDC (6dp) → 1,000,000 USDC
    let r_out = U256::from(1_000_000u64) * U256::from(1_000_000_000_000_000_000u64); // 1e6 DAI (18dp)
    let amt = U256::from(1_000_000_000u64); // 1000 USDC
    c.bench_function("solidly_stable_swap_cross_decimal", |b| {
        b.iter(|| {
            calculate_stable_swap_output_normalized(
                black_box(r_in),
                black_box(r_out),
                black_box(amt),
                5,  // 0.05% fee bps
                6,  // decimals_in
                18, // decimals_out
            )
            .unwrap()
        })
    });
}

// ---- Curve CryptoSwap-NG (Tricrypto, 3-coin) ----
fn bench_cryptoswap(c: &mut Criterion) {
    use engine_amm::curve_cryptoswap::CryptoSwapState;
    // Same balanced reference fixture the kernel's reference-vector test uses.
    let u = |x: u128| U256::from(x);
    let mut state = CryptoSwapState {
        balances: [
            u(5_000_000_000_000),
            u(8_000_000_000),
            u(1_600_000_000_000_000_000_000),
        ],
        precisions: [u(1_000_000_000_000), u(10_000_000_000), U256::ONE],
        price_scale: [
            u(60_000_000_000_000_000_000_000),
            u(3_000_000_000_000_000_000_000),
        ],
        ann: u(1_707_629),
        gamma: u(11_809_167_828_997),
        mid_fee: u(3_000_000),
        out_fee: u(30_000_000),
        fee_gamma: u(500_000_000_000_000),
        d: U256::ZERO,
        last_k0_prev: U256::ZERO,
    };
    state.recompute_d().expect("reference pool converges");
    // Swap a small amount of coin0 (USDC-like) for coin2 (ETH-like).
    let dx = u(1_000_000_000); // 1000 units at 1e6
    c.bench_function("cryptoswap_get_amount_out_3coin", |b| {
        b.iter(|| state.get_amount_out_exact(0, 2, black_box(dx)).unwrap())
    });
}

criterion_group!(
    benches,
    bench_dodo,
    bench_balancer_stable,
    bench_balancer_weighted,
    bench_lb,
    bench_solidly,
    bench_cryptoswap
);
criterion_main!(benches);
