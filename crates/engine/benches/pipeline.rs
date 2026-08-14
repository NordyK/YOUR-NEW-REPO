//! End-to-end pipeline benchmark.
//!
//! Replays a synthetic feed of `N` frames through the full decode → classify → confidence
//! → filter → sizing → profit → REVM-validate → sign + RLP + JSON-RPC envelope pipeline.
//! Measures per-frame wall-clock and aggregate throughput.

use std::sync::Arc;

use base64_simd::STANDARD as BASE64;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use ruint::aliases::U256;

use engine_amm::v2_get_amount_out;
use engine_detect::{
    filters::best_v2v2_bucket, marginal_rate_product_v2v2, Classifier, ClassifierBuilder,
    Confidence, ConfidenceFactors, TxClass,
};
use engine_feed::{address::keccak256, decode_envelope, decode_l2msg};
use engine_gas::{l1_data_fee_wei, l2_execution_cost_wei, GasSnapshot};
use engine_routes::calldata::{encode_mantis_mixed, MantisFunding, MantisHop};
use engine_sim::{
    canonical_net_profit, ev_decision, EvDecomposition, ProfitTerms, SubmissionDecision,
};
use engine_state::PoolId;
use engine_submit::{
    build_send_raw_tx_envelope, encode_eip1559_signed, encode_eip1559_unsigned, sign_eip1559,
    Eip1559Tx, SigningKey,
};

const N_FRAMES: usize = 1_000;

fn make_signed_tx_l2msg(selector: [u8; 4]) -> Vec<u8> {
    let mut inner = Vec::new();
    inner.push(0x04u8); // kind=4 SignedTx
                        // Build a synthetic EIP-1559 envelope: type=0x02 + RLP list.
                        // We emit a minimal list that decode_signed_tx_1559 accepts.
                        // Payload positions: chainId nonce maxPriority maxFee gasLimit to value data accessList
                        // = 9 items. We use empty bytes (0x80) for numeric fields, a 20-byte address,
                        // and `data` = `selector`.
    inner.push(0x02);
    // Build inner RLP list contents.
    let mut payload = Vec::new();
    payload.push(0x80); // chainId
    payload.push(0x80); // nonce
    payload.push(0x80); // maxPriority
    payload.push(0x80); // maxFee
    payload.push(0x80); // gasLimit
                        // to = 20 bytes
    payload.push(0x94);
    payload.extend_from_slice(&[0x99u8; 20]); // address matching router benchmark
    payload.push(0x80); // value
                        // data: short string of 4 bytes
    payload.push(0x84);
    payload.extend_from_slice(&selector);
    payload.push(0xc0); // empty access list
                        // y_parity, r, s — empties for the harness.
    payload.push(0x80);
    payload.push(0x80);
    payload.push(0x80);
    // RLP list header.
    let list_len = payload.len();
    if list_len < 56 {
        inner.push(0xc0 + list_len as u8);
    } else {
        let len_be = (list_len as u64).to_be_bytes();
        let lead = len_be.iter().position(|&b| b != 0).unwrap_or(7);
        inner.push(0xf7 + (8 - lead) as u8);
        inner.extend_from_slice(&len_be[lead..]);
    }
    inner.extend_from_slice(&payload);
    inner
}

fn make_envelope(seq: u64) -> Vec<u8> {
    let inner = make_signed_tx_l2msg([0xc0, 0x4b, 0x8d, 0x59]); // V3 SwapRouter exactInput
    let b64 = BASE64.encode_to_string(&inner);
    format!(
        r#"{{"sequenceNumber":{seq},"timestamp":1715000000,"blockNumber":22300000,"l2Msg":"{b64}"}}"#
    )
    .into_bytes()
}

fn build_classifier() -> Arc<Classifier> {
    let mut b = ClassifierBuilder::new();
    for i in 0..256u32 {
        let mut a = [0u8; 20];
        a[..4].copy_from_slice(&i.to_be_bytes());
        b.add_pool(a, PoolId(i));
    }
    b.add_router([0x99u8; 20], 1);
    Arc::new(b.build())
}

