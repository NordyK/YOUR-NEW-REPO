//! Chunked `eth_getLogs` fetcher. Providers (Alchemy, Infura, public Arbitrum RPC) all
//! cap the block-range or result-count per call; we chunk the range and accumulate.
//!
//! Each chunk is one JSON-RPC call. On a `400`-class "range too large" failure we halve
//! the chunk and retry — this is the standard adaptive pattern. On transient `5xx` /
//! network errors we retry up to `max_retries` with exponential backoff (250 ms × 2^n).

use std::time::Duration;

use engine_rpc::ArbitrumRpcClient;
use serde::Deserialize;
use tracing::{info, warn};

use crate::types::{Address, DiscoveryError};

/// One decoded log entry. Topics and data are kept in hex-decoded byte form so callers can
/// slice directly into them.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Block number the log was emitted at.
    pub block_number: u64,
    /// Contract address that emitted the log.
    pub address: Address,
    /// Indexed topics (`topic[0]` = event signature hash).
    pub topics: Vec<[u8; 32]>,
    /// Non-indexed event data, concatenated.
    pub data: Vec<u8>,
}

/// Fetch every log matching `(contract, topic0)` over `[from_block, to_block]`, chunking
/// the range so each call stays within provider quota. Adaptive: if a chunk fails with a
/// range-too-large signal (Alchemy returns code `-32602` with a message containing
/// "block range" or "logs are limited"), the chunk is halved.
pub fn fetch_logs_chunked(
    client: &ArbitrumRpcClient,
    contract: &Address,
    topic0: &[u8; 32],
    from_block: u64,
    to_block: u64,
    chunk_blocks: u64,
    max_retries: u32,
) -> Result<Vec<LogEntry>, DiscoveryError> {
    if chunk_blocks == 0 {
        return Err(DiscoveryError::ZeroChunk);
    }
    let mut out = Vec::new();
    let mut cursor = from_block;
    let mut chunk = chunk_blocks;
    let span_total = to_block.saturating_sub(from_block).saturating_add(1);
    let mut next_log_at = from_block;
    while cursor <= to_block {
        let end = (cursor + chunk - 1).min(to_block);
        match try_get_logs(client, contract, topic0, cursor, end, max_retries) {
            Ok(mut chunk_out) => {
                out.append(&mut chunk_out);
                if cursor >= next_log_at {
                    let scanned = cursor.saturating_sub(from_block);
                    let pct = if span_total > 0 {
                        scanned * 100 / span_total
                    } else {
                        100
                    };
                    info!(
                        from = cursor,
                        to = end,
                        chunk_size = chunk,
                        running_total = out.len(),
                        pct,
                        "discovery progress"
                    );
                    // Log every 5% of the span (or every 10M blocks, whichever is smaller).
                    let stride = (span_total / 20).max(10_000_000);
                    next_log_at = cursor + stride;
                }
                cursor = end + 1;
                if chunk < chunk_blocks {
                    chunk = (chunk * 2).min(chunk_blocks);
                }
            }
            Err(e) => {
                if is_range_too_large(&e) && chunk > 1 {
                    let next = (chunk / 2).max(1);
                    warn!(
                        from = cursor,
                        to = end,
                        old_chunk = chunk,
                        new_chunk = next,
                        "halving log chunk"
                    );
                    chunk = next;
                    continue;
                }
                return Err(e);
            }
        }
    }
    Ok(out)
}

