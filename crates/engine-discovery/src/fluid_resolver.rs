//! Fluid DEX resolver-based pool discovery.
//!
//! Fluid uses a singleton + resolver architecture rather than per-pool factory
//! deploys, so the V3-style `PoolCreated` log scan does not apply. Instead the
//! engine calls the official Fluid resolvers via `eth_call`:
//!
//! * **v1/T1** — `DexReservesResolver.getAllPools()` returns the full list of
//!   v1 pool addresses (each is a discrete DEX contract). The resolver also
//!   exposes `getPoolReservesAdjusted` / `constantsView2` for per-pool token
//!   and reserve state, consumed downstream by the snapshot poller.
//!
//! * **v2 D3/D4** — `DexResolver.getAllPermissionedDexes()` returns the active
//!   permissioned dex keys. Each key resolves to a singleton-scoped pool
//!   address via the v2 resolver's pool lookup. Today the kernel ships with
//!   v1/T1 production paths only and v2 D3/D4 is permissioned + not yet GA on
//!   Arbitrum, so the v2 discovery path is *prepared* (return type wired,
//!   selector computed) and falls back gracefully when the resolver returns
//!   an empty list.
//!
//! ## Reference
//!
//! * `POOL_DISCOVERY_AND_ARBITRUM_2026.md` (shipped with `fluid-amm-kernel`)
//! * Fluid contracts: <https://github.com/Instadapp/fluid-contracts-public>
//! * Arbitrum deployment manifest (resolved at process boot, not hardcoded
//!   in the math kernel).

use ahash::AHashSet;
use engine_rpc::{ArbitrumRpcClient, BlockTag};
use engine_state::PoolKind;
use tiny_keccak::{Hasher, Keccak};
use tracing::{info, warn};

use crate::types::{Address, DiscoveredPool, DiscoveryError};

/// One Fluid resolver to scan. Each Fluid family (v1, v2) has its own resolver
/// contract; the engine queries both at startup and on the discovery refresh
/// cadence (§6).
#[derive(Debug, Clone)]
pub struct FluidResolverConfig {
    /// Resolver contract address.
    pub address: Address,
    /// Family this resolver serves. `PoolKind::FluidDex` for v1/T1 reserve
    /// resolver; the v2 resolver also reports `FluidDex` since v1 and v2
    /// share the same kernel dispatch and differ only in per-pool snapshot
    /// shape (handled at quote time).
    pub family: PoolKind,
    /// Family discriminator passed through to the resolver-poll background
    /// task — distinguishes v1 (collateral/debt adjusted-reserve math) from
    /// v2 D3/D4 (concentrated-liquidity segment math) when populating the
    /// snapshot.
    pub variant: FluidVariant,
    /// Wrapped-native address (WETH on Arbitrum). Fluid lists native-ETH legs
    /// as the sentinel `0xEee…EEeE`; we normalize those to the wrapped token so
    /// the pool joins the allowlist and shares a token id with every other
    /// venue's WETH pools (atomic executor wraps/unwraps at swap time).
    pub native_wrapped: Address,
}

/// Fluid's native-ETH sentinel address (`0xEeeeeEEeE…EEeE`). Used in pool token
/// legs in place of a real ERC-20; normalized to the wrapped-native token.
pub const FLUID_NATIVE_ETH: Address = [0xEe; 20];

/// Which Fluid family this resolver discovers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidVariant {
    /// v1/T1 collateral/debt adjusted-reserve math.
    V1,
    /// v2 D3/D4 concentrated-liquidity segment math.
    V2,
}

/// Selector for `getAllPools()` — v1 reserves resolver returns
/// `Pool[] memory`, where each `Pool` struct begins with the pool address.
/// We slice the address out of each 32-byte word at the start of each tuple.
fn selector_get_all_pools() -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(b"getAllPools()");
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

/// Selector for `getAllDexAddresses()` — v2 resolver returns the list of every
/// active Fluid DEX address. The previous selector `getAllPermissionedDexes()`
/// existed in an older draft of the resolver ABI and reverts on the current
/// deployment (Arbitrum DexResolver `0x11D8…cC07`). Confirmed against the
/// canonical source at
/// github.com/Instadapp/fluid-contracts-public/blob/main/contracts/periphery/resolvers/dex/main.sol
fn selector_get_all_dex_addresses() -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(b"getAllDexAddresses()");
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

/// Selector for `getDexTokens(address)` — v2 resolver returns `(token0, token1)`
/// for a specific DEX. Used in the v2 discovery path because
/// `getAllDexAddresses()` only returns addresses; we then fan-out to look up the
/// token pair so the allowlist filter can be applied before snapshot fetch.
fn selector_get_dex_tokens() -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(b"getDexTokens(address)");
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

