//! V2 reserves snapshot via `getReserves()`.
//!
//! Selector: `keccak256("getReserves()")[..4] == 0x0902f1ac`.
//! Returns `(uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)` packed into a
//! 96-byte ABI tuple (3 × 32).

use engine_feed::Address;

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

const GET_RESERVES_SELECTOR: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];

/// `token0()` / `token1()` selectors (V2 + V3 share the same ABI).
const TOKEN0_SELECTOR: [u8; 4] = [0x0d, 0xfe, 0x16, 0x81];
const TOKEN1_SELECTOR: [u8; 4] = [0xd2, 0x12, 0x20, 0xa7];

/// `stableSwap()` selector — Camelot V2 pairs expose this bool. `keccak256("stableSwap()")[..4]`
/// (verified by `stableswap_selector_matches_keccak`). Plain Uniswap/Sushi pairs don't have it
/// (the call reverts), so the reader below treats any decode/RPC error as "not stable".
const STABLE_SWAP_SELECTOR: [u8; 4] = [0x9e, 0x54, 0x8b, 0x7f];

/// Read Camelot V2's `stableSwap()` flag. `Some(true)` for a solidly-style stable pair,
/// `Some(false)` for a constant-product (volatile) pair, `None` if the pool doesn't expose the
/// selector / the call fails (a plain Uniswap-V2-style pair, or an RPC error). Callers treat `None`
/// as "not stable" — fail-safe, since the constant-product kernel is correct for those.
pub fn fetch_camelot_is_stable(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Option<bool> {
    let resp = client.eth_call(pool, &STABLE_SWAP_SELECTOR, block).ok()?;
    if resp.len() < 32 {
        return None;
    }
    // ABI bool: low byte of the 32-byte word is 1 for true, 0 for false.
    Some(resp[31] != 0)
}

/// Read `token0()` from a Uniswap-style pool.
pub fn fetch_token0(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<Address, RpcError> {
    let resp = client.eth_call(pool, &TOKEN0_SELECTOR, block)?;
    if resp.len() < 32 {
        return Err(RpcError::Decode("token0 response too short".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Ok(a)
}

/// Read `token1()` from a Uniswap-style pool.
pub fn fetch_token1(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<Address, RpcError> {
    let resp = client.eth_call(pool, &TOKEN1_SELECTOR, block)?;
    if resp.len() < 32 {
        return Err(RpcError::Decode("token1 response too short".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Ok(a)
}

/// V2 reserves snapshot.
#[derive(Debug, Clone, Copy)]
pub struct V2Snapshot {
    /// `reserve0`.
    pub reserve0: u128,
    /// `reserve1`.
    pub reserve1: u128,
    /// `blockTimestampLast`.
    pub block_timestamp_last: u32,
}

/// Fetch reserves for a V2-style pair pool at `block`.
pub fn fetch_v2_reserves(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<V2Snapshot, RpcError> {
    let resp = client.eth_call(pool, &GET_RESERVES_SELECTOR, block)?;
    if resp.len() < 96 {
        return Err(RpcError::Decode("v2 reserves response too short".into()));
    }
    // reserve0 is at bytes [16..32] (uint112 right-aligned in 32-byte word).
    let mut buf0 = [0u8; 16];
    buf0.copy_from_slice(&resp[16..32]);
    let reserve0 = u128::from_be_bytes(buf0);
    let mut buf1 = [0u8; 16];
    buf1.copy_from_slice(&resp[48..64]);
    let reserve1 = u128::from_be_bytes(buf1);
    let mut buf_ts = [0u8; 4];
    buf_ts.copy_from_slice(&resp[92..96]);
    let block_timestamp_last = u32::from_be_bytes(buf_ts);
    Ok(V2Snapshot {
        reserve0,
        reserve1,
        block_timestamp_last,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_matches_keccak() {
        // keccak256("getReserves()")[..4] = 0x0902f1ac
        assert_eq!(GET_RESERVES_SELECTOR, [0x09, 0x02, 0xf1, 0xac]);
    }

    #[test]
    fn stableswap_selector_matches_keccak() {
        use tiny_keccak::{Hasher, Keccak};
        let mut k = Keccak::v256();
        let mut o = [0u8; 32];
        k.update(b"stableSwap()");
        k.finalize(&mut o);
        assert_eq!(STABLE_SWAP_SELECTOR, [o[0], o[1], o[2], o[3]]);
    }
}