fn try_get_logs(
    client: &ArbitrumRpcClient,
    contract: &Address,
    topic0: &[u8; 32],
    from_block: u64,
    to_block: u64,
    max_retries: u32,
) -> Result<Vec<LogEntry>, DiscoveryError> {
    let mut topic0_hex = [0u8; 64];
    faster_hex::hex_encode(topic0, &mut topic0_hex).expect("32 → 64 hex");
    let topic0_str = format!("0x{}", std::str::from_utf8(&topic0_hex).unwrap());
    let mut contract_hex = [0u8; 40];
    faster_hex::hex_encode(contract, &mut contract_hex).expect("20 → 40 hex");
    let contract_str = format!("0x{}", std::str::from_utf8(&contract_hex).unwrap());

    let params = serde_json::json!([{
        "address": contract_str,
        "topics": [topic0_str],
        "fromBlock": format!("0x{:x}", from_block),
        "toBlock": format!("0x{:x}", to_block),
    }]);

    let mut attempt: u32 = 0;
    loop {
        match client.call("eth_getLogs", params.clone()) {
            Ok(v) => return decode_logs(&v),
            Err(e) => {
                if attempt >= max_retries {
                    return Err(e.into());
                }
                attempt += 1;
                let sleep_ms = 250u64 << attempt.min(5);
                warn!(attempt, sleep_ms, err = ?e, "eth_getLogs transient; backing off");
                std::thread::sleep(Duration::from_millis(sleep_ms));
            }
        }
    }
}

fn is_range_too_large(e: &DiscoveryError) -> bool {
    let hit = |msg: &str| {
        let m = msg.to_ascii_lowercase();
        m.contains("block range")
            || m.contains("logs are limited")
            || m.contains("query returned more than")
            || m.contains("try with this block range")
            || m.contains("range is too large")
            // Free-tier provider signatures (2026-07-10): blastapi "up to a 10 block range",
            // drpc "ranges over 10000 blocks are not supported", 1rpc "limited to 0 - 50 blocks",
            // plus HTTP 413 request-body caps surfaced as a transport error.
            || m.contains("block range")
            || m.contains("are not supported")
            || m.contains("limited to")
            || m.contains("up to a")
            || m.contains("413")
            || m.contains("payload too large")
            || m.contains("request entity too large")
    };
    match e {
        DiscoveryError::Rpc(engine_rpc::RpcError::Server { message: msg, .. }) => hit(msg),
        DiscoveryError::Rpc(engine_rpc::RpcError::Transport(msg)) => hit(msg),
        _ => false,
    }
}

#[derive(Debug, Deserialize)]
struct RawLog {
    #[serde(rename = "blockNumber")]
    block_number: String,
    address: String,
    topics: Vec<String>,
    data: String,
}

fn decode_logs(v: &serde_json::Value) -> Result<Vec<LogEntry>, DiscoveryError> {
    let raws: Vec<RawLog> = serde_json::from_value(v.clone())
        .map_err(|e| DiscoveryError::Hex(format!("logs json: {e}")))?;
    let mut out = Vec::with_capacity(raws.len());
    for raw in raws {
        let block_number = u64::from_str_radix(raw.block_number.trim_start_matches("0x"), 16)
            .map_err(|e| DiscoveryError::Hex(format!("block_number: {e}")))?;
        let mut address = [0u8; 20];
        let addr_hex = raw.address.trim_start_matches("0x");
        if addr_hex.len() != 40 {
            return Err(DiscoveryError::MalformedLog("log address length"));
        }
        faster_hex::hex_decode(addr_hex.as_bytes(), &mut address)
            .map_err(|e| DiscoveryError::Hex(format!("addr: {e}")))?;
        let mut topics = Vec::with_capacity(raw.topics.len());
        for t in &raw.topics {
            let mut tb = [0u8; 32];
            let th = t.trim_start_matches("0x");
            if th.len() != 64 {
                return Err(DiscoveryError::MalformedLog("topic length"));
            }
            faster_hex::hex_decode(th.as_bytes(), &mut tb)
                .map_err(|e| DiscoveryError::Hex(format!("topic: {e}")))?;
            topics.push(tb);
        }
        let dh = raw.data.trim_start_matches("0x");
        let mut data = vec![0u8; dh.len() / 2];
        if !dh.is_empty() {
            faster_hex::hex_decode(dh.as_bytes(), &mut data)
                .map_err(|e| DiscoveryError::Hex(format!("data: {e}")))?;
        }
        out.push(LogEntry {
            block_number,
            address,
            topics,
            data,
        });
    }
    Ok(out)
}