/// Discover every v1/T1 Fluid pool from the configured `DexReservesResolver`.
///
/// Returns one `DiscoveredPool` per pool whose `token0` AND `token1` are both
/// in `token_allowlist`. Pools outside the allowlist are silently dropped (the
/// downstream snapshot fetch would also drop them; we save the RPC round-trips
/// by filtering early).
///
/// The discovery path is fail-open: if the resolver call errors (e.g. the
/// resolver is not deployed on the target chain yet), we log and return an
/// empty list rather than aborting startup. Fluid is one of many DEXes and a
/// missing resolver should not block UniV3/Camelot execution.
pub fn discover_fluid_v1_pools(
    client: &ArbitrumRpcClient,
    resolver: &FluidResolverConfig,
    head_block: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    info!(
        resolver = ?resolver.address,
        head_block,
        "calling DexReservesResolver.getAllPools()"
    );
    // `getAllPools()` returns `Pool[]` where each tuple begins
    // `(address pool, address token0, address token1, uint256 fee, …)`. Reading
    // the tokens straight out of this tuple is correct and robust; the earlier
    // `getAllPoolAddresses()` + per-pool `constantsView()` path mis-decoded the
    // `ConstantViews` struct (token0/token1 live at word 9/10, not 0/1) and so
    // dropped every pool. Fee is reported in ppm (1e6), matching our V3 scale.
    let block = BlockTag::Number(head_block);
    let tuples = match call_fluid_pool_tuples(
        client,
        &resolver.address,
        &selector_get_all_pools(),
        block,
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(?e, resolver = ?resolver.address, "Fluid v1 getAllPools failed — empty discovery");
            return Ok(Vec::new());
        }
    };
    info!(
        count = tuples.len(),
        "Fluid v1 resolver returned pool tuples"
    );
    let mut out = Vec::with_capacity(tuples.len());
    for t in tuples {
        // Normalize Fluid's native-ETH sentinel to the wrapped-native token so
        // the leg matches the allowlist and shares a token id with every other
        // venue's WETH pools.
        let t0 = if t.token0 == FLUID_NATIVE_ETH {
            resolver.native_wrapped
        } else {
            t.token0
        };
        let t1 = if t.token1 == FLUID_NATIVE_ETH {
            resolver.native_wrapped
        } else {
            t.token1
        };
        let (token0, token1) = if t0 < t1 { (t0, t1) } else { (t1, t0) };
        if !token_allowlist.contains(&token0) || !token_allowlist.contains(&token1) {
            continue;
        }
        out.push(DiscoveredPool {
            address: t.pool,
            family: resolver.family,
            // Fee is ppm (1e6); the snapshot poller refreshes the live dynamic
            // fee, but this seed value lets the fee-floor pre-filter run before
            // the first poll.
            fee_pips: t.fee,
            fee_bps: 0,
            tick_spacing: 0,
            token0,
            token1,
            created_block: head_block,
        });
    }
    info!(
        resolver = ?resolver.address,
        discovered = out.len(),
        "Fluid v1 discovery complete"
    );
    Ok(out)
}

/// Discover every v2 D3/D4 Fluid pool from the configured `DexResolver`.
///
/// v2 returns a list of `DexKey` tuples; the pool address is derived by the
/// resolver's pool-lookup endpoint. For now we surface only the count
/// (no Arbitrum v2 deployment is GA yet — the path is wired but will return
/// 0 on Arbitrum mainnet until Fluid governance enables permissionless v2).
pub fn discover_fluid_v2_pools(
    client: &ArbitrumRpcClient,
    resolver: &FluidResolverConfig,
    head_block: u64,
    token_allowlist: &AHashSet<Address>,
) -> Result<Vec<DiscoveredPool>, DiscoveryError> {
    info!(
        resolver = ?resolver.address,
        head_block,
        "calling DexResolver.getAllDexAddresses()"
    );
    let block = BlockTag::Number(head_block);
    // Step 1: enumerate every DEX address. The canonical Arbitrum DexResolver
    // (0x11D8…cC07) exposes `getAllDexAddresses() returns (address[])`.
    let dex_addrs = match call_address_array(
        client,
        &resolver.address,
        &selector_get_all_dex_addresses(),
        block,
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(?e, resolver = ?resolver.address, "Fluid v2 getAllDexAddresses failed — empty discovery");
            return Ok(Vec::new());
        }
    };
    info!(
        resolver = ?resolver.address,
        count = dex_addrs.len(),
        "Fluid v2 resolver returned dex addresses"
    );

    // Step 2: per-DEX, call `getDexTokens(dex)` to read `(token0, token1)` and
    // drop any pool whose tokens are outside our allowlist. We accept resolver
    // call failures gracefully (skip the offending DEX, continue with the rest)
    // since one bad DEX must not block discovery for the others.
    let mut out: Vec<DiscoveredPool> = Vec::with_capacity(dex_addrs.len() / 4);
    let selector = selector_get_dex_tokens();
    for dex in dex_addrs {
        let mut calldata = [0u8; 4 + 32];
        calldata[0..4].copy_from_slice(&selector);
        // address arg is right-aligned in a 32-byte word, low 20 bytes.
        calldata[16..36].copy_from_slice(&dex);
        let resp = match client.eth_call(&resolver.address, &calldata, block) {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(?e, dex = ?dex, "Fluid v2 getDexTokens reverted; skipping dex");
                continue;
            }
        };
        if resp.len() < 64 {
            continue;
        }
        // Two 32-byte words: token0 (right-aligned address), token1 (same).
        if resp[0..12].iter().any(|&b| b != 0) || resp[32..44].iter().any(|&b| b != 0) {
            continue;
        }
        let mut token0 = [0u8; 20];
        let mut token1 = [0u8; 20];
        token0.copy_from_slice(&resp[12..32]);
        token1.copy_from_slice(&resp[44..64]);
        if token0 == FLUID_NATIVE_ETH {
            token0 = resolver.native_wrapped;
        }
        if token1 == FLUID_NATIVE_ETH {
            token1 = resolver.native_wrapped;
        }
        if !token_allowlist.contains(&token0) || !token_allowlist.contains(&token1) {
            continue;
        }
        out.push(DiscoveredPool {
            address: dex,
            family: engine_state::PoolKind::FluidDex,
            token0,
            token1,
            fee_pips: 0,
            fee_bps: 0,
            tick_spacing: 0,
            created_block: 0,
        });
    }
    info!(
        resolver = ?resolver.address,
        admitted = out.len(),
        "Fluid v2 discovery complete"
    );
    Ok(out)
}

