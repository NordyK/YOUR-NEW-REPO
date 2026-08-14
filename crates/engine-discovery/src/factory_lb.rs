//! Trader Joe Liquidity Book (LB v2.1 / v2.2) factory `LBPairCreated` log scanner.
//!
//! Event signature:
//! ```text
//! event LBPairCreated(
//!     address indexed tokenX,
//!     address indexed tokenY,
//!     uint256 indexed binStep,
//!     address         LBPair,
//!     uint256         pid
//! );
//! ```
//!
//! The three indexed args occupy `topics[1..=3]`; `LBPair` and `pid` live in the
//! data field (each padded to 32 bytes). LB sorts tokenX < tokenY on-chain, so
//! `tokenX == token0` and selling token0 is `zero_for_one` in the kernel.
//!
//! The event hash is `keccak256("LBPairCreated(address,address,uint256,address,uint256)")`,
//! computed at init time so the canonical signature is the single source of truth.

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info};

use crate::logs::{fetch_logs_chunked, LogEntry};
use crate::types::{Address, DiscoveredPool, DiscoveryError};

/// Topic hash for `LBPairCreated`. Computed lazily on first access via OnceLock.
pub fn lb_pair_created_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut k = Keccak::v256();
        k.update(b"LBPairCreated(address,address,uint256,address,uint256)");
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    })
}

/// Public alias — ergonomic name for the topic getter.
pub const LB_PAIR_CREATED_TOPIC: fn() -> &'static [u8; 32] = lb_pair_created_topic;

/// One LB factory to scan (v2.1 at `0x8e42f2F4...`, optionally v2.2).
#[derive(Debug, Clone)]
pub struct LbFactoryConfig {
    /// Factory contract address.
    pub address: Address,
    /// Earliest block to scan from.
    pub deployment_block: u64,
}

/// Scan a single LB factory for every `LBPairCreated` event between
/// `deployment_block` and `head_block`, decode each, drop pools whose
/// `tokenX`/`tokenY` are outside `token_allowlist`, and emit `DiscoveredPool`
/// records tagged `PoolKind::TraderJoeLb`.
pub fn discover_lb_pools(
    client: &ArbitrumRpcClient,
    factory: &LbFactoryConfig,
    head_block: u64,
    chunk_blocks: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    let topic0 = lb_pair_created_topic();
    info!(
        factory = ?factory.address,
        from = factory.deployment_block,
        to = head_block,
        chunk = chunk_blocks,
        "scanning LB LBPairCreated logs"
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
    debug!(count = logs.len(), "raw LB LBPairCreated logs returned");
    let mut out = Vec::with_capacity(logs.len() / 4);
    for log in logs {
        match decode_lb_pair_created(&log) {
            Ok((token0, token1, bin_step, pool, block)) => {
                if !token_allowlist.contains(&token0) || !token_allowlist.contains(&token1) {
                    continue;
                }
                out.push(DiscoveredPool {
                    address: pool,
                    family: PoolKind::TraderJoeLb,
                    token0,
                    token1,
                    // LB fees are dynamic (base + variable). Stash the binStep in
                    // `fee_pips` so the snapshot phase can fetch real fee params;
                    // the registered fee (bps) is resolved from on-chain state.
                    fee_pips: bin_step,
                    fee_bps: 0,
                    tick_spacing: 0,
                    created_block: block,
                });
            }
            Err(e) => {
                tracing::warn!(?e, ?log.address, ?log.block_number, "malformed LB log; skipping");
            }
        }
    }
    info!(
        factory = ?factory.address,
        discovered = out.len(),
        "LB LBPairCreated discovery complete"
    );
    Ok(out)
}

/// Decode an `LBPairCreated` log into `(token0, token1, bin_step, pool, block)`.
fn decode_lb_pair_created(
    log: &LogEntry,
) -> Result<(Address, Address, u32, Address, u64), DiscoveryError> {
    if log.topics.len() != 4 {
        return Err(DiscoveryError::MalformedLog("LB LBPairCreated topic count"));
    }
    // topics[1] = tokenX (== token0, sorted X<Y), topics[2] = tokenY, topics[3] = binStep.
    let token0 = address_from_topic(&log.topics[1])?;
    let token1 = address_from_topic(&log.topics[2])?;
    let bin_step = u32_from_topic(&log.topics[3])?;
    // data = LBPair (32 bytes, address left-padded) + pid (32 bytes, unused).
    if log.data.len() < 64 {
        return Err(DiscoveryError::MalformedLog("LB LBPairCreated data len"));
    }
    let pool = address_from_topic(&log.data[0..32].try_into().expect("32 bytes"))?;
    Ok((token0, token1, bin_step, pool, log.block_number))
}

/// Extract a 20-byte address from a 32-byte ABI word (low 20 bytes, leading 12 zero).
fn address_from_topic(t: &[u8; 32]) -> Result<Address, DiscoveryError> {
    if t[0..12].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("address padding nonzero"));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&t[12..32]);
    Ok(a)
}

/// Extract a `uint256` binStep from an indexed-topic word as `u32`. Real LB bin
/// steps are tiny (1..=200 bp); reject anything that overflows the low 4 bytes.
fn u32_from_topic(t: &[u8; 32]) -> Result<u32, DiscoveryError> {
    if t[0..28].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("binStep too large"));
    }
    Ok(u32::from_be_bytes([t[28], t[29], t[30], t[31]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_matches_known_lb_pair_created_hash() {
        // keccak256("LBPairCreated(address,address,uint256,address,uint256)").
        let computed = lb_pair_created_topic();
        let mut got = [0u8; 64];
        faster_hex::hex_encode(computed, &mut got).unwrap();
        let got_str = std::str::from_utf8(&got).unwrap();
        assert_eq!(
            got_str,
            "2c8d104b27c6b7f4492017a6f5cf3803043688934ebcaa6a03540beeaf976aff"
        );
    }

    #[test]
    fn decode_well_formed_log() {
        let mut topics = vec![*lb_pair_created_topic(), [0u8; 32], [0u8; 32], [0u8; 32]];
        topics[1][12..32].copy_from_slice(&[0xAA; 20]); // tokenX
        topics[2][12..32].copy_from_slice(&[0xBB; 20]); // tokenY
        topics[3][31] = 20; // binStep = 20
        let mut data = vec![0u8; 64];
        data[12..32].copy_from_slice(&[0xDD; 20]); // LBPair
        let log = LogEntry {
            address: [0u8; 20],
            topics,
            data,
            block_number: 12345,
        };
        let (t0, t1, bin_step, pool, blk) = decode_lb_pair_created(&log).unwrap();
        assert_eq!(t0, [0xAA; 20]);
        assert_eq!(t1, [0xBB; 20]);
        assert_eq!(bin_step, 20);
        assert_eq!(pool, [0xDD; 20]);
        assert_eq!(blk, 12345);
    }

    #[test]
    fn rejects_wrong_topic_count() {
        let log = LogEntry {
            address: [0u8; 20],
            topics: vec![*lb_pair_created_topic()],
            data: vec![0u8; 64],
            block_number: 1,
        };
        assert!(decode_lb_pair_created(&log).is_err());
    }
}
