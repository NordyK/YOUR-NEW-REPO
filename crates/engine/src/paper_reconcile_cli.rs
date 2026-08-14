//! `engine reconcile-paper <paper.jsonl>` — W2.2 (FIX_PLAN_2026-07-10): the win-rate
//! reconciler as a first-class subcommand of the deployed binary (operators run the box
//! binary, not cargo examples).
//!
//! For each paper record (a would-submit decision recorded in dry-run) it asks the chain via
//! `eth_getLogs`: did anyone emit a log from the route's pools within the next
//! `RECONCILE_BLOCK_WINDOW` blocks? Verdicts follow the unit-tested
//! `engine_rpc::paper_reconcile` core: Won / Lost (raced) / Uncontested.
//!
//! NEW vs the old example driver — the FORMAL MIRAGE TEST (plan finding #9): groups of
//! records on the same pool set that stay `Uncontested` across ≥`MIRAGE_MIN_RECORDS`
//! records spanning ≥`MIRAGE_MIN_SPAN_SECS` are flagged **MIRAGE-SUSPECT**: a real
//! dislocation gets raced away in seconds; "profit" nobody ever takes is a pricing
//! artifact (the 2026-07-09 corpus's persistent 4.53-ETH DODO pair is the type specimen)
//! and must be EXCLUDED from profit projections.
//!
//! Env knobs (same names as the historical example): `ARBITRUM_RPC_URL`,
//! `ENGINE_ONE_WAY_MS` (projected submit latency, default 0), `RECONCILE_SAFETY_MS`
//! (default 100), `RECONCILE_BLOCK_WINDOW` (default 2), `MIRAGE_MIN_RECORDS` (default 3),
//! `MIRAGE_MIN_SPAN_SECS` (default 600).

use std::collections::HashMap;

use anyhow::{Context, Result};
use engine_rpc::client::ArbitrumRpcClient;
use engine_rpc::paper_reconcile::{
    classify_verdict, earliest_log_block, mirage_groups, parse_paper_record, Stats, Verdict,
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

/// Earliest block in `[from, to]` where a competitor emitted a log from one of `pools`
/// (the node does exact emitter matching — robust vs opaque router calldata).
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

/// Block timestamp in nanoseconds (header only).
fn block_timestamp_ns(client: &ArbitrumRpcClient, block: u64) -> Option<u128> {
    let v = client
        .call(
            "eth_getBlockByNumber",
            json!([format!("0x{block:x}"), false]),
        )
        .ok()?;
    Some(hex_to_u64(v.get("timestamp")?.as_str()?)? as u128 * 1_000_000_000)
}

/// Run the reconciler over `path` (a paper.jsonl). Prints verdicts, capture rates, and the
/// mirage table; returns an error only on unusable inputs (missing file / RPC env).
pub fn run(path: Option<std::path::PathBuf>) -> Result<()> {
    let path = path.context("usage: engine reconcile-paper <paper.jsonl>")?;
    let rpc_url = std::env::var("ARBITRUM_RPC_URL").context("set ARBITRUM_RPC_URL")?;
    let one_way_ns = env_u128("ENGINE_ONE_WAY_MS", 0) * 1_000_000;
    let safety_ns = env_u128("RECONCILE_SAFETY_MS", 100) * 1_000_000;
    let window = env_u128("RECONCILE_BLOCK_WINDOW", 2) as u64;
    let mirage_min_records = env_u128("MIRAGE_MIN_RECORDS", 3) as u32;
    let mirage_min_span_ns = env_u128("MIRAGE_MIN_SPAN_SECS", 600) as u64 * 1_000_000_000;

    let client = ArbitrumRpcClient::new(rpc_url);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let records: Vec<_> = content.lines().filter_map(parse_paper_record).collect();
    println!(
        "reconciling {} paper records (one_way={}ms safety={}ms window={} blocks)",
        records.len(),
        one_way_ns / 1_000_000,
        safety_ns / 1_000_000,
        window,
    );

    let mut ts_cache: HashMap<u64, Option<u128>> = HashMap::new();
    let mut reconciled: Vec<(engine_rpc::paper_reconcile::PaperRecordInput, Verdict)> =
        Vec::with_capacity(records.len());
    for (i, rec) in records.into_iter().enumerate() {
        let engine_submit_ns = rec.timestamp_ns as u128 + one_way_ns;
        let to = rec.block_number_seen.saturating_add(window);
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
                "  route {:>5} block {} -> {v:?}",
                rec.route_id, rec.block_number_seen
            );
        }
        reconciled.push((rec, v));
    }

    let verdicts: Vec<Verdict> = reconciled.iter().map(|(_, v)| *v).collect();
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

    // W2.2 formal mirage test: persistent nobody-took groups.
    let groups = mirage_groups(&reconciled, mirage_min_records, mirage_min_span_ns);
    if groups.is_empty() {
        println!(
            "mirage scan: no pool-set with ≥{mirage_min_records} records spanning ≥{}s",
            mirage_min_span_ns / 1_000_000_000
        );
    } else {
        println!("\n── mirage scan (nobody-took with persistent net = pricing artifact) ──");
        for g in &groups {
            println!(
                "  {}  records={} span={}s  → {}",
                g.pools
                    .iter()
                    .map(|p| format!("0x{}…", &p[..8.min(p.len())]))
                    .collect::<Vec<_>>()
                    .join("+"),
                g.records,
                g.span_ns / 1_000_000_000,
                if g.all_uncontested {
                    "MIRAGE-SUSPECT (exclude from profit projections)"
                } else {
                    "contested (live flow)"
                }
            );
        }
    }
    Ok(())
}