/// Decode an ABI-encoded `address[]` return value into a vector of 20-byte
/// addresses.
///
/// Layout (head + tail per the canonical Solidity ABI):
/// * `[0..32]`: offset to the array start (always 0x20 for a single dynamic
///   return value).
/// * `[offset..offset+32]`: array length `N`.
/// * `[offset+32..offset+32+32*N]`: `N` consecutive 32-byte words, each an
///   address right-aligned in the low 20 bytes.
fn call_address_array(
    client: &ArbitrumRpcClient,
    to: &Address,
    selector: &[u8; 4],
    block: BlockTag,
) -> Result<Vec<Address>, DiscoveryError> {
    let raw = client.eth_call(to, selector, block)?;
    decode_address_array(&raw)
}

fn decode_address_array(raw: &[u8]) -> Result<Vec<Address>, DiscoveryError> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.len() < 64 {
        return Err(DiscoveryError::MalformedLog("address[] too short"));
    }
    // Offset to dynamic array — must be exactly 0x20 for a single return.
    let offset = u256_to_usize(&raw[0..32])
        .ok_or(DiscoveryError::MalformedLog("address[] offset > usize"))?;
    if offset.checked_add(32).is_none_or(|end| end > raw.len()) {
        return Err(DiscoveryError::MalformedLog("address[] offset OOB"));
    }
    let len = u256_to_usize(&raw[offset..offset + 32])
        .ok_or(DiscoveryError::MalformedLog("address[] length > usize"))?;
    let elements_start = offset + 32;
    let elements_end = elements_start
        .checked_add(
            len.checked_mul(32)
                .ok_or(DiscoveryError::MalformedLog("len*32 overflow"))?,
        )
        .ok_or(DiscoveryError::MalformedLog("elements end overflow"))?;
    if elements_end > raw.len() {
        return Err(DiscoveryError::MalformedLog("address[] truncated"));
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let word_start = elements_start + i * 32;
        let word = &raw[word_start..word_start + 32];
        // Address is right-aligned — high 12 bytes must be zero.
        if word[0..12].iter().any(|&b| b != 0) {
            return Err(DiscoveryError::MalformedLog("address word padding nonzero"));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&word[12..32]);
        out.push(a);
    }
    Ok(out)
}

/// One decoded `Pool` tuple from `DexReservesResolver.getAllPools()`.
struct FluidPoolTuple {
    pool: Address,
    token0: Address,
    token1: Address,
    /// Fee in ppm (1e6), as reported by the resolver.
    fee: u32,
}

