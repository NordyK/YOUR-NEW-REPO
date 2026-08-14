use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_feed::address::keccak256;
use engine_submit::{
    build_send_raw_tx_envelope, encode_eip1559_signed, encode_eip1559_unsigned, sign_eip1559,
    Eip1559Tx, SigningKey,
};
use ruint::aliases::U256;

fn make_tx() -> Eip1559Tx {
    Eip1559Tx {
        chain_id: 42_161,
        nonce: 7,
        max_priority_fee_per_gas: U256::ZERO,
        max_fee_per_gas: U256::from(40_000_000u64),
        gas_limit: 500_000,
        to: [0xeeu8; 20],
        value: U256::ZERO,
        data: vec![0u8; 356], // executor calldata: execute(pathId, amountIn, minProfit, hopMinOuts[8])
    }
}

fn bench_rlp_encode_unsigned(c: &mut Criterion) {
    let tx = make_tx();
    c.bench_function("submit_rlp_encode_unsigned", |b| {
        b.iter(|| encode_eip1559_unsigned(black_box(&tx)))
    });
}

fn bench_keccak_signing_hash(c: &mut Criterion) {
    let tx = make_tx();
    let unsigned = encode_eip1559_unsigned(&tx);
    c.bench_function("submit_keccak256_signing_hash", |b| {
        b.iter(|| keccak256(black_box(&unsigned)))
    });
}

fn bench_secp256k1_sign(c: &mut Criterion) {
    let tx = make_tx();
    let unsigned = encode_eip1559_unsigned(&tx);
    let hash = keccak256(&unsigned);
    let key = SigningKey::from_bytes(&[0x11u8; 32]).unwrap();
    c.bench_function("submit_secp256k1_sign", |b| {
        b.iter(|| sign_eip1559(black_box(&key), black_box(&hash)).unwrap())
    });
}

fn bench_jsonrpc_envelope(c: &mut Criterion) {
    let tx = make_tx();
    let unsigned = encode_eip1559_unsigned(&tx);
    let hash = keccak256(&unsigned);
    let key = SigningKey::from_bytes(&[0x11u8; 32]).unwrap();
    let sig = sign_eip1559(&key, &hash).unwrap();
    let raw = encode_eip1559_signed(&tx, sig);
    let mut buf = Vec::with_capacity(512);
    c.bench_function("submit_jsonrpc_envelope", |b| {
        b.iter(|| build_send_raw_tx_envelope(black_box(&raw), black_box(1), black_box(&mut buf)))
    });
}

fn bench_end_to_end_sign_and_serialize(c: &mut Criterion) {
    let tx = make_tx();
    let key = SigningKey::from_bytes(&[0x11u8; 32]).unwrap();
    let mut buf = Vec::with_capacity(512);
    c.bench_function("submit_end_to_end_sign_and_serialize", |b| {
        b.iter(|| {
            let unsigned = encode_eip1559_unsigned(&tx);
            let hash = keccak256(&unsigned);
            let sig = sign_eip1559(&key, &hash).unwrap();
            let raw = encode_eip1559_signed(&tx, sig);
            build_send_raw_tx_envelope(&raw, 1, &mut buf);
        })
    });
}

criterion_group!(
    benches,
    bench_rlp_encode_unsigned,
    bench_keccak_signing_hash,
    bench_secp256k1_sign,
    bench_jsonrpc_envelope,
    bench_end_to_end_sign_and_serialize
);
criterion_main!(benches);
