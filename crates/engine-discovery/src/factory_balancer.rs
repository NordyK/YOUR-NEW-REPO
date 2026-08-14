//! Balancer V2 pool discovery via the Vault `TokensRegistered` event.
//!
//! Balancer V2 registers every pool's tokens with the singleton Vault:
//! ```text
//! event TokensRegistered(bytes32 indexed poolId, address[] tokens, address[] assetManagers);
//! ```
//! The pool (BPT) address is the high 20 bytes of `poolId`. The token list is in the
//! data field, so we get a pool's tokens without a per-pool RPC. We emit one
//! `DiscoveredPool` per allowlisted token PAIR (the route layer is pairwise); the BPT
//! token (== pool address, present for ComposableStable) is excluded.
//!
//! This event fires for ALL Balancer pools (weighted too). We don't have stable-pool
//! math via the Vault event alone, so the snapshot step (`fetch_balancer_stable`,
//! which calls `getAmplificationParameter`) is what filters stable from weighted —
//! weighted pools revert that call and are dropped at snapshot time.

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info};

use crate::logs::{fetch_logs_chunked, LogEntry};
use crate::types::{Address, DiscoveredPool, DiscoveryError};

/// Topic hash for `TokensRegistered(bytes32,address[],address[])`.
pub fn tokens_registered_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut k = Keccak::v256();
        k.update(b"TokensRegistered(bytes32,address[],address[])");
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    })
}

/// Balancer Vault scan config.
#[derive(Debug, Clone)]
pub struct BalancerVaultConfig {
    /// Vault contract address.
    pub vault: Address,
    /// Earliest block to scan (Balancer V2 Vault deploy on Arbitrum).
    pub deployment_block: u64,
}

/// Scan the Vault for `TokensRegistered`, keep pools with ≥2 allowlisted (non-BPT)
/// tokens, and emit one `DiscoveredPool{family: BalancerStable}` per allowlisted pair.
pub fn discover_balancer_pools(
    client: &ArbitrumRpcClient,
    cfg: &BalancerVaultConfig,
    head_block: u64,
    chunk_blocks: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    let topic0 = tokens_registered_topic();
    info!(
        vault = ?cfg.vault,
        from = cfg.deployment_block,
        to = head_block,
        chunk = chunk_blocks,
        "scanning Balancer TokensRegistered logs"
    );
    let logs = fetch_logs_chunked(
        client,
        &cfg.vault,
        topic0,
        cfg.deployment_block,
        head_block,
        chunk_blocks,
        4,
    )?;
    debug!(
        count = logs.len(),
        "raw Balancer TokensRegistered logs returned"
    );
    let mut out = Vec::new();
    let mut seen_pairs: AHashSet<(Address, Address, Address)> = AHashSet::default();
    for log in &logs {
        let (pool_addr, tokens, block) = match decode_tokens_registered(log) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(?e, ?log.block_number, "malformed Balancer log; skipping");
                continue;
            }
        };
        // Keep allowlisted tokens, excluding the BPT (== pool address).
        let mut allow: Vec<Address> = tokens
            .into_iter()
            .filter(|t| *t != pool_addr && token_allowlist.contains(t))
            .collect();
        allow.sort_unstable();
        allow.dedup();
        if allow.len() < 2 {
            continue;
        }
        // Emit each unordered pair (token0 < token1 by address).
        for a in 0..allow.len() {
            for b in (a + 1)..allow.len() {
                let (t0, t1) = (allow[a], allow[b]);
                if !seen_pairs.insert((pool_addr, t0, t1)) {
                    continue;
                }
                out.push(DiscoveredPool {
                    address: pool_addr,
                    family: PoolKind::BalancerStable,
                    token0: t0,
                    token1: t1,
                    fee_pips: 0,
                    fee_bps: 0,
                    tick_spacing: 0,
                    created_block: block,
                });
            }
        }
    }
    info!(vault = ?cfg.vault, discovered_pairs = out.len(), "Balancer discovery complete");
    Ok(out)
}

/// Decode `(pool_address, tokens, block)` from a `TokensRegistered` log.
fn decode_tokens_registered(
    log: &LogEntry,
) -> Result<(Address, Vec<Address>, u64), DiscoveryError> {
    if log.topics.len() != 2 {
        return Err(DiscoveryError::MalformedLog(
            "Balancer TokensRegistered topic count",
        ));
    }
    // poolId = topics[1]; pool address = high 20 bytes.
    let pool_id = &log.topics[1];
    let mut pool_addr = [0u8; 20];
    pool_addr.copy_from_slice(&pool_id[0..20]);
    // data: [0:32]=offset to tokens[], [32:64]=offset to assetManagers[]
    if log.data.len() < 64 {
        return Err(DiscoveryError::MalformedLog("Balancer data too short"));
    }
    let off_tokens = word_usize(&log.data, 0)?;
    let len = word_usize(&log.data, off_tokens)?;
    if len > 16 {
        return Err(DiscoveryError::MalformedLog(
            "Balancer token array too long",
        ));
    }
    let mut tokens = Vec::with_capacity(len);
    for k in 0..len {
        let base = off_tokens + 32 + k * 32;
        if log.data.len() < base + 32 {
            return Err(DiscoveryError::MalformedLog("Balancer token word short"));
        }
        if log.data[base..base + 12].iter().any(|&x| x != 0) {
            return Err(DiscoveryError::MalformedLog(
                "Balancer token padding nonzero",
            ));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&log.data[base + 12..base + 32]);
        tokens.push(a);
    }
    Ok((pool_addr, tokens, log.block_number))
}

fn word_usize(data: &[u8], off: usize) -> Result<usize, DiscoveryError> {
    // `off` is an attacker-influenced ABI offset word — compute `off+32` with a checked add so a
    // crafted astronomical offset returns MalformedLog instead of overflowing usize (release: wraps
    // → reversed slice range → panic; debug: add-overflow panic). Mirrors multicall::read_offset.
    let end = off
        .checked_add(32)
        .ok_or(DiscoveryError::MalformedLog("Balancer word offset overflow"))?;
    if data.len() < end {
        return Err(DiscoveryError::MalformedLog("Balancer word short"));
    }
    // Only the low 8 bytes matter for offsets/lengths in practice.
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[off + 24..end]);
    Ok(u64::from_be_bytes(b) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_is_stable() {
        let t = tokens_registered_topic();
        // Recompute and compare (sanity that OnceLock is stable).
        let mut k = Keccak::v256();
        k.update(b"TokensRegistered(bytes32,address[],address[])");
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        assert_eq!(t, &out);
    }
}

#[cfg(test)]
mod abi_offset_overflow_tests {
    //! AUD-ABI-OFFSET: `word_usize` must reject an astronomical ABI offset with an Err,
    //! never panic on the `off+32` usize overflow (reachable from a hostile eth_getLogs response).
    use super::*;
    #[test]
    fn word_usize_rejects_astronomical_offset_without_panic() {
        let data = [0u8; 64];
        let r = word_usize(&data, usize::MAX - 10); // off+32 wraps
        assert!(r.is_err(), "astronomical offset must Err, not panic");
    }
}
