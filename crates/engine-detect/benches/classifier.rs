use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_detect::{ClassifierBuilder, Confidence, ConfidenceFactors};
use engine_state::PoolId;

fn bench_classify(c: &mut Criterion) {
    let mut b = ClassifierBuilder::new();
    // Seed with 256 pool addresses to mirror a hot universe of ~50–80 hot pools plus
    // warm overflow.
    for i in 0..256u32 {
        let mut a = [0u8; 20];
        a[..4].copy_from_slice(&i.to_be_bytes());
        b.add_pool(a, PoolId(i));
    }
    b.add_router([0x99u8; 20], 1);
    let classifier = b.build();

    let pool_addr = {
        let mut a = [0u8; 20];
        a[..4].copy_from_slice(&7u32.to_be_bytes());
        a
    };
    let router_data = {
        let mut d = vec![0u8; 8];
        d[..4].copy_from_slice(&[0xc0, 0x4b, 0x8d, 0x59]);
        d
    };

    c.bench_function("classify_known_pool", |c| {
        c.iter(|| classifier.classify(black_box(&pool_addr), black_box(&[])))
    });
    c.bench_function("classify_router_known", |c| {
        c.iter(|| classifier.classify(black_box(&[0x99u8; 20]), black_box(&router_data)))
    });
    c.bench_function("classify_unknown_ignored", |c| {
        c.iter(|| classifier.classify(black_box(&[0xff; 20]), black_box(&[0u8; 4])))
    });
}

fn bench_confidence(c: &mut Criterion) {
    let f = ConfidenceFactors::full();
    c.bench_function("confidence_chain_full", |b| {
        b.iter(|| Confidence::from_factors(black_box(f)))
    });
}

criterion_group!(benches, bench_classify, bench_confidence);
criterion_main!(benches);
