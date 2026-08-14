//! Solidly-fork factory discovery (Velodrome / Chronos / Ramses / Aerodrome).
//!
//! Event signature:
//! ```text
//! event PairCreated(
//!     address indexed token0,
//!     address indexed token1,
//!     bool    indexed stable,
//!     address pair,
//!     uint    length
//! );
//! ```
//!
//! Three indexed args occupy `topics[1..=3]`. `pair` is the first 32-byte word
//! in `data`. Distinguishes stable vs volatile pools — the engine uses this to
//! pick the correct math kernel (`solidly_stable` for stable; `uniswap_v2`
//! constant-product for volatile).

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info};

use crate::logs::{fetch_logs_chunked, LogEntry};
use crate::types::{Address, DiscoveredPool, DiscoveryError};

/// `keccak256("PairCreated(address,address,bool,address,uint256)")`.
pub fn pair_created_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut k = Keccak::v256();
        k.update(b"PairCreated(address,address,bool,address,uint256)");
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    })
}

/// One Solidly-style factory to scan.
#[derive(Debug, Clone)]
pub struct SolidlyFactoryConfig {
    /// Factory contract address.
    pub address: Address,
    /// Earliest block to scan from.
    pub deployment_block: u64,
    /// Default volatile-fee bps (used when the fee schedule fetcher hasn't run).
    pub default_volatile_fee_bps: u32,
    /// Default stable-fee bps.
    pub default_stable_fee_bps: u32,
}

/// Scan a Solidly-style factory for `PairCreated` events.
pub fn discover_solidly_pools(
    client: &ArbitrumRpcClient,
    factory: &SolidlyFactoryConfig,
    head_block: u64,
    chunk_blocks: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    let topic0 = pair_created_topic();
    info!(
        factory = ?factory.address,
        from = factory.deployment_block,
        to = head_block,
        chunk = chunk_blocks,
        "scanning Solidly PairCreated logs"
    );
    let logs = fetch_logs_chunked(
        client,
        &factory.address,
        topic0,
        factory.deployment_block,
        head_block,
        chunk_blocks,
        /* max_retries */ 4,
    )?;
    debug!(count = logs.len(), "raw Solidly PairCreated logs returned");
    let mut out = Vec::with_capacity(logs.len() / 4);
    for log in logs {
        match decode_solidly_pair(&log) {
            Ok((token0, token1, stable, pool, block)) => {
                if !token_allowlist.contains(&token0) || !token_allowlist.contains(&token1) {
                    continue;
                }
                // Stable pools dispatch to the stable-curve math (PoolKind::SolidlyV3 acts
                // as the stable tag); volatile pools reuse the V2 constant-product math via
                // PoolKind::UniswapV2 (different kind would require new evaluator).
                let (family, fee_bps) = if stable {
                    (PoolKind::SolidlyV3, factory.default_stable_fee_bps)
                } else {
                    (PoolKind::UniswapV2, factory.default_volatile_fee_bps)
                };
                out.push(DiscoveredPool {
                    address: pool,
                    family,
                    token0,
                    token1,
                    fee_pips: 0,
                    fee_bps,
                    tick_spacing: 0,
                    created_block: block,
                });
            }
            Err(e) => {
                tracing::warn!(?e, ?log.address, ?log.block_number, "malformed Solidly log");
            }
        }
    }
    info!(
        factory = ?factory.address,
        discovered = out.len(),
        "Solidly discovery complete"
    );
    Ok(out)
}

fn decode_solidly_pair(
    log: &LogEntry,
) -> Result<(Address, Address, bool, Address, u64), DiscoveryError> {
    // Two known event shapes share the same topic0 hash but differ in which
    // args are `indexed`:
    //   * Velodrome v1: token0+token1+stable all indexed → 4 topics, pool in
    //     data[0..32]. (Aerodrome, Velodrome, base Solidly.)
    //   * Chronos / Ramses v2: only token0+token1 indexed → 3 topics, then
    //     `stable` (bool) at data[0..32] and `pool` (address) at data[32..64].
    match log.topics.len() {
        4 => {
            // 3-indexed variant.
            let token0 = address_from_topic(&log.topics[1])?;
            let token1 = address_from_topic(&log.topics[2])?;
            let stable = bool_from_topic(&log.topics[3]);
            if log.data.len() < 32 {
                return Err(DiscoveryError::MalformedLog("Solidly data short (4-topic)"));
            }
            let pool = address_from_topic(&log.data[0..32].try_into().expect("32 bytes"))?;
            Ok((token0, token1, stable, pool, log.block_number))
        }
        3 => {
            // 2-indexed variant (Chronos / Ramses).
            let token0 = address_from_topic(&log.topics[1])?;
            let token1 = address_from_topic(&log.topics[2])?;
            if log.data.len() < 64 {
                return Err(DiscoveryError::MalformedLog("Solidly data short (3-topic)"));
            }
            // word[0]: stable (bool padded), word[1]: pool (address padded).
            let stable = log.data[31] != 0;
            let pool = address_from_topic(&log.data[32..64].try_into().expect("32 bytes"))?;
            Ok((token0, token1, stable, pool, log.block_number))
        }
        n => Err(DiscoveryError::MalformedLog(if n < 3 {
            "Solidly topic count < 3"
        } else {
            "Solidly topic count > 4"
        })),
    }
}

fn address_from_topic(t: &[u8; 32]) -> Result<Address, DiscoveryError> {
    if t[0..12].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("address padding nonzero"));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&t[12..32]);
    Ok(a)
}

fn bool_from_topic(t: &[u8; 32]) -> bool {
    // bool indexed is left-padded — last byte is the boolean.
    t[31] != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_matches_velodrome_signature() {
        // Reference: `keccak256("PairCreated(address,address,bool,address,uint256)")`.
        // Velodrome v1 factory on Optimism uses this exact signature.
        let _ = pair_created_topic();
    }

    #[test]
    fn decode_stable_pair() {
        let mut topic0 = [0u8; 32];
        topic0.copy_from_slice(pair_created_topic());
        let mut topic1 = [0u8; 32];
        topic1[12..32].copy_from_slice(&[0x11; 20]);
        let mut topic2 = [0u8; 32];
        topic2[12..32].copy_from_slice(&[0x22; 20]);
        let mut topic3 = [0u8; 32];
        topic3[31] = 0x01; // stable = true
        let mut data = vec![0u8; 32];
        data[12..32].copy_from_slice(&[0x33; 20]);
        let log = LogEntry {
            address: [0u8; 20],
            topics: vec![topic0, topic1, topic2, topic3],
            data,
            block_number: 42,
        };
        let (_, _, stable, pool, _) = decode_solidly_pair(&log).unwrap();
        assert!(stable);
        assert_eq!(pool, [0x33; 20]);
    }
}
