use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_detect::{filters::best_v2v2_bucket, marginal_rate_product_v2v2, route_passes_filters};

fn bench_marginal_rate(c: &mut Criterion) {
    c.bench_function("filter_marginal_rate_v2v2", |b| {
        b.iter(|| {
            marginal_rate_product_v2v2(
                black_box(1_000),
                black_box(2_000),
                black_box(30),
                black_box(2_000),
                black_box(1_010),
                black_box(30),
                black_box(0),
            )
        })
    });
}

fn bench_grid_scan(c: &mut Criterion) {
    let grid = vec![1_000u128, 10_000, 100_000, 1_000_000];
    c.bench_function("filter_best_v2v2_bucket_4", |b| {
        b.iter(|| {
            best_v2v2_bucket(
                black_box(&grid),
                black_box(1_000_000_000u128),
                black_box(4_000_000_000u128),
                black_box(30),
                black_box(4_000_000_000u128),
                black_box(1_010_000_000u128),
                black_box(30),
            )
        })
    });
}

fn bench_route_filters(c: &mut Criterion) {
    let expected = vec![1u64, 2, 3, 4, 5, 6, 7, 8];
    let current = expected.clone();
    c.bench_function("filter_route_passes", |b| {
        b.iter(|| {
            route_passes_filters(
                black_box(&expected),
                black_box(&current),
                black_box(0),
                black_box(0),
                black_box(0),
                black_box(true),
                black_box(true),
                black_box(true),
            )
        })
    });
}

criterion_group!(
    benches,
    bench_marginal_rate,
    bench_grid_scan,
    bench_route_filters
);
criterion_main!(benches);