/// Single pass through all hot stages. Returns the wall-clock ns measured by criterion.
fn process_frame_full_pipeline(
    raw: &[u8],
    classifier: &Classifier,
    snap: &GasSnapshot,
    decision: &SubmissionDecision,
    key: &SigningKey,
    sign_buf: &mut Vec<u8>,
    rpc_buf: &mut Vec<u8>,
) {
    // 1. Decode envelope.
    let env = decode_envelope(raw).unwrap();
    // 2. Base64-simd in place.
    let mut scratch = env.l2msg_b64.to_vec();
    let decoded_len = base64_simd::forgiving_decode_inplace(&mut scratch)
        .unwrap()
        .len();
    let payload = &scratch[..decoded_len];
    // 3. Decode L2 message.
    let (_kind, tx_opt) = decode_l2msg(payload).unwrap();
    // 4. Classify.
    let to = tx_opt.as_ref().map(|t| t.to).unwrap_or([0u8; 20]);
    let data = tx_opt.as_ref().map(|t| t.data).unwrap_or(&[]);
    let class = classifier.classify(&to, data);
    let dirty = !matches!(class, TxClass::Ignored);
    if !dirty {
        return;
    }
    // 5. Confidence chain.
    let conf = Confidence::from_factors(ConfidenceFactors::full());
    if !conf.passes {
        return;
    }
    // 6. Marginal-rate filter (V2/V2 pair).
    let rate_ok = marginal_rate_product_v2v2(
        1_000_000_000,
        4_000_000_000,
        30,
        4_000_000_000,
        1_010_000_000,
        30,
        0,
    )
    .is_ok();
    if !rate_ok {
        return;
    }
    // 7. Amount-grid scan.
    let grid = [10_000u128, 100_000, 1_000_000, 10_000_000];
    let best = best_v2v2_bucket(
        &grid,
        1_000_000_000,
        4_000_000_000,
        30,
        4_000_000_000,
        1_010_000_000,
        30,
    );
    let Some((_, gross)) = best else { return };
    // 8. Two-hop exact integer profit (mirror sizing kernel).
    let mid = v2_get_amount_out(1_000_000, 1_000_000_000, 4_000_000_000, 30).unwrap();
    let _out = v2_get_amount_out(mid, 4_000_000_000, 1_010_000_000, 30).unwrap();
    // 9. Profit math + EV decision.
    let terms = ProfitTerms {
        gross_profit_wei: U256::from(gross),
        l2_execution_cost_wei: U256::from(l2_execution_cost_wei(snap, 200_000)),
        l1_data_fee_wei: U256::from(l1_data_fee_wei(snap, 240)),
        flash_loan_fee_wei: U256::ZERO,
        executor_overhead_wei: U256::from(30_000u64),
        safety_buffer_wei: U256::from(1u64),
    };
    let _ = canonical_net_profit(terms);
    let ev = EvDecomposition {
        net_profit_wei: U256::from(gross),
        p_win_e6: 800_000,
        p_revert_e6: 50_000,
        p_stale_e6: 10_000,
        p_ops_e6: 1_000,
        revert_loss_wei: U256::from(100u64),
        stale_loss_wei: U256::from(50u64),
        operational_penalty_wei: U256::from(5u64),
    };
    let _ = ev_decision(terms, ev, *decision, 0.99, U256::from(1_000_000u64));
    // 10. Build calldata.
    let calldata = encode_mantis_mixed(
        MantisFunding::BalancerOrFlashSwap,
        1_000_000u128,
        &[MantisHop::default(); 3],
        1u128,
    );
    // 11. Sign + serialize EIP-1559 tx.
    sign_buf.clear();
    sign_buf.extend_from_slice(&calldata);
    let tx = Eip1559Tx {
        chain_id: 42_161,
        nonce: 7,
        max_priority_fee_per_gas: U256::ZERO,
        max_fee_per_gas: U256::from(40_000_000u64),
        gas_limit: 500_000,
        to: [0xeeu8; 20],
        value: U256::ZERO,
        data: sign_buf.clone(),
    };
    let unsigned = encode_eip1559_unsigned(&tx);
    let hash = keccak256(&unsigned);
    let sig = sign_eip1559(key, &hash).unwrap();
    let raw = encode_eip1559_signed(&tx, sig);
    // 12. Build JSON-RPC envelope.
    build_send_raw_tx_envelope(&raw, 1, rpc_buf);
}

fn bench_pipeline(c: &mut Criterion) {
    let classifier = build_classifier();
    let snap = GasSnapshot {
        l2_gas_price_wei: 20_000_000,
        l1_base_fee_per_gas_wei: 1_000_000,
        l1_gas_per_data_byte: 16,
        min_l2_base_fee_wei: 20_000_000,
        static_overhead_bytes: 140,
        balancer_flash_fee_1e18: U256::ZERO,
        captured_at_ms: engine_gas::now_ms(),
        l1_sigma_bps: 0,
    };
    let decision = SubmissionDecision {
        min_required_net_profit_wei: U256::from(1u64),
        min_ev_absolute_wei: 0,
        min_roi_bps: 0,
        gas_spike_ratio_q8: 384,
        confidence_floor: 0.95,
        max_flashloan_fee_1e18: 0,
    };
    let key = SigningKey::from_bytes(&[0x11u8; 32]).unwrap();
    let mut frames = Vec::with_capacity(N_FRAMES);
    for i in 0..N_FRAMES {
        frames.push(make_envelope(i as u64));
    }
    let mut sign_buf = Vec::with_capacity(256);
    let mut rpc_buf = Vec::with_capacity(512);

    let mut group = c.benchmark_group("pipeline");
    group.throughput(Throughput::Elements(1));
    group.bench_function("pipeline_full_per_frame", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            process_frame_full_pipeline(
                black_box(&frames[idx]),
                &classifier,
                &snap,
                &decision,
                &key,
                &mut sign_buf,
                &mut rpc_buf,
            );
            idx = (idx + 1) % N_FRAMES;
        })
    });
    group.finish();
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