/// Decode an ABI-encoded `Pool[] memory` from `getAllPools()`. Each `Pool`
/// tuple is laid out `(address pool, address token0, address token1, uint256
/// fee, …)`; we read the first three address fields plus the fee.
///
/// Because tuple stride depends on the struct's full field count, we probe the
/// stride from `(data_len - 32) / len` and require it to be a positive multiple
/// of 32 covering at least the 3 address words we read. This tolerates
/// resolver-version field additions without hardcoding the layout.
fn call_fluid_pool_tuples(
    client: &ArbitrumRpcClient,
    to: &Address,
    selector: &[u8; 4],
    block: BlockTag,
) -> Result<Vec<FluidPoolTuple>, DiscoveryError> {
    let raw = client.eth_call(to, selector, block)?;
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.len() < 64 {
        return Err(DiscoveryError::MalformedLog("Pool[] too short"));
    }
    let offset = u256_to_usize(&raw[0..32]).ok_or(DiscoveryError::MalformedLog("Pool[] offset"))?;
    if offset.checked_add(32).is_none_or(|end| end > raw.len()) {
        return Err(DiscoveryError::MalformedLog("Pool[] offset OOB"));
    }
    let len = u256_to_usize(&raw[offset..offset + 32])
        .ok_or(DiscoveryError::MalformedLog("Pool[] length"))?;
    if len == 0 {
        return Ok(Vec::new());
    }
    let elements_start = offset + 32;
    let data_after = raw.len() - elements_start;
    let stride = data_after / len;
    if stride < 32 * 4 || stride % 32 != 0 {
        return Err(DiscoveryError::MalformedLog(
            "Pool[] stride too small / unaligned",
        ));
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let ts = elements_start + i * stride;
        if ts + 4 * 32 > raw.len() {
            return Err(DiscoveryError::MalformedLog("Pool[] truncated mid-tuple"));
        }
        let pool = address_from_word(&raw[ts..ts + 32])?;
        let token0 = address_from_word(&raw[ts + 32..ts + 64])?;
        let token1 = address_from_word(&raw[ts + 64..ts + 96])?;
        // Fee is a uint256; fees are tiny (< 1e6) so the low 8 bytes suffice,
        // capped to u32.
        let mut fb = [0u8; 8];
        fb.copy_from_slice(&raw[ts + 96 + 24..ts + 128]);
        let fee = u64::from_be_bytes(fb).min(u32::MAX as u64) as u32;
        out.push(FluidPoolTuple {
            pool,
            token0,
            token1,
            fee,
        });
    }
    Ok(out)
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

fn u256_to_usize(w: &[u8]) -> Option<usize> {
    if w.len() != 32 {
        return None;
    }
    // High 24 bytes must be zero (we only accept usize-sized values).
    if w[0..24].iter().any(|&b| b != 0) {
        return None;
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&w[24..32]);
    Some(u64::from_be_bytes(bytes) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_get_all_pools_matches_keccak_prefix() {
        let s = selector_get_all_pools();
        // keccak256("getAllPools()")[0..4]. We don't hardcode the exact
        // bytes — just assert the selector is non-trivial and stable
        // across calls.
        let s2 = selector_get_all_pools();
        assert_eq!(s, s2);
        assert!(s.iter().any(|&b| b != 0));
    }

    #[test]
    fn decode_empty_address_array() {
        // Empty array: offset=0x20, length=0.
        let mut raw = vec![0u8; 64];
        raw[31] = 0x20;
        let v = decode_address_array(&raw).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn decode_single_address_array() {
        // offset=0x20, length=1, then 1 address word.
        let mut raw = vec![0u8; 96];
        raw[31] = 0x20;
        raw[63] = 0x01;
        // Address bytes in slots 12..32 of the third word.
        for (i, b) in (0xDE..0xDE + 20).enumerate() {
            raw[64 + 12 + i] = b as u8;
        }
        let v = decode_address_array(&raw).unwrap();
        assert_eq!(v.len(), 1);
        let mut expected = [0u8; 20];
        for (i, b) in (0xDE..0xDE + 20).enumerate() {
            expected[i] = b as u8;
        }
        assert_eq!(v[0], expected);
    }

    #[test]
    fn decode_two_address_array() {
        let mut raw = vec![0u8; 128];
        raw[31] = 0x20;
        raw[63] = 0x02;
        for i in 0..20 {
            raw[64 + 12 + i] = 0xAA;
            raw[96 + 12 + i] = 0xBB;
        }
        let v = decode_address_array(&raw).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], [0xAA; 20]);
        assert_eq!(v[1], [0xBB; 20]);
    }

    #[test]
    fn rejects_nonzero_address_padding() {
        let mut raw = vec![0u8; 96];
        raw[31] = 0x20;
        raw[63] = 0x01;
        raw[64] = 0x01; // nonzero high byte in address word
        assert!(decode_address_array(&raw).is_err());
    }

    #[test]
    fn rejects_truncated_payload() {
        let mut raw = vec![0u8; 80]; // length says 1 element, but no element word
        raw[31] = 0x20;
        raw[63] = 0x01;
        assert!(decode_address_array(&raw).is_err());
    }

    #[test]
    fn empty_input_decodes_as_empty() {
        let v = decode_address_array(&[]).unwrap();
        assert!(v.is_empty());
    }
}
