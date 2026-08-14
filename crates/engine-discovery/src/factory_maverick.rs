//! Maverick V2 factory discovery via `poolCount() + lookup(uint, uint)`.
//!
//! Unlike V2/V3 factories that emit creation events, Maverick exposes a
//! paginated `lookup(startIndex, endIndex)` that returns the full pool roster.
//! We page in batches of ~100 and filter by allowlist using a follow-up
//! `tokenA()` / `tokenB()` per pool.

use ahash::AHashSet;
use engine_rpc::ArbitrumRpcClient;
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{debug, info, warn};

use crate::types::{Address, DiscoveredPool, DiscoveryError};

use engine_rpc::BlockTag;

/// `poolCount() returns (uint256)`.
fn pool_count_selector() -> [u8; 4] {
    selector(b"poolCount()")
}

/// `lookup(uint256,uint256) returns (address[])` — paginated pool enumeration.
fn lookup_range_selector() -> [u8; 4] {
    selector(b"lookup(uint256,uint256)")
}

/// `tokenA() returns (address)`.
fn token_a_selector() -> [u8; 4] {
    selector(b"tokenA()")
}

/// `tokenB() returns (address)`.
fn token_b_selector() -> [u8; 4] {
    selector(b"tokenB()")
}

fn selector(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

/// One Maverick V2 factory to scan.
#[derive(Debug, Clone)]
pub struct MaverickFactoryConfig {
    /// Factory contract address.
    pub address: Address,
}

/// Discover every Maverick V2 pool whose `(tokenA, tokenB)` pair is in the
/// allowlist. Paginated via `lookup(startIndex, endIndex)`.
pub fn discover_maverick_pools(
    client: &ArbitrumRpcClient,
    factory: &MaverickFactoryConfig,
    head_block: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    let block = BlockTag::Number(head_block);
    info!(factory = ?factory.address, "querying Maverick V2 poolCount()");

    // Step 1: total pool count.
    let count_resp = client
        .eth_call(&factory.address, &pool_count_selector(), block)
        .map_err(|e| {
            warn!(?e, "Maverick poolCount call failed");
            DiscoveryError::MalformedLog("Maverick poolCount failed")
        })?;
    if count_resp.len() < 32 {
        return Err(DiscoveryError::MalformedLog("Maverick poolCount short"));
    }
    let raw_total = u256_low_u64(&count_resp[0..32]) as usize;
    // AUD-DISCOVERY-CAP: clamp an RPC-supplied count so a hostile/buggy `poolCount()` cannot drive
    // `Vec::with_capacity` into an OOM abort (or spin the paging loop unbounded).
    let total = crate::capped_pool_count(raw_total);
    if total != raw_total {
        warn!(factory = ?factory.address, raw_total, total, "Maverick poolCount clamped to cap");
    }
    info!(factory = ?factory.address, total, "Maverick V2 total pool count");
    if total == 0 {
        return Ok(Vec::new());
    }

    // Step 2: page through lookup(uint256, uint256).
    const PAGE_SIZE: usize = 100;
    let mut all_pools: Vec<Address> = Vec::with_capacity(total);
    let lookup_sel = lookup_range_selector();
    let mut start = 0usize;
    while start < total {
        let end = (start + PAGE_SIZE).min(total);
        let mut calldata = Vec::with_capacity(4 + 64);
        calldata.extend_from_slice(&lookup_sel);
        let mut start_word = [0u8; 32];
        start_word[24..32].copy_from_slice(&(start as u64).to_be_bytes());
        let mut end_word = [0u8; 32];
        end_word[24..32].copy_from_slice(&(end as u64).to_be_bytes());
        calldata.extend_from_slice(&start_word);
        calldata.extend_from_slice(&end_word);
        let resp = match client.eth_call(&factory.address, &calldata, block) {
            Ok(b) => b,
            Err(e) => {
                warn!(?e, start, end, "Maverick lookup page failed; stopping");
                break;
            }
        };
        let page = decode_address_array(&resp).map_err(|e| {
            warn!(?e, "Maverick lookup decode failed");
            e
        })?;
        debug!(start, end, returned = page.len(), "Maverick lookup page");
        all_pools.extend(page);
        start = end;
    }
    info!(factory = ?factory.address, fetched = all_pools.len(), "Maverick V2 pool roster fetched");

    // Step 3: per-pool token lookup + allowlist filter.
    let token_a = token_a_selector();
    let token_b = token_b_selector();
    let mut out = Vec::with_capacity(all_pools.len() / 4);
    for pool in all_pools {
        let ta = match client.eth_call(&pool, &token_a, block) {
            Ok(b) => match decode_address_from_word(&b) {
                Ok(a) => a,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        let tb = match client.eth_call(&pool, &token_b, block) {
            Ok(b) => match decode_address_from_word(&b) {
                Ok(a) => a,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        if !token_allowlist.contains(&ta) || !token_allowlist.contains(&tb) {
            continue;
        }
        out.push(DiscoveredPool {
            address: pool,
            family: PoolKind::MaverickV2,
            token0: ta,
            token1: tb,
            fee_pips: 0,
            fee_bps: 0,
            tick_spacing: 0,
            created_block: 0,
        });
    }
    info!(
        factory = ?factory.address,
        discovered = out.len(),
        "Maverick V2 discovery complete (allowlist-filtered)"
    );
    Ok(out)
}

fn u256_low_u64(w: &[u8]) -> u64 {
    if w.len() < 32 {
        return 0;
    }
    u64::from_be_bytes(w[24..32].try_into().expect("8 bytes"))
}

fn decode_address_from_word(resp: &[u8]) -> Result<Address, DiscoveryError> {
    if resp.len() < 32 {
        return Err(DiscoveryError::MalformedLog("address word short"));
    }
    if resp[0..12].iter().any(|&b| b != 0) {
        return Err(DiscoveryError::MalformedLog("address padding nonzero"));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Ok(a)
}

fn decode_address_array(raw: &[u8]) -> Result<Vec<Address>, DiscoveryError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.len() < 64 {
        return Err(DiscoveryError::MalformedLog("address[] too short"));
    }
    // First word is offset to dynamic array (always 0x20 for single dynamic return).
    let offset =
        u256_to_usize(&raw[0..32]).ok_or(DiscoveryError::MalformedLog("address[] offset"))?;
    if offset.checked_add(32).is_none_or(|end| end > raw.len()) {
        return Err(DiscoveryError::MalformedLog("address[] offset OOB"));
    }
    let len = u256_to_usize(&raw[offset..offset + 32])
        .ok_or(DiscoveryError::MalformedLog("address[] length"))?;
    let elements_start = offset + 32;
    let elements_end = elements_start
        .checked_add(
            len.checked_mul(32)
                .ok_or(DiscoveryError::MalformedLog("len*32"))?,
        )
        .ok_or(DiscoveryError::MalformedLog("elements end"))?;
    if elements_end > raw.len() {
        return Err(DiscoveryError::MalformedLog("address[] truncated"));
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let w_start = elements_start + i * 32;
        let w = &raw[w_start..w_start + 32];
        if w[0..12].iter().any(|&b| b != 0) {
            return Err(DiscoveryError::MalformedLog("address word padding"));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&w[12..32]);
        out.push(a);
    }
    Ok(out)
}

fn u256_to_usize(w: &[u8]) -> Option<usize> {
    if w.len() < 32 {
        return None;
    }
    if w[..24].iter().any(|&b| b != 0) {
        return None;
    }
    Some(u64::from_be_bytes(w[24..32].try_into().expect("8")) as usize)
}
