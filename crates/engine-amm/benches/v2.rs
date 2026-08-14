use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_amm::{v2_get_amount_in, v2_get_amount_out};

fn bench_v2_get_amount_out(c: &mut Criterion) {
    let r_in: u128 = 1_000_000_000_000_000_000_000;
    let r_out: u128 = 2_000_000_000_000_000_000_000;
    let amt: u128 = 1_000_000_000_000_000_000;
    c.bench_function("v2_get_amount_out_30bps", |b| {
        b.iter(|| {
            v2_get_amount_out(
                black_box(amt),
                black_box(r_in),
                black_box(r_out),
                black_box(30),
            )
            .unwrap()
        })
    });
    c.bench_function("v2_get_amount_out_10bps_camelot", |b| {
        b.iter(|| {
            v2_get_amount_out(
                black_box(amt),
                black_box(r_in),
                black_box(r_out),
                black_box(10),
            )
            .unwrap()
        })
    });
}

fn bench_v2_get_amount_in(c: &mut Criterion) {
    let r_in: u128 = 1_000_000_000_000_000_000_000;
    let r_out: u128 = 2_000_000_000_000_000_000_000;
    let target_out: u128 = 1_000_000_000_000_000_000;
    c.bench_function("v2_get_amount_in_30bps", |b| {
        b.iter(|| {
            v2_get_amount_in(
                black_box(target_out),
                black_box(r_in),
                black_box(r_out),
                black_box(30),
            )
            .unwrap()
        })
    });
}

criterion_group!(benches, bench_v2_get_amount_out, bench_v2_get_amount_in);
criterion_main!(benches);
