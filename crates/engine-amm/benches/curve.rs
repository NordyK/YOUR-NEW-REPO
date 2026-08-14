use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_amm::{curve_get_dy, CurveStableSwapState, CURVE_A_PRECISION};

fn bench_curve_get_dy_2coin(c: &mut Criterion) {
    let state = CurveStableSwapState {
        n_coins: 2,
        balances: [
            1_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
        rates: [
            1_000_000_000_000_000_000,
            1_000_000_000_000_000_000,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
        a_param: 200 * CURVE_A_PRECISION,
        fee: 4_000_000,
        offpeg_fee_multiplier: 0,
    };
    let dx: u128 = 1_000_000_000_000_000_000;
    c.bench_function("curve_get_dy_2coin", |b| {
        b.iter(|| curve_get_dy(black_box(&state), 0, 1, black_box(dx)).unwrap())
    });
}

fn bench_curve_get_dy_3coin(c: &mut Criterion) {
    let state = CurveStableSwapState {
        n_coins: 3,
        balances: [
            1_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000,
            0,
            0,
            0,
            0,
            0,
        ],
        rates: [
            1_000_000_000_000_000_000,
            1_000_000_000_000_000_000,
            1_000_000_000_000_000_000,
            0,
            0,
            0,
            0,
            0,
        ],
        a_param: 200 * CURVE_A_PRECISION,
        fee: 4_000_000,
        offpeg_fee_multiplier: 0,
    };
    let dx: u128 = 1_000_000_000_000_000_000;
    c.bench_function("curve_get_dy_3coin", |b| {
        b.iter(|| curve_get_dy(black_box(&state), 0, 1, black_box(dx)).unwrap())
    });
}

criterion_group!(benches, bench_curve_get_dy_2coin, bench_curve_get_dy_3coin);
criterion_main!(benches);
