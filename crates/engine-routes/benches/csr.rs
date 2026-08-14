use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_routes::CsrBuilder;
use engine_state::{PoolId, RouteId};

fn build_csr(n_pools: u32, routes_per_pool: u32) -> engine_routes::Csr {
    let mut b = CsrBuilder::new(n_pools);
    for p in 0..n_pools {
        for r in 0..routes_per_pool {
            b.push(p, RouteId(p * routes_per_pool + r));
        }
    }
    b.build()
}

fn bench_csr_lookup(c: &mut Criterion) {
    // 80 hot pools (§5.5 hard cap) × 64 routes each = 5,120 hot routes.
    let csr = build_csr(80, 64);
    c.bench_function("csr_pool_to_hot_routes_slice", |b| {
        b.iter(|| {
            let s = csr.slice_for(black_box(PoolId(42)));
            assert_eq!(s.len(), 64);
        })
    });
}

fn bench_csr_build(c: &mut Criterion) {
    c.bench_function("csr_builder_80x64", |b| {
        b.iter(|| {
            let csr = build_csr(80, 64);
            assert_eq!(csr.total_inner(), 80 * 64);
        })
    });
}

criterion_group!(benches, bench_csr_lookup, bench_csr_build);
criterion_main!(benches);
