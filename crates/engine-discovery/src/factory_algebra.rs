//! Algebra-V1.9 (`AlgebraV19` → Camelot V3) `Pool` log scanner.
//!
//! Algebra forks emit a different creation event than Uniswap V3. The canonical signature
//! in `AlgebraFactory.sol` is:
//! ```text
//! event Pool(
//!     address indexed token0,
//!     address indexed token1,
//!     address          pool
//! );
//! ```
//!
//! Indexed args occupy `topics[1..=2]` (no `fee` topic because Algebra fees are dynamic
//! per-pool via a plugin, not fixed per-tier). `pool` is in the data field, padded to 32
//! bytes. The topic-0 hash is `keccak256("Pool(address,address,address)")`, computed at
//! init time from the canonical signature string.
//!
//! There is no `fee` or `tickSpacing` in the event itself; we record `fee_pips = 0` and
//! `tick_spacing = 60` (the canonical Algebra default) and let the runtime pool reader
//! resolve the actual fee dynamically via `globalState().fee` when the pool is first
//! warmed for evaluation.

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info};

use crate::logs::{fetch_logs_chunked, LogEntry};
use crate::types::{Address, DiscoveredPool, DiscoveryError};

/// Topic hash for Algebra `Pool` event. Computed lazily on first access via OnceLock so
/// the canonical signature is the single source of truth.
pub fn algebra_pool_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut k = Keccak::v256();
        k.update(b"Pool(address,address,address)");
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    })
}

/// Public alias for ergonomics.
pub const ALGEBRA_POOL_TOPIC: fn() -> &'static [u8; 32] = algebra_pool_topic;

/// One Algebra-family factory to scan.
#[derive(Debug, Clone)]
pub struct AlgebraFactoryConfig {
    /// Factory contract address.
    pub address: Address,
    /// AMM family discriminant — currently `AlgebraV19` (Camelot V3 on Arbitrum).
    pub family: PoolKind,
    /// Earliest block to scan from.
    pub deployment_block: u64,
}

/// Scan a single Algebra factory for every `Pool` event between `deployment_block` and
/// `head_block`, decode each, drop pools whose `token0`/`token1` are outside
/// `token_allowlist`, and emit `DiscoveredPool` records.
pub fn discover_algebra_pools(
    client: &ArbitrumRpcClient,
    factory: &AlgebraFactoryConfig,
    head_block: u64,
    chunk_blocks: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    let topic0 = algebra_pool_topic();
    info!(
        factory = ?factory.address,
        from = factory.deployment_block,
        to = head_block,
        chunk = chunk_blocks,
        "scanning Algebra Pool logs"
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
    debug!(count = logs.len(), "raw Algebra Pool logs returned");
    let mut out = Vec::with_capacity(logs.len() / 4);
    for log in logs {
        match decode_algebra_pool(&log) {
            Ok((token0, token1, pool, block)) => {
                if !token_allowlist.contains(&token0) || !token_allowlist.contains(&token1) {
                    continue;
                }
                out.push(DiscoveredPool {
                    address: pool,
                    family: factory.family,
                    token0,
                    token1,
                    // Algebra fees are dynamic; the per-swap fee is read from
                    // `globalState().fee` when the pool is warmed. We set 0 here so the
                    // discovery record reflects "fee not yet resolved" and any consumer
                    // that needs the live fee must read it from chain state.
                    fee_pips: 0,
                    fee_bps: 0,
                    // Algebra V1.9 default tick spacing on Arbitrum is 60. The actual
                    // tick spacing can change via governance but the canonical
                    // deployment uses 60.
                    tick_spacing: 60,
                    created_block: block,
                });
            }
            Err(e) => {
                tracing::warn!(?e, ?log.address, ?log.block_number, "malformed Algebra log; skipping");
            }
        }
    }
    info!(
        factory = ?factory.address,
        discovered = out.len(),
        "Algebra Pool discovery complete"
    );
    Ok(out)
}

/// Decode a single Algebra `Pool` log into `(token0, token1, pool, block)`.
fn decode_algebra_pool(log: &LogEntry) -> Result<(Address, Address, Address, u64), DiscoveryError> {
    if log.topics.len() != 3 {
        return Err(DiscoveryError::MalformedLog("Algebra Pool topic count"));
    }
    // topics[1] = token0 (left-padded 32 bytes), topics[2] = token1.
    let token0 = address_from_topic(&log.topics[1])?;
    let token1 = address_from_topic(&log.topics[2])?;
    // data = pool (32 bytes, address left-padded).
    if log.data.len() < 32 {
        return Err(DiscoveryError::MalformedLog("Algebra Pool data len"));
    }
    let pool = address_from_topic(&log.data[0..32].try_into().expect("32 bytes"))?;
    Ok((token0, token1, pool, log.block_number))
}

/// Extract a 20-byte address from a 32-byte ABI word. The address occupies the *low*
/// 20 bytes (right-aligned) and the leading 12 bytes are zero.
fn address_from_topic(t: &[u8; 32]) -> Result<Address, DiscoveryError> {
    if t[0..12].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("address padding nonzero"));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&t[12..32]);
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_matches_known_algebra_pool_hash() {
        // Reference: keccak256("Pool(address,address,address)").
        // Cross-checked against the Algebra V1.9 source and Arbiscan event lookup.
        let computed = algebra_pool_topic();
        let mut got = [0u8; 64];
        faster_hex::hex_encode(computed, &mut got).unwrap();
        let got_str = std::str::from_utf8(&got).unwrap();
        assert_eq!(
            got_str,
            "91ccaa7a278130b65168c3a0c8d3bcae84cf5e43704342bd3ec0b59e59c036db"
        );
    }

    #[test]
    fn decodes_well_formed_log() {
        let mut topic0 = [0u8; 32];
        topic0.copy_from_slice(algebra_pool_topic());
        let mut topic1 = [0u8; 32];
        topic1[12..32].copy_from_slice(&[0xAA; 20]);
        let mut topic2 = [0u8; 32];
        topic2[12..32].copy_from_slice(&[0xBB; 20]);
        let mut data = vec![0u8; 32];
        data[12..32].copy_from_slice(&[0xCC; 20]);
        let log = LogEntry {
            address: [0u8; 20],
            topics: vec![topic0, topic1, topic2],
            data,
            block_number: 42,
        };
        let (t0, t1, p, b) = decode_algebra_pool(&log).unwrap();
        assert_eq!(t0, [0xAA; 20]);
        assert_eq!(t1, [0xBB; 20]);
        assert_eq!(p, [0xCC; 20]);
        assert_eq!(b, 42);
    }
}
