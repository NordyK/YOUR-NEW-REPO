//! Curve StableSwap-NG plain-pool discovery via the factory's enumeration surface.
//!
//! Unlike V2/V3 factories (which share a `Pair/PoolCreated` topic) Curve has no common
//! create event, so we enumerate the StableSwap-NG factory directly via eth_calls — the
//! cleanest path, no log scan:
//! * `pool_count()` → `uint256`
//! * `pool_list(uint256 i)` → `address` for i in 0..count
//!
//! For each pool we read `coins(uint256 j)` (probing until revert / zero address) and
//! keep only PLAIN stable pools whose coins are ALL in the token allowlist — so every
//! coin participates in a cross-venue cycle. Pools with 2..=3 coins are admitted for v1;
//! a coin outside the allowlist (e.g. an LP token of a meta/lending pool, or crvUSD when
//! it isn't allowlisted) drops the whole pool. We emit one `DiscoveredPool` per
//! allowlisted coin PAIR (the route layer is pairwise); the snapshot step
//! (`fetch_curve_state`) fills the kernel state.

use ahash::AHashSet;
use engine_rpc::client::BlockTag;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info, warn};

use crate::types::{Address, DiscoveredPool, DiscoveryError};

fn selector(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

fn indexed_call(sel: [u8; 4], idx: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(36);
    data.extend_from_slice(&sel);
    let mut word = [0u8; 32];
    word[24..32].copy_from_slice(&idx.to_be_bytes());
    data.extend_from_slice(&word);
    data
}

fn decode_address(resp: &[u8]) -> Option<Address> {
    if resp.len() < 32 || resp[0..12].iter().any(|&b| b != 0) {
        return None;
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Some(a)
}

fn decode_count(resp: &[u8]) -> Option<u64> {
    if resp.len() < 32 {
        return None;
    }
    // Only the low 8 bytes matter for a realistic pool count.
    let mut b = [0u8; 8];
    b.copy_from_slice(&resp[24..32]);
    Some(u64::from_be_bytes(b))
}

/// Curve StableSwap-NG factory scan config.
#[derive(Debug, Clone)]
pub struct CurveFactoryConfig {
    /// Factory contract address (StableSwap-NG factory).
    pub address: Address,
    /// Maximum coins to accept per plain pool (v1 keeps it to small plain pools).
    pub max_coins: usize,
    /// Family to tag discovered pools with — `CurveStableSwapNg` for the StableSwap-NG factory,
    /// `CurveCryptoSwap` for the Tricrypto-NG/TwoCrypto-NG factory (same `pool_count()`/
    /// `pool_list(i)`/`coins(j)` enumeration surface; only the downstream kernel differs).
    pub family: PoolKind,
}

/// Enumerate a Curve factory's `pool_list` (StableSwap-NG **or** CryptoSwap-NG — both share the
/// `pool_count()`/`pool_list(i)`/`coins(j)` ABI), keep pools whose coins are ALL in the allowlist
/// (2..=`max_coins`), and emit one `DiscoveredPool {family: cfg.family}` per allowlisted coin pair.
pub fn discover_curve_pools(
    client: &ArbitrumRpcClient,
    cfg: &CurveFactoryConfig,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    let block = BlockTag::Latest;
    let count_sel = selector(b"pool_count()");
    let pool_list_sel = selector(b"pool_list(uint256)");
    let coins_sel = selector(b"coins(uint256)");
    // StableSwap-NG plain pools allow up to 8 coins; the v1 cap is usually 3.
    const CURVE_MAX_COINS: usize = 8;
    let max_coins = cfg.max_coins.clamp(2, CURVE_MAX_COINS);

    let count_resp = client.eth_call(&cfg.address, &count_sel, block)?;
    let raw_count =
        decode_count(&count_resp).ok_or(DiscoveryError::MalformedLog("curve pool_count decode"))?;
    // AUD-DISCOVERY-CAP: clamp an RPC-supplied count so a hostile/buggy `pool_count()` cannot spin
    // the per-index `eth_call` loop unbounded.
    let count = crate::capped_pool_count(raw_count as usize) as u64;
    if count != raw_count {
        warn!(factory = ?cfg.address, raw_count, count, "Curve pool_count clamped to cap");
    }
    info!(factory = ?cfg.address, pool_count = count, "enumerating Curve StableSwap-NG factory");

    let mut out = Vec::new();
    let mut seen_pairs: AHashSet<(Address, Address, Address)> = AHashSet::default();
    let mut scanned = 0u64;
    let mut admitted_pools = 0u64;
    for i in 0..count {
        let pool = match client.eth_call(&cfg.address, &indexed_call(pool_list_sel, i), block) {
            Ok(r) => match decode_address(&r) {
                Some(a) if a != [0u8; 20] => a,
                _ => continue,
            },
            Err(e) => {
                warn!(idx = i, ?e, "curve pool_list read failed; skipping index");
                continue;
            }
        };
        scanned += 1;
        // Read coins until revert / zero / over the cap.
        let mut coins: Vec<Address> = Vec::new();
        let mut over_cap = false;
        for j in 0..=max_coins {
            match client.eth_call(&pool, &indexed_call(coins_sel, j as u64), block) {
                Ok(r) => match decode_address(&r) {
                    Some(a) if a != [0u8; 20] => {
                        if j >= max_coins {
                            over_cap = true; // more coins than the v1 cap → skip (meta/large)
                            break;
                        }
                        coins.push(a);
                    }
                    _ => break,
                },
                Err(_) => break, // index out of range → end of coin list
            }
        }
        if over_cap || coins.len() < 2 {
            continue;
        }
        // PLAIN-pool filter: every coin must be allowlisted (so a coin outside the set —
        // e.g. an LP token from a meta/lending pool, or non-allowlisted crvUSD — drops
        // the whole pool and we never quote a phantom leg through an unknown token).
        if !coins.iter().all(|c| token_allowlist.contains(c)) {
            continue;
        }
        admitted_pools += 1;
        // Emit each unordered allowlisted pair (token0 < token1 by address).
        let mut allow: Vec<Address> = coins.clone();
        allow.sort_unstable();
        allow.dedup();
        for a in 0..allow.len() {
            for b in (a + 1)..allow.len() {
                let (t0, t1) = (allow[a], allow[b]);
                if !seen_pairs.insert((pool, t0, t1)) {
                    continue;
                }
                out.push(DiscoveredPool {
                    address: pool,
                    family: cfg.family,
                    token0: t0,
                    token1: t1,
                    fee_pips: 0,
                    fee_bps: 0,
                    tick_spacing: 0,
                    created_block: 0,
                });
            }
        }
    }
    debug!(scanned, admitted_pools, "curve coin scan complete");
    info!(
        factory = ?cfg.address,
        admitted_pools,
        discovered_pairs = out.len(),
        "Curve discovery complete"
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_decode() {
        let mut w = vec![0u8; 32];
        w[31] = 0x0b;
        w[30] = 0x01; // 0x10b = 267
        assert_eq!(decode_count(&w), Some(267));
    }

    #[test]
    fn address_decode_rejects_dirty_padding() {
        let mut w = vec![0u8; 32];
        w[0] = 1; // nonzero high byte → not a clean address word
        assert!(decode_address(&w).is_none());
        let mut ok = vec![0u8; 32];
        ok[12..32].copy_from_slice(&[0xab; 20]);
        assert_eq!(decode_address(&ok), Some([0xab; 20]));
    }

    #[test]
    fn indexed_call_layout() {
        let c = indexed_call(selector(b"pool_list(uint256)"), 5);
        assert_eq!(c.len(), 36);
        assert_eq!(c[35], 5);
    }
}
