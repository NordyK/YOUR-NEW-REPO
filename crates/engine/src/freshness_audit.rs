//! Freshness auditor (env `FRESHNESS_AUDIT=1`, background core).
//!
//! Directly tests the stale-state phantom hypothesis: for a set of watch pools (default: the
//! DODO WBTC/USDC pool behind the recurring WBTC/WETH/USDC triangle), it periodically
//! compares the engine's STORED pool state to a FRESH on-chain snapshot and logs the
//! staleness (blocks since the stored state's update block) and any price divergence.
//!
//! If the stored DODO oracle `i` diverges from fresh on-chain `i` while the engine is
//! emitting the triangle as profitable, the "arb" is a stale-state phantom — not a kernel
//! bug (the kernel was proven to match on-chain `querySellBase`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ahash::AHashMap;
use engine_amm::dodo_v2::DodoPoolClass;
use engine_rpc::{dodo_snapshot::fetch_dodo_state, ArbitrumRpcClient, BlockTag};
use engine_state::{PoolId, PoolKind};
use tracing::{info, warn};

use crate::pipeline::DetectionContext;

/// Default watch set: all three legs of the recurring WBTC/WETH/USDC triangle — DODO
/// WBTC/USDC plus the two UniV3 legs (WBTC/WETH and WETH/USDC). Auditing all three locates
/// which leg (if any) goes stale, and the V3 audit also logs each leg's fee tier (which
/// determines whether the cross-venue spread can beat round-trip fees = a real arb).
const DEFAULT_WATCH: &[[u8; 20]] = &[
    [
        0x0d, 0x32, 0xa3, 0xbb, 0x04, 0xe8, 0xff, 0x29, 0xf0, 0x97, 0x4c, 0x9d, 0x3f, 0x3d, 0x85,
        0x01, 0x2a, 0x0b, 0x5d, 0x14,
    ], // DODO WBTC/USDC
    [
        0x2f, 0x5e, 0x87, 0xc9, 0x31, 0x2f, 0xa2, 0x9a, 0xed, 0x5c, 0x17, 0x9e, 0x45, 0x66, 0x25,
        0xd7, 0x90, 0x15, 0x29, 0x9c,
    ], // UniV3 WBTC/WETH
    [
        0xc6, 0x96, 0x20, 0x04, 0xf4, 0x52, 0xbe, 0x92, 0x03, 0x59, 0x19, 0x91, 0xd1, 0x5f, 0x6b,
        0x38, 0x8e, 0x09, 0xe8, 0xd0,
    ], // UniV3 WETH/USDC
];

