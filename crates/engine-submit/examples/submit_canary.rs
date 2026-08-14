//! Submit canary — the no-capital, no-arbitrage inclusion-latency probe for AWS bring-up.
//!
//! Sends ONE 1-wei self-transfer (`to == from`) through the EXACT live-submit wire the engine
//! uses (`Eip1559Tx` → secp256k1 sign → `eth_sendRawTransaction` envelope → sequencer POST →
//! `classify_submit_response`), then prints the submit→ack latency and the returned tx hash.
//! This validates the first real POST to the sequencer and measures same-AZ round-trip BEFORE
//! any arbitrage capital is at risk (proof-on-AWS step 4).
//!
//! Money-safety:
//! * It moves **1 wei to yourself** (configurable `VALUE_WEI`); the only real cost is gas
//!   (~21,000 units, a fraction of a cent on Arbitrum). Review before running.
//! * It refuses to broadcast unless the signed tx **recovers to the funding EOA** (guards against
//!   a malformed signature sending to the wrong place).
//! * `CANARY_DRY=1` does everything EXCEPT the POST (sign + recover-check + print) — use it to
//!   smoke-test the wire with zero broadcast.
//!
//! Env:
//!   CANARY_PRIVATE_KEY  32-byte hex (with/without 0x) of the funded canary EOA. REQUIRED.
//!   SEQUENCER_URL       e.g. https://arb1-sequencer.arbitrum.io/rpc  (or http://… mock). REQUIRED unless dry.
//!   NONCE               current on-chain nonce of the EOA (eth_getTransactionCount). REQUIRED.
//!   CHAIN_ID            default 42161 (Arbitrum One).
//!   MAX_FEE_WEI         max_fee_per_gas in wei. default 100_000_000 (0.1 gwei, > ArbOS-51 0.02 floor).
//!   GAS_LIMIT           default 21000 (plain value transfer, no calldata).
//!   VALUE_WEI           default 1.
//!   CANARY_DRY=1        sign + recover-check + print, but DO NOT POST.
//!
//! Run (dry smoke):   CANARY_PRIVATE_KEY=0x11..11 NONCE=0 CANARY_DRY=1 \
//!                      cargo run --release -p engine-submit --example submit_canary
//! Run (live on AWS): CANARY_PRIVATE_KEY=<funded> NONCE=<n> SEQUENCER_URL=<seq> \
//!                      cargo run --release -p engine-submit --example submit_canary

use std::time::Instant;

use alloy_primitives::U256;
use engine_submit::{
    build_send_raw_tx_envelope, classify_submit_response, speculative::SpeculativeTx, Eip1559Tx,
    SequencerTlsConnection, SigningKey,
};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

fn parse_hex32(s: &str) -> [u8; 32] {
    let s = s.trim().trim_start_matches("0x");
    assert_eq!(
        s.len(),
        64,
        "CANARY_PRIVATE_KEY must be 32 bytes (64 hex chars)"
    );
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
    }
    out
}

fn main() {
    let key_hex =
        env("CANARY_PRIVATE_KEY").expect("set CANARY_PRIVATE_KEY (funded EOA, 32-byte hex)");
    let nonce: u64 = env("NONCE")
        .expect("set NONCE (eth_getTransactionCount of the EOA at 'latest')")
        .parse()
        .expect("NONCE must be a u64");
    let chain_id: u64 = env("CHAIN_ID")
        .map(|s| s.parse().unwrap())
        .unwrap_or(42_161);
    let max_fee: u64 = env("MAX_FEE_WEI")
        .map(|s| s.parse().unwrap())
        .unwrap_or(100_000_000);
    let gas_limit: u64 = env("GAS_LIMIT")
        .map(|s| s.parse().unwrap())
        .unwrap_or(21_000);
    let value_wei: u64 = env("VALUE_WEI").map(|s| s.parse().unwrap()).unwrap_or(1);
    let dry = env("CANARY_DRY").as_deref() == Some("1");

    let key = SigningKey::from_bytes(&parse_hex32(&key_hex)).expect("invalid private key");
    let from = key.address();
    println!("canary EOA   = 0x{}", faster_hex::hex_string(&from));
    println!("chain_id={chain_id} nonce={nonce} max_fee_wei={max_fee} gas_limit={gas_limit} value_wei={value_wei} dry={dry}");

    // 1-wei self-transfer: to == from, no calldata.
    let tx = Eip1559Tx {
        chain_id,
        nonce,
        max_priority_fee_per_gas: U256::ZERO, // priority fee is irrelevant on Arbitrum FCFS
        max_fee_per_gas: U256::from(max_fee),
        gas_limit,
        to: from,
        value: U256::from(value_wei),
        data: Vec::new(),
    };
    let raw = SpeculativeTx::build_and_sign(&tx, &key)
        .expect("sign")
        .commit()
        .into_bytes();

    // Safety gate: the signed tx MUST recover to the funding EOA before we ever broadcast.
    let recovered = engine_feed::recover_tx_sender(&raw).expect("recover signer from signed tx");
    assert_eq!(
        recovered, from,
        "signed tx does not recover to the funding EOA — refusing to broadcast"
    );
    println!("recover-check OK (signed tx resolves to the funding EOA)");

    let mut body = Vec::new();
    build_send_raw_tx_envelope(&raw, 1, &mut body);

    if dry {
        println!(
            "CANARY_DRY=1 — NOT broadcasting. Envelope is {} bytes, signed-tx is {} bytes.",
            body.len(),
            raw.len()
        );
        println!("raw signed tx = 0x{}", faster_hex::hex_string(&raw));
        return;
    }

    let seq_url = env("SEQUENCER_URL")
        .expect("set SEQUENCER_URL (e.g. https://arb1-sequencer.arbitrum.io/rpc)");
    let mut conn = SequencerTlsConnection::new(&seq_url).expect("connect to sequencer");
    let t0 = Instant::now();
    let resp = conn.post_json(&body).expect("post_json to sequencer");
    let submit_ack_us = t0.elapsed().as_micros();

    let outcome = classify_submit_response(&resp);
    println!("submit_ack_us = {submit_ack_us}");
    println!("outcome       = {outcome:?}");
    println!("response      = {}", String::from_utf8_lossy(&resp));
    println!(
        "\nNext: grep the local relay feed for this tx hash to measure submit_start -> own_tx_seen_in_feed,\nthen run the paper/reconcile harness against the REAL block to get the first p_win."
    );
}
