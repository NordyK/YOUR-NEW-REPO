//! Uniswap-V3-family (`UniswapV3`, `SushiV3`, `PancakeV3`) `PoolCreated` log scanner.
//!
//! Event signature:
//! ```text
//! event PoolCreated(
//!     address indexed token0,
//!     address indexed token1,
//!     uint24  indexed fee,
//!     int24            tickSpacing,
//!     address          pool
//! );
//! ```
//!
//! Indexed args occupy `topics[1..=3]`. `tickSpacing` and `pool` live in the data field
//! (each padded to 32 bytes). The event hash is `keccak256("PoolCreated(address,address,uint24,int24,address)")`,
//! computed at init time from the canonical signature string so we never hard-code a
//! magic number.

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info};

use crate::logs::{fetch_logs_chunked, LogEntry};
use crate::types::{Address, DiscoveredPool, DiscoveryError};

/// Topic hash for V3 `PoolCreated`. Computed lazily on first access via OnceLock so the
/// canonical signature is the single source of truth.
pub fn pool_created_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut k = Keccak::v256();
        k.update(b"PoolCreated(address,address,uint24,int24,address)");
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        out
    })
}

/// Public alias — keeps the API ergonomic for callers that want the topic by name.
pub const V3_POOL_CREATED_TOPIC: fn() -> &'static [u8; 32] = pool_created_topic;

/// One V3 factory to scan.
#[derive(Debug, Clone)]
pub struct V3FactoryConfig {
    /// Factory contract address.
    pub address: Address,
    /// AMM family discriminant — `UniswapV3`, `SushiV3`, `PancakeV3`, etc.
    pub family: PoolKind,
    /// Earliest block to scan from. For Arbitrum One: Uniswap V3 ≈ 165, Sushi V3
    /// ≈ 75M, Pancake V3 ≈ 101M, Camelot Algebra ≈ 80M. The plan §5.4 calls for
    /// "factory event scan" — scanning from chain inception is correct but slow;
    /// callers should pass a tighter `deployment_block` when known.
    pub deployment_block: u64,
}

/// Scan a single V3 factory for every `PoolCreated` event between `deployment_block`
/// and `head_block`, decode each, drop pools whose `token0`/`token1` are outside
/// `token_allowlist`, and emit `DiscoveredPool` records.
///
/// `chunk_blocks` controls the per-`eth_getLogs` block range. Alchemy's free tier
/// caps at ~10 000 blocks per call when topic-filtered; the chunker halves on the
/// "range too large" signal so 50 000 here is a fine starting value.
pub fn discover_v3_pools(
    client: &ArbitrumRpcClient,
    factory: &V3FactoryConfig,
    head_block: u64,
    chunk_blocks: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    let topic0 = pool_created_topic();
    info!(
        factory = ?factory.address,
        from = factory.deployment_block,
        to = head_block,
        chunk = chunk_blocks,
        "scanning V3 PoolCreated logs"
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
    debug!(count = logs.len(), "raw V3 PoolCreated logs returned");
    let mut out = Vec::with_capacity(logs.len() / 4);
    for log in logs {
        match decode_v3_pool_created(&log) {
            Ok((token0, token1, fee, tick_spacing, pool, block)) => {
                if !token_allowlist.contains(&token0) || !token_allowlist.contains(&token1) {
                    continue;
                }
                out.push(DiscoveredPool {
                    address: pool,
                    family: factory.family,
                    token0,
                    token1,
                    fee_pips: fee,
                    fee_bps: 0,
                    tick_spacing,
                    created_block: block,
                });
            }
            Err(e) => {
                tracing::warn!(?e, ?log.address, ?log.block_number, "malformed V3 log; skipping");
            }
        }
    }
    info!(
        factory = ?factory.address,
        discovered = out.len(),
        "V3 PoolCreated discovery complete"
    );
    Ok(out)
}

/// Decode a single V3 `PoolCreated` log into `(token0, token1, fee, tick_spacing, pool, block)`.
fn decode_v3_pool_created(
    log: &LogEntry,
) -> Result<(Address, Address, u32, i32, Address, u64), DiscoveryError> {
    if log.topics.len() != 4 {
        return Err(DiscoveryError::MalformedLog("V3 PoolCreated topic count"));
    }
    // topics[1] = token0 (left-padded 32 bytes), topics[2] = token1, topics[3] = fee.
    let token0 = address_from_topic(&log.topics[1])?;
    let token1 = address_from_topic(&log.topics[2])?;
    let fee = u32_from_indexed_topic(&log.topics[3])?;
    // data = tickSpacing (32 bytes, signed) + pool (32 bytes, address left-padded).
    if log.data.len() < 64 {
        return Err(DiscoveryError::MalformedLog("V3 PoolCreated data len"));
    }
    let tick_spacing = i32_from_padded_word(&log.data[0..32])?;
    let pool = address_from_topic(&log.data[32..64].try_into().expect("32 bytes"))?;
    Ok((token0, token1, fee, tick_spacing, pool, log.block_number))
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

/// Extract a `u24` from an indexed-topic word. `uint24` is right-aligned, low 3 bytes.
/// We return it as `u32` for ergonomics; check the high byte is zero.
fn u32_from_indexed_topic(t: &[u8; 32]) -> Result<u32, DiscoveryError> {
    if t[0..29].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("u24 padding nonzero"));
    }
    Ok(u32::from_be_bytes([0, t[29], t[30], t[31]]))
}