fn hex20(a: &[u8; 20]) -> String {
    let mut s = String::with_capacity(42);
    s.push_str("0x");
    for b in a {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_watch_env() -> Vec<[u8; 20]> {
    match std::env::var("WATCH_POOLS") {
        Ok(s) if !s.is_empty() => s
            .split(',')
            .filter_map(|h| {
                let h = h.trim().trim_start_matches("0x");
                let mut out = [0u8; 20];
                faster_hex::hex_decode(h.as_bytes(), &mut out)
                    .ok()
                    .map(|_| out)
            })
            .collect(),
        _ => DEFAULT_WATCH.to_vec(),
    }
}

/// Run the freshness auditor until `shutdown`.
pub fn freshness_audit_loop(
    shutdown: &AtomicBool,
    rpc_url: String,
    detection: &Arc<DetectionContext>,
    interval_secs: u64,
) {
    let client = ArbitrumRpcClient::new(rpc_url);
    let watch = parse_watch_env();
    // Resident address → PoolId.
    let addr_to_id: AHashMap<[u8; 20], PoolId> = detection
        .universe
        .registry
        .pools()
        .map(|(a, id)| (*a, *id))
        .collect();
    info!(
        watch = watch.len(),
        "FRESHNESS_AUDIT online — comparing stored vs fresh state"
    );

    while !shutdown.load(Ordering::Acquire) {
        // Sleep in short slices so shutdown is responsive.
        for _ in 0..(interval_secs.max(1) * 2) {
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let head = match client.block_number() {
            Ok(h) => h,
            Err(_) => continue,
        };
        let block = BlockTag::Number(head);
        for addr in &watch {
            let Some(&pid) = addr_to_id.get(addr) else {
                warn!(pool = %hex20(addr), "FRESHNESS_AUDIT: pool not resident (engine never quotes it)");
                continue;
            };
            let kind = detection.universe.pools.meta(pid).kind;
            match kind {
                PoolKind::DodoV2 => audit_dodo(&client, detection, addr, pid, block, head),
                PoolKind::UniswapV3
                | PoolKind::SushiV3
                | PoolKind::PancakeV3
                | PoolKind::KyberSwapElastic => audit_v3(&client, detection, addr, pid, block),
                other => {
                    info!(pool = %hex20(addr), ?other, "FRESHNESS_AUDIT: unsupported watch kind")
                }
            }
        }
    }
}

fn audit_dodo(
    client: &ArbitrumRpcClient,
    detection: &Arc<DetectionContext>,
    addr: &[u8; 20],
    pid: PoolId,
    block: BlockTag,
    head: u64,
) {
    let slot = detection.universe.pools.meta(pid).state_slot;
    let stored = detection.universe.dodo_state.snapshot(slot);
    let fresh = [DodoPoolClass::Dvm, DodoPoolClass::Dpp, DodoPoolClass::Dsp]
        .into_iter()
        .find_map(|c| fetch_dodo_state(client, addr, c, block, head).ok());
    let Some(fresh) = fresh else {
        warn!(pool = %hex20(addr), "FRESHNESS_AUDIT: fresh DODO snapshot failed");
        return;
    };
    // i is quote/base in 1e18; divergence in basis points.
    let stored_i = stored.i;
    let fresh_i = fresh.i;
    let i_bps_diff = bps_diff(stored_i, fresh_i);
    let base_bps = bps_diff(stored.base_reserve, fresh.base_reserve);
    let quote_bps = bps_diff(stored.quote_reserve, fresh.quote_reserve);
    // Verdict is on VALUE divergence (the DodoState block stamp is unreliable, often 0).
    let stale = i_bps_diff > 20 || base_bps > 50 || quote_bps > 50;
    info!(
        pool = %hex20(addr),
        stored_i = %stored_i,
        fresh_i = %fresh_i,
        i_diff_bps = i_bps_diff,
        base_diff_bps = base_bps,
        quote_diff_bps = quote_bps,
        head,
        verdict = if stale { "STALE — stored value diverges from chain" } else { "FRESH — stored matches chain" },
        "FRESHNESS_AUDIT dodo"
    );
}

fn audit_v3(
    client: &ArbitrumRpcClient,
    detection: &Arc<DetectionContext>,
    addr: &[u8; 20],
    pid: PoolId,
    block: BlockTag,
) {
    let slot = detection.universe.pools.meta(pid).state_slot;
    let stored = detection.universe.v3_state.snapshot(slot);
    // Fresh slot0(): sqrtPriceX96 in the low 160 bits of the first word.
    let slot0_sel = [0x38u8, 0x50, 0xc7, 0xbd];
    let fresh_sqrt = match client.eth_call(addr, &slot0_sel, block) {
        Ok(b) if b.len() >= 32 => {
            use alloy_primitives::U256;
            let word = U256::from_be_slice(&b[0..32]);
            let mask = (U256::from(1u8) << 160usize) - U256::from(1u8);
            word & mask
        }
        _ => {
            warn!(pool = %hex20(addr), "FRESHNESS_AUDIT: fresh slot0 read failed");
            return;
        }
    };
    let sqrt_bps = bps_diff(stored.sqrt_price_x96, fresh_sqrt);
    // Price ∝ sqrt²; a sqrt divergence of N bps ≈ 2N bps of price divergence.
    let stale = sqrt_bps > 10;
    info!(
        pool = %hex20(addr),
        fee_pips = stored.fee_pips,
        stored_sqrt = %stored.sqrt_price_x96,
        fresh_sqrt = %fresh_sqrt,
        sqrt_diff_bps = sqrt_bps,
        price_diff_bps = sqrt_bps * 2,
        verdict = if stale { "STALE — stored slot0 diverges from chain" } else { "FRESH — stored matches chain" },
        "FRESHNESS_AUDIT v3"
    );
}

/// Absolute difference between two 1e18 values in basis points (of the larger).
fn bps_diff(a: alloy_primitives::U256, b: alloy_primitives::U256) -> u64 {
    use alloy_primitives::U256;
    if a == b {
        return 0;
    }
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    let diff = hi - lo;
    let bps = diff.saturating_mul(U256::from(10_000u64)) / hi.max(U256::from(1u8));
    bps.try_into().unwrap_or(u64::MAX)
}
