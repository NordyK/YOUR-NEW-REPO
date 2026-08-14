use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
};

use engine_feed::{
    address::keccak256, decode_envelope, decode_l2msg_recursive_raw, decode_signed_tx,
};

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2 + 2);
    out.push_str("0x");
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn main() {
    let path = env::args()
        .nth(1)
        .expect("usage: dump_corpus_txs <corpus.jsonl>");
    let file = File::open(path).expect("open corpus");
    let mut decoded = 0u64;
    for line in BufReader::new(file).lines() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        let env = decode_envelope(line.as_bytes()).expect("decode envelope");
        let mut payload = env.l2msg_b64.to_vec();
        let n = base64_simd::forgiving_decode_inplace(&mut payload)
            .expect("base64")
            .len();
        let payload = &payload[..n];
        let mut tx_idx = 0u64;
        decode_l2msg_recursive_raw(payload, |raw| {
            let hash = keccak256(raw);
            if let Some(tx) = decode_signed_tx(raw) {
                let selector = tx
                    .data
                    .get(..4)
                    .map(hex)
                    .unwrap_or_else(|| "0x".to_string());
                println!(
                    "seq={} tx_idx={} tx_hash={} tx_type=0x{:02x} to={} selector={} data_len={} value={} nonce={} chain_id={} raw_len={}",
                    env.sequence_number,
                    tx_idx,
                    hex(&hash),
                    tx.tx_type,
                    hex(&tx.to),
                    selector,
                    tx.data.len(),
                    tx.value,
                    tx.nonce,
                    tx.chain_id,
                    raw.len(),
                );
                decoded += 1;
            } else {
                println!(
                    "seq={} tx_idx={} tx_hash={} undecodable raw_len={}",
                    env.sequence_number,
                    tx_idx,
                    hex(&hash),
                    raw.len(),
                );
            }
            tx_idx += 1;
        });
    }
    eprintln!("decoded_signed_txs={decoded}");
}
