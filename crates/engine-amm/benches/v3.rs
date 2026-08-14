use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_amm::{
    next_sqrt_price_from_amount0_in, next_sqrt_price_from_amount1_in, sqrt_price_at_tick,
    v3_get_amount_out, TickInfo, V3Slot0,
};
use ruint::aliases::U256;

fn bench_sqrt_price_at_tick(c: &mut Criterion) {
    c.bench_function("v3_sqrt_price_at_tick_0", |b| {
        b.iter(|| sqrt_price_at_tick(black_box(0)).unwrap())
    });
    c.bench_function("v3_sqrt_price_at_tick_200000", |b| {
        b.iter(|| sqrt_price_at_tick(black_box(200_000)).unwrap())
    });
    c.bench_function("v3_sqrt_price_at_tick_neg_200000", |b| {
        b.iter(|| sqrt_price_at_tick(black_box(-200_000)).unwrap())
    });
}

fn bench_next_sqrt(c: &mut Criterion) {
    let s = sqrt_price_at_tick(0).unwrap();
    let liq: u128 = 1_000_000_000_000_000_000;
    let amount = U256::from(1_000_000u64);
    c.bench_function("v3_next_sqrt_from_amount0", |b| {
        b.iter(|| {
            next_sqrt_price_from_amount0_in(black_box(s), black_box(liq), black_box(amount))
                .unwrap()
        })
    });
    c.bench_function("v3_next_sqrt_from_amount1", |b| {
        b.iter(|| {
            next_sqrt_price_from_amount1_in(black_box(s), black_box(liq), black_box(amount))
                .unwrap()
        })
    });
}

fn bench_v3_swap_no_cross(c: &mut Criterion) {
    let slot0 = V3Slot0 {
        sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
        tick: 0,
        fee_pips: 3000,
        liquidity: 1_000_000_000_000_000_000u128,
        tick_spacing: 60,
    };
    let ticks = vec![
        TickInfo {
            tick: -10_000,
            liquidity_net: 0,
        },
        TickInfo {
            tick: 10_000,
            liquidity_net: 0,
        },
    ];
    let amt = U256::from(1_000_000u64);
    c.bench_function("v3_swap_no_tick_cross", |b| {
        b.iter(|| {
            v3_get_amount_out(
                black_box(slot0),
                black_box(amt),
                black_box(true),
                black_box(&ticks),
            )
            .unwrap()
        })
    });
}

fn bench_v3_swap_tick_cross(c: &mut Criterion) {
    let slot0 = V3Slot0 {
        sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
        tick: 0,
        fee_pips: 500,
        liquidity: 1_000_000_000_000_000_000u128,
        tick_spacing: 10,
    };
    // Warmed window with several initialised ticks the swap will need to cross.
    let ticks = vec![
        TickInfo {
            tick: -1000,
            liquidity_net: 100_000,
        },
        TickInfo {
            tick: -500,
            liquidity_net: 50_000,
        },
        TickInfo {
            tick: -100,
            liquidity_net: 20_000,
        },
        TickInfo {
            tick: 100,
            liquidity_net: -20_000,
        },
        TickInfo {
            tick: 500,
            liquidity_net: -50_000,
        },
        TickInfo {
            tick: 1000,
            liquidity_net: -100_000,
        },
    ];
    let amt = U256::from(10_000_000_000u64);
    c.bench_function("v3_swap_with_tick_cross", |b| {
        b.iter(|| {
            let _ = v3_get_amount_out(
                black_box(slot0),
                black_box(amt),
                black_box(true),
                black_box(&ticks),
            );
        })
    });
}

criterion_group!(
    benches,
    bench_sqrt_price_at_tick,
    bench_next_sqrt,
    bench_v3_swap_no_cross,
    bench_v3_swap_tick_cross
);
criterion_main!(benches);
