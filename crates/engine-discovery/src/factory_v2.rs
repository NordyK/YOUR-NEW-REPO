//! Uniswap-V2-family (`UniswapV2`, `SushiV2`, `CamelotV2`) `PairCreated` log scanner.
//!
//! Event signature:
//! ```text
//! event PairCreated(
//!     address indexed token0,
//!     address indexed token1,
//!     address          pair,
//!     uint256
//! );
//! ```
//!
//! Indexed args occupy `topics[1..=2]`. `pair` and the unindexed `uint256` live in the
//! data field, each padded to 32 bytes.

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info};

use crate::factory_v3::{V3FactoryConfig, V3_POOL_CREATED_TOPIC};
use crate::logs::{fetch_logs_chunked, LogEntry};
use crate::types::{Address, DiscoveredPool, DiscoveryError};

/// Topic hash for V2 `PairCreated`. Same lazy-init pattern as `V3_POOL_CREATED_TOPIC`.
pub fn pair_created_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut k = Keccak::v256();
        k.update(b"PairCreated(address,address,address,uint256)");
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    })
}

/// Public alias.
pub const V2_PAIR_CREATED_TOPIC: fn() -> &'static [u8; 32] = pair_created_topic;

/// One V2 factory to scan. Same shape as `V3FactoryConfig` but the family selects a V2
/// pool-kind and the default fee is encoded as bps (30 = 0.30 % for Uniswap V2 forks;
/// Camelot V2 reads dynamic directional fees per swap and `fee_bps` is therefore an
/// uninitialised placeholder — `30` until the per-pool reader runs).
#[derive(Debug, Clone)]
pub struct V2FactoryConfig {
    /// Factory contract.
    pub address: Address,
    /// AMM family discriminant — `UniswapV2`, `SushiV2`, `CamelotV2`.
    pub family: PoolKind,
    /// Earliest block to scan.
    pub deployment_block: u64,
    /// Default fee in bps (used when the family is fixed-fee). Camelot V2 overrides at
    /// snapshot time via the per-pair `stableSwap` / `pairFee` reader.
    pub default_fee_bps: u32,
}

/// Scan a single V2 factory's `PairCreated` history. Same contract as `discover_v3_pools`.
pub fn discover_v2_pools(
    client: &ArbitrumRpcClient,
    factory: &V2FactoryConfig,
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
        "scanning V2 PairCreated logs"
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
    debug!(count = logs.len(), "raw V2 PairCreated logs returned");
    let mut out = Vec::with_capacity(logs.len() / 4);
    for log in logs {
        match decode_v2_pair_created(&log) {
            Ok((token0, token1, pair, block)) => {
                if !token_allowlist.contains(&token0) || !token_allowlist.contains(&token1) {
                    continue;
                }
                out.push(DiscoveredPool {
                    address: pair,
                    family: factory.family,
                    token0,
                    token1,
                    fee_pips: 0,
                    fee_bps: factory.default_fee_bps,
                    tick_spacing: 0,
                    created_block: block,
                });
            }
            Err(e) => {
                tracing::warn!(?e, ?log.address, ?log.block_number, "malformed V2 log; skipping");
            }
        }
    }
    info!(
        factory = ?factory.address,
        discovered = out.len(),
        "V2 PairCreated discovery complete"
    );
    Ok(out)
}

fn decode_v2_pair_created(
    log: &LogEntry,
) -> Result<(Address, Address, Address, u64), DiscoveryError> {
    if log.topics.len() != 3 {
        return Err(DiscoveryError::MalformedLog("V2 PairCreated topic count"));
    }
    let token0 = address_from_topic(&log.topics[1])?;
    let token1 = address_from_topic(&log.topics[2])?;
    if log.data.len() < 64 {
        return Err(DiscoveryError::MalformedLog("V2 PairCreated data len"));
    }
    let pair = address_from_topic(&log.data[0..32].try_into().expect("32 bytes"))?;
    Ok((token0, token1, pair, log.block_number))
}

fn address_from_topic(t: &[u8; 32]) -> Result<Address, DiscoveryError> {
    if t[0..12].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("address padding nonzero"));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&t[12..32]);
    Ok(a)
}

// Re-export V3 helpers so this module's public-API surface is clean. (No-op import keeps
// the symbol's name discoverable via cargo-doc cross-links between V2 and V3 modules.)
#[allow(dead_code)]
fn _link_v3_topic() -> &'static [u8; 32] {
    V3_POOL_CREATED_TOPIC()
}
#[allow(dead_code)]
fn _link_v3_factory_cfg() -> Option<V3FactoryConfig> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_matches_known_uniswap_v2_pair_created_hash() {
        // Reference: keccak256("PairCreated(address,address,address,uint256)") =
        //   0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9
        let computed = pair_created_topic();
        let mut got = [0u8; 64];
        faster_hex::hex_encode(computed, &mut got).unwrap();
        let got_str = std::str::from_utf8(&got).unwrap();
        assert_eq!(
            got_str,
            "0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9"
        );
    }
}
