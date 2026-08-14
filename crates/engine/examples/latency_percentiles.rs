//! Full latency-percentile dump for the hot per-frame pipeline.
//!
//! Runs the real decode -> classify -> confidence -> filter -> sizing -> profit/EV ->
//! sign + RLP + JSON-RPC envelope stages over a feed of frames, timing each stage into the
//! production `LatencyHistograms` (HdrHistogram), then prints p50/p90/p95/p99/p99.9/max per
//! stage and end-to-end, in ns, us and ms.
//!
//! Run: cargo run --release --example latency_percentiles -- [iterations]

use std::sync::Arc;
use std::time::Instant;

use base64_simd::STANDARD as BASE64;
use ruint::aliases::U256;

use engine_amm::v2_get_amount_out;
use engine_detect::{
    filters::best_v2v2_bucket, marginal_rate_product_v2v2, Classifier, ClassifierBuilder,
    Confidence, ConfidenceFactors, TxClass,
};
use engine_feed::{address::keccak256, decode_envelope, decode_l2msg};
use engine_gas::{l1_data_fee_wei, l2_execution_cost_wei, GasSnapshot};
use engine_metrics::{LatencyHistograms, StageLatency};
use engine_routes::calldata::{encode_mantis_mixed, MantisFunding, MantisHop};
use engine_sim::{
    canonical_net_profit, ev_decision, EvDecomposition, ProfitTerms, SubmissionDecision,
};
use engine_state::PoolId;
use engine_submit::{
    build_send_raw_tx_envelope, encode_eip1559_signed, encode_eip1559_unsigned, sign_eip1559,
    Eip1559Tx, SigningKey,
};

