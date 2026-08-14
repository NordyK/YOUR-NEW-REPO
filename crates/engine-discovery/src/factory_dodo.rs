//! DODO V2 factory event-log discovery (NewDVM / NewDPP / NewDSP).
//!
//! Three sibling factories (DVMFactory, DPPFactory, DSPFactory) each emit a
//! `New{X}(address baseToken, address quoteToken, address creator, address dvm)`
//! event when a pool is deployed. We scan all configured factories and emit a
//! `DiscoveredPool { family: PoolKind::DodoV2, … }` per allowlisted pair.

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info};

use crate::logs::{fetch_logs_chunked, LogEntry};
use crate::types::{Address, DiscoveredPool, DiscoveryError};

fn topic_for(sig: &[u8]) -> [u8; 32] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

/// `keccak256("NewDVM(address,address,address,address)")`.
pub fn new_dvm_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| topic_for(b"NewDVM(address,address,address,address)"))
}

/// `keccak256("NewDPP(address,address,address,address)")`.
pub fn new_dpp_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| topic_for(b"NewDPP(address,address,address,address)"))
}

/// `keccak256("NewDSP(address,address,address,address)")`.
pub fn new_dsp_topic() -> &'static [u8; 32] {
    static CELL: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    CELL.get_or_init(|| topic_for(b"NewDSP(address,address,address,address)"))
}

/// One DODO factory to scan. Each variant emits a different topic-0; the caller
/// passes the matching topic so we don't have to coalesce events.
#[derive(Debug, Clone)]
pub struct DodoFactoryConfig {
    /// Factory contract address.
    pub address: Address,
    /// Earliest block to scan from.
    pub deployment_block: u64,
    /// Event topic-0 — one of `new_dvm_topic()` / `new_dpp_topic()` / `new_dsp_topic()`.
    pub event_topic: [u8; 32],
}

/// Scan a single DODO factory for pool-creation events.
///
/// DODO `New{Type}` events have all four args **non-indexed** (in the data
/// payload), so topics has length 1 (just topic-0). Data layout: token0,
/// token1, creator, pool — each padded to 32 bytes.
pub fn discover_dodo_pools(
    client: &ArbitrumRpcClient,
    factory: &DodoFactoryConfig,
    head_block: u64,
    chunk_blocks: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    info!(
        factory = ?factory.address,
        from = factory.deployment_block,
        to = head_block,
        chunk = chunk_blocks,
        "scanning DODO V2 factory logs"
    );
    let logs = fetch_logs_chunked(
        client,
        &factory.address,
        &factory.event_topic,
        factory.deployment_block,
        head_block,
        chunk_blocks,
        /* max_retries */ 4,
    )?;
    debug!(count = logs.len(), "raw DODO factory logs returned");
    let mut out = Vec::with_capacity(logs.len() / 4);
    for log in logs {
        match decode_dodo_pool(&log) {
            Ok((token0, token1, pool, block)) => {
                if !token_allowlist.contains(&token0) || !token_allowlist.contains(&token1) {
                    continue;
                }
                out.push(DiscoveredPool {
                    address: pool,
                    family: PoolKind::DodoV2,
                    token0,
                    token1,
                    fee_pips: 0,
                    fee_bps: 0,
                    tick_spacing: 0,
                    created_block: block,
                });
            }
            Err(e) => {
                tracing::warn!(?e, ?log.address, ?log.block_number, "malformed DODO log; skipping");
            }
        }
    }
    info!(
        factory = ?factory.address,
        discovered = out.len(),
        "DODO V2 discovery complete"
    );
    Ok(out)
}

fn decode_dodo_pool(log: &LogEntry) -> Result<(Address, Address, Address, u64), DiscoveryError> {
    if log.topics.len() != 1 {
        return Err(DiscoveryError::MalformedLog("DODO log topic count != 1"));
    }
    if log.data.len() < 4 * 32 {
        return Err(DiscoveryError::MalformedLog("DODO log data length < 128"));
    }
    let token0 = address_from_word(&log.data[0..32])?;
    let token1 = address_from_word(&log.data[32..64])?;
    // word[2] = creator — unused
    let pool = address_from_word(&log.data[96..128])?;
    Ok((token0, token1, pool, log.block_number))
}

fn address_from_word(w: &[u8]) -> Result<Address, DiscoveryError> {
    if w.len() != 32 {
        return Err(DiscoveryError::MalformedLog("address word len"));
    }
    if w[0..12].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("address padding nonzero"));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&w[12..32]);
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dodo_topic_hashes_are_distinct() {
        assert_ne!(new_dvm_topic(), new_dpp_topic());
        assert_ne!(new_dpp_topic(), new_dsp_topic());
        assert_ne!(new_dvm_topic(), new_dsp_topic());
    }

    #[test]
    fn decode_well_formed_log() {
        let mut data = vec![0u8; 128];
        data[12..32].copy_from_slice(&[0xAA; 20]);
        data[44..64].copy_from_slice(&[0xBB; 20]);
        data[76..96].copy_from_slice(&[0xCC; 20]); // creator
        data[108..128].copy_from_slice(&[0xDD; 20]); // pool
        let log = LogEntry {
            address: [0u8; 20],
            topics: vec![*new_dvm_topic()],
            data,
            block_number: 100,
        };
        let (t0, t1, pool, blk) = decode_dodo_pool(&log).unwrap();
        assert_eq!(t0, [0xAA; 20]);
        assert_eq!(t1, [0xBB; 20]);
        assert_eq!(pool, [0xDD; 20]);
        assert_eq!(blk, 100);
    }
}
