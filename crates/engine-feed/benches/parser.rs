use base64_simd::STANDARD as BASE64;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_feed::{decode_envelope, decode_l2msg};

fn make_signed_tx_l2msg() -> Vec<u8> {
    let mut inner = Vec::new();
    inner.push(0x04u8); // kind=4 SignedTx
    let payload: Vec<u8> = (0..9).map(|_| 0x80u8).collect(); // 9 empty bytes
    inner.push(0xc9);
    inner.extend_from_slice(&payload);
    inner
}

fn make_envelope() -> Vec<u8> {
    let inner = make_signed_tx_l2msg();
    let b64 = BASE64.encode_to_string(&inner);
    format!(
        r#"{{"version":1,"messages":[{{"sequenceNumber":12345,"message":{{"message":{{"header":{{"kind":3,"sender":"0x","blockNumber":22300000,"timestamp":1715000000,"requestId":null,"baseFeeL1":null}},"l2Msg":"{b64}"}}}}}}]}}"#,
    )
    .into_bytes()
}

fn bench_decode_envelope(c: &mut Criterion) {
    let raw = make_envelope();
    c.bench_function("feed_decode_envelope", |b| {
        b.iter(|| decode_envelope(black_box(&raw)).unwrap())
    });
}

fn bench_decode_l2msg(c: &mut Criterion) {
    let payload = make_signed_tx_l2msg();
    c.bench_function("feed_decode_l2msg_signed_tx", |b| {
        b.iter(|| decode_l2msg(black_box(&payload)).unwrap())
    });
}

fn bench_base64_inplace(c: &mut Criterion) {
    // Simulate parser's in-place base64 decode step.
    let inner = make_signed_tx_l2msg();
    let b64 = BASE64.encode_to_string(&inner).into_bytes();
    c.bench_function("feed_base64_simd_inplace", |b| {
        b.iter_with_setup(
            || b64.clone(),
            |mut buf| {
                let _ = base64_simd::forgiving_decode_inplace(&mut buf)
                    .unwrap()
                    .len();
            },
        )
    });
}

criterion_group!(
    benches,
    bench_decode_envelope,
    bench_decode_l2msg,
    bench_base64_inplace
);
criterion_main!(benches);