fn make_signed_tx_l2msg(selector: [u8; 4]) -> Vec<u8> {
    let mut inner = Vec::new();
    inner.push(0x04u8); // kind=4 SignedTx
    inner.push(0x02); // EIP-1559
    let mut payload = Vec::new();
    for _ in 0..5 {
        payload.push(0x80);
    } // chainId nonce maxPrio maxFee gas
    payload.push(0x94);
    payload.extend_from_slice(&[0x99u8; 20]); // to = router
    payload.push(0x80); // value
    payload.push(0x84);
    payload.extend_from_slice(&selector); // data = selector
    payload.push(0xc0); // empty access list
    payload.push(0x80);
    payload.push(0x80);
    payload.push(0x80);
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
    let inner = make_signed_tx_l2msg([0xc0, 0x4b, 0x8d, 0x59]); // V3 exactInput
    let b64 = BASE64.encode_to_string(&inner);
    format!(r#"{{"sequenceNumber":{seq},"timestamp":1715000000,"blockNumber":22300000,"l2Msg":"{b64}"}}"#)
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

#[allow(clippy::too_many_arguments)]
fn one_frame(
    raw: &[u8],
    classifier: &Classifier,
    snap: &GasSnapshot,
    decision: &SubmissionDecision,
    key: &SigningKey,
    sign_buf: &mut Vec<u8>,
    rpc_buf: &mut Vec<u8>,
    h: &LatencyHistograms,
) {
    let t_e2e = Instant::now();

    // Stage 1: FrameParse (envelope + base64 + l2msg)
    let t = Instant::now();
    let env = decode_envelope(raw).unwrap();
    let mut scratch = env.l2msg_b64.to_vec();
    let decoded_len = base64_simd::forgiving_decode_inplace(&mut scratch)
        .unwrap()
        .len();
    let payload = &scratch[..decoded_len];
    let (_kind, tx_opt) = decode_l2msg(payload).unwrap();
    h.record(StageLatency::FrameParse, t.elapsed().as_nanos() as u64);

    // Stage 2: Classify
    let t = Instant::now();
    let to = tx_opt.as_ref().map(|t| t.to).unwrap_or([0u8; 20]);
    let data = tx_opt.as_ref().map(|t| t.data).unwrap_or(&[]);
    let class = classifier.classify(&to, data);
    h.record(StageLatency::Classify, t.elapsed().as_nanos() as u64);
    if matches!(class, TxClass::Ignored) {
        return;
    }

    // Stage 3: Confidence + Filters
    let t = Instant::now();
    let conf = Confidence::from_factors(ConfidenceFactors::full());
    let _ = conf.passes;
    let _ = marginal_rate_product_v2v2(
        1_000_000_000,
        4_000_000_000,
        30,
        4_000_000_000,
        1_010_000_000,
        30,
        0,
    );
    h.record(StageLatency::Filters, t.elapsed().as_nanos() as u64);

    // Stage 4: Sim (sizing grid + two-hop exact profit + profit/EV)
    let t = Instant::now();
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
    let mid = v2_get_amount_out(1_000_000, 1_000_000_000, 4_000_000_000, 30).unwrap();
    let _ = v2_get_amount_out(mid, 4_000_000_000, 1_010_000_000, 30).unwrap();
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
    h.record(StageLatency::Sim, t.elapsed().as_nanos() as u64);

    // Stage 5: Submit (calldata + sign + RLP + JSON-RPC envelope)
    let t = Instant::now();
    let calldata = encode_mantis_mixed(
        MantisFunding::BalancerOrFlashSwap,
        1_000_000u128,
        &[MantisHop::default(); 3],
        1u128,
    );
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
    let raw_signed = encode_eip1559_signed(&tx, sig);
    build_send_raw_tx_envelope(&raw_signed, 1, rpc_buf);
    h.record(StageLatency::Submit, t.elapsed().as_nanos() as u64);

    h.record(StageLatency::EndToEnd, t_e2e.elapsed().as_nanos() as u64);
}

fn dump(h: &LatencyHistograms, stage: StageLatency, name: &str) {
    let p = |q: f64| h.percentile(stage, q);
    let us = |ns: u64| ns as f64 / 1_000.0;
    let ms = |ns: u64| ns as f64 / 1_000_000.0;
    println!(
        "{:<11} | n={:>8} | p50 {:>8.3}us  p90 {:>8.3}us  p95 {:>8.3}us  p99 {:>8.3}us  p99.9 {:>8.3}us  max {:>8.3}us",
        name, h.count(stage), us(p(50.0)), us(p(90.0)), us(p(95.0)), us(p(99.0)), us(p(99.9)), us(h.max(stage))
    );
    println!(
        "{:<11} | {:>10} | p50 {:>8.5}ms  p90 {:>8.5}ms  p95 {:>8.5}ms  p99 {:>8.5}ms  p99.9 {:>8.5}ms  max {:>8.5}ms",
        "", "", ms(p(50.0)), ms(p(90.0)), ms(p(95.0)), ms(p(99.0)), ms(p(99.9)), ms(h.max(stage))
    );
}

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    engine_metrics::set_metrics_enabled(true);

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
    let h = LatencyHistograms::new();
    let mut sign_buf = Vec::with_capacity(256);
    let mut rpc_buf = Vec::with_capacity(512);

    const N: usize = 1024;
    let frames: Vec<Vec<u8>> = (0..N).map(|i| make_envelope(i as u64)).collect();

    // Warm up (page-ins, branch predictor) — not recorded.
    for i in 0..2048 {
        one_frame(
            &frames[i % N],
            &classifier,
            &snap,
            &decision,
            &key,
            &mut sign_buf,
            &mut rpc_buf,
            &h,
        );
    }
    // Reset histograms after warmup by building a fresh set.
    let h = LatencyHistograms::new();
    for i in 0..iters {
        one_frame(
            &frames[i % N],
            &classifier,
            &snap,
            &decision,
            &key,
            &mut sign_buf,
            &mut rpc_buf,
            &h,
        );
    }

    println!("# Hot per-frame pipeline latency percentiles  (iterations={iters})");
    println!("# stage      |   samples  | percentiles (microseconds, then milliseconds)");
    dump(&h, StageLatency::FrameParse, "FrameParse");
    dump(&h, StageLatency::Classify, "Classify");
    dump(&h, StageLatency::Filters, "Filters");
    dump(&h, StageLatency::Sim, "Sim");
    dump(&h, StageLatency::Submit, "Submit");
    dump(&h, StageLatency::EndToEnd, "EndToEnd");
}