/// Extract a signed `int24` from a 32-byte ABI word. `int24` is sign-extended to 256
/// bits in the canonical ABI encoding, so a positive value has zeros padding the top
/// 29 bytes and a negative has 0xFF padding.
fn i32_from_padded_word(w: &[u8]) -> Result<i32, DiscoveryError> {
    if w.len() != 32 {
        return Err(DiscoveryError::MalformedLog("int24 word len"));
    }
    let high = w[0];
    // Determine sign from the most-significant byte.
    let mut bytes = [0u8; 4];
    bytes[1] = w[29];
    bytes[2] = w[30];
    bytes[3] = w[31];
    if high == 0xFF {
        bytes[0] = 0xFF;
        // Validate the rest of the padding is also 0xFF.
        for &b in &w[1..29] {
            if b != 0xFF {
                return Err(DiscoveryError::MalformedLog("int24 padding mixed"));
            }
        }
        // And the high byte of the low 24 bits must have its sign bit set.
        if w[29] & 0x80 == 0 {
            return Err(DiscoveryError::MalformedLog(
                "int24 sign byte without high bit",
            ));
        }
    } else if high == 0 {
        for &b in &w[1..29] {
            if b != 0 {
                return Err(DiscoveryError::MalformedLog("int24 padding mixed"));
            }
        }
        // Positive value — the high byte of the low 24 bits must not have its sign bit.
        if w[29] & 0x80 != 0 {
            return Err(DiscoveryError::MalformedLog(
                "int24 positive but high bit set",
            ));
        }
    } else {
        return Err(DiscoveryError::MalformedLog("int24 padding not 00/FF"));
    }
    Ok(i32::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_matches_known_uniswap_v3_pool_created_hash() {
        // Reference: keccak256("PoolCreated(address,address,uint24,int24,address)") =
        //   0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118
        let computed = pool_created_topic();
        let mut got = [0u8; 64];
        faster_hex::hex_encode(computed, &mut got).unwrap();
        let got_str = std::str::from_utf8(&got).unwrap();
        assert_eq!(
            got_str,
            "783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
        );
    }

    #[test]
    fn address_extracts_low_20_bytes() {
        let mut t = [0u8; 32];
        t[12..32].copy_from_slice(&[0xDE; 20]);
        assert_eq!(address_from_topic(&t).unwrap(), [0xDE; 20]);
    }

    #[test]
    fn address_rejects_nonzero_padding() {
        let mut t = [0u8; 32];
        t[0] = 1;
        t[12..32].copy_from_slice(&[0xDE; 20]);
        assert!(address_from_topic(&t).is_err());
    }

    #[test]
    fn u24_indexed_topic_decodes() {
        let mut t = [0u8; 32];
        t[29] = 0x00;
        t[30] = 0x0B;
        t[31] = 0xB8; // 0x0BB8 = 3000 = 0.30% fee tier
        assert_eq!(u32_from_indexed_topic(&t).unwrap(), 3000);
    }

    #[test]
    fn int24_positive_word_decodes() {
        let mut w = [0u8; 32];
        w[29] = 0x00;
        w[30] = 0x00;
        w[31] = 60; // tickSpacing 60
        assert_eq!(i32_from_padded_word(&w).unwrap(), 60);
    }

    #[test]
    fn int24_negative_word_decodes() {
        let mut w = [0xFFu8; 32];
        w[29] = 0xFF;
        w[30] = 0xFF;
        w[31] = 0xC4; // -60 in two's-complement 24-bit = 0xFFFFC4
        assert_eq!(i32_from_padded_word(&w).unwrap(), -60);
    }
}
