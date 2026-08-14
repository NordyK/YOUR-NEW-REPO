//! Win-rate harness (part B) — offline reconciler driver.
//!
//! Reads the paper-mode JSONL the engine wrote in dry-run and, for each would-submit opportunity,
//! asks the chain: did a competitor land a same-pool tx in the next few blocks, and would the
//! engine's projected submit time have beaten that inclusion? Prints an aggregate capture rate.
//! All decision logic is the unit-tested `engine_rpc::paper_reconcile` module; this file is the
//! I/O shell (read file, fetch blocks, print).
//!
//! Usage:
//!   ARBITRUM_RPC_URL=<url> \
//!   PAPER_MODE_LOG=<paper.jsonl> \
//!   ENGINE_ONE_WAY_MS=<projected submit latency, default 0> \
//!   RECONCILE_SAFETY_MS=<win margin, default 100> \
//!   RECONCILE_BLOCK_WINDOW=<blocks to scan after detection, default 2> \
//!   cargo run --release -p engine-rpc --example paper_reconcile

use std::collections::HashMap;

use engine_rpc::client::ArbitrumRpcClient;
use engine_rpc::paper_reconcile::{
    classify_verdict, earliest_log_block, parse_paper_record, Stats,
};
use serde_json::json;

fn env_u128(name: &str, default: u128) -> u128 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

fn hex_to_u64(s: &str) -> Option<u64> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

/// Earliest block in `[from, to]` where a competitor emitted a swap/sync log from one of `pools`.
/// Uses `eth_getLogs` with a pool-address filter — the NODE does exact emitter matching, far more
/// robust than scanning calldata (most arb bots use opaque router calldata, but the pool itself
/// always emits the log). `None` if uncontested / the RPC failed.
fn earliest_competitor_block(
    client: &ArbitrumRpcClient,
    pools: &[String],
    from: u64,
    to: u64,
) -> Option<u64> {
    let v = client
        .call(
            "eth_getLogs",
            json!([{
                "fromBlock": format!("0x{from:x}"),
                "toBlock": format!("0x{to:x}"),
                "address": pools,
            }]),
        )
        .ok()?;
    earliest_log_block(v.as_array()?)
}

/// Block timestamp in nanoseconds (block header only — no full txs needed).
fn block_timestamp_ns(client: &ArbitrumRpcClient, block: u64) -> Option<u128> {
    let v = client
        .call(
            "eth_getBlockByNumber",
            json!([format!("0x{block:x}"), false]),
        )
        .ok()?;
    Some(hex_to_u64(v.get("timestamp")?.as_str()?)? as u128 * 1_000_000_000)
}

fn main() {
    let rpc_url = std::env::var("ARBITRUM_RPC_URL").expect("set ARBITRUM_RPC_URL");
    let log_path =
        std::env::var("PAPER_MODE_LOG").expect("set PAPER_MODE_LOG (paper.jsonl to read)");
    let one_way_ns = env_u128("ENGINE_ONE_WAY_MS", 0) * 1_000_000;
    let safety_ns = env_u128("RECONCILE_SAFETY_MS", 100) * 1_000_000;
    let window = env_u128("RECONCILE_BLOCK_WINDOW", 2) as u64;

    let client = ArbitrumRpcClient::new(&rpc_url);
    let content = std::fs::read_to_string(&log_path).expect("read PAPER_MODE_LOG");
    let records: Vec<_> = content.lines().filter_map(parse_paper_record).collect();
    println!(
        "reconciling {} paper records (one_way={}ms safety={}ms window={} blocks)",
        records.len(),
        one_way_ns / 1_000_000,
        safety_ns / 1_000_000,
        window,
    );

    // Cache block timestamps so repeated competitor blocks don't refetch.
    let mut ts_cache: HashMap<u64, Option<u128>> = HashMap::new();
    let mut verdicts = Vec::with_capacity(records.len());

    for (i, rec) in records.iter().enumerate() {
        let engine_submit_ns = rec.timestamp_ns as u128 + one_way_ns;
        let to = rec.block_number_seen.saturating_add(window);
        // Earliest competitor inclusion (block timestamp ns), or None if uncontested.
        let competitor_ns =
            earliest_competitor_block(&client, &rec.pools, rec.block_number_seen, to).and_then(
                |b| {
                    *ts_cache
                        .entry(b)
                        .or_insert_with(|| block_timestamp_ns(&client, b))
                },
            );
        let v = classify_verdict(engine_submit_ns, competitor_ns, safety_ns);
        if i < 12 {
            println!(
                "  route {:>5} block {} -> {:?}",
                rec.route_id, rec.block_number_seen, v
            );
        }
        verdicts.push(v);
    }

    let s = Stats::from_verdicts(&verdicts);
    println!(
        "RESULT won={} lost={} uncontested={} total={}",
        s.won,
        s.lost,
        s.uncontested,
        s.total()
    );
    println!(
        "capture_rate (contested, won/(won+lost)) = {:.1}%",
        s.capture_rate_contested() * 100.0
    );
    println!(
        "capture_rate (optimistic, incl uncontested) = {:.1}%",
        s.capture_rate_incl_uncontested() * 100.0
    );
}
