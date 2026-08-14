//! Multicall3 batched `eth_call` helper.
//!
//! [Multicall3](https://github.com/mds1/multicall) is deployed at the canonical
//! address `0xcA11bde05977b3631167028862bE2a173976CA11` on every major EVM
//! chain, including Arbitrum One. It exposes `aggregate3(Call3[])` which lets
//! us issue N read-only `eth_call`s through a single JSON-RPC round-trip.
//!
//! Used during the startup snapshot phase to collapse the per-pool RPC fanout
//! (e.g. 21 `ticks(int24)` calls per V3 pool) into one network roundtrip — the
//! dominant chunk of the 9-minute startup latency without this helper.
//!
//! ABI:
//!
//! ```text
//! struct Call3 { address target; bool allowFailure; bytes callData; }
//! struct Result { bool success; bytes returnData; }
//! function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory)
//! ```
//!
//! Selector = `keccak256("aggregate3((address,bool,bytes)[])")[0..4]` = `0x82ad56cb`.

use alloy_primitives::U256;
use engine_feed::Address;

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

/// Atomically-fetched price state for one pool (V2 reserves or V3 slot0+liquidity).
#[derive(Debug, Clone)]
pub enum PoolStateUpdate {
    /// V2-family reserves.
    V2 {
        /// Token0 reserve.
        reserve0: u128,
        /// Token1 reserve.
        reserve1: u128,
        /// V2 oracle timestamp.
        block_timestamp_last: u32,
    },
    /// V3-family price + active liquidity.
    V3 {
        /// `sqrt(price) * 2^96`.
        sqrt_price_x96: U256,
        /// Current tick.
        tick: i32,
        /// Active liquidity.
        liquidity: u128,
    },
}

/// How to read a pool's price state in the batched resync. V3-family pools split
/// by which "current state" view they expose: Uniswap-style `slot0()` vs Algebra
/// V1.9 (Camelot V3) `globalState()`. Sending `slot0()` to an Algebra pool reverts,
/// so the pool would silently never refresh — leaving it on stale state while its
/// cross-venue counterparts update, manufacturing phantom dislocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceFetchKind {
    /// V2-style `getReserves()`.
    V2,
    /// Uniswap-V3-style `slot0()` + `liquidity()`.
    UniswapV3,
    /// Algebra-V1.9 (Camelot V3) `globalState()` + `liquidity()`.
    Algebra,
}

/// Fetch price state for many pools in ONE `aggregate3` (a single `eth_call` at a
/// single block). Each V3 pool issues `slot0()` + `liquidity()` (2 calls); each V2
/// pool issues `getReserves()` (1 call). Returns one `Option` per input pool
/// (`None` if its call(s) failed or decoded short).
///
/// The whole batch resolves at ONE block, so the returned states are mutually
/// consistent — eliminating the cross-time skew that a pool-at-a-time refresh
/// produces (which makes a cross-venue cycle "profit" from price drift between
/// snapshots taken seconds apart rather than from a real simultaneous
/// dislocation).
pub fn fetch_pool_states_batched(
    client: &ArbitrumRpcClient,
    multicall3: &Address,
    pools: &[(Address, PriceFetchKind)],
    block: BlockTag,
) -> Result<Vec<Option<PoolStateUpdate>>, RpcError> {
    const SLOT0: [u8; 4] = [0x38, 0x50, 0xc7, 0xbd];
    const GLOBAL_STATE: [u8; 4] = [0xe7, 0x6c, 0x01, 0xe4];
    const LIQUIDITY: [u8; 4] = [0x1a, 0x68, 0x65, 0x02];
    const GET_RESERVES: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];
    let mut calls: Vec<Call3> = Vec::with_capacity(pools.len() * 2);
    let mut plan: Vec<(usize, bool)> = Vec::with_capacity(pools.len());
    for (addr, kind) in pools {
        let start = calls.len();
        let is_v3 = match kind {
            PriceFetchKind::V2 => {
                calls.push(Call3 {
                    target: *addr,
                    allow_failure: true,
                    call_data: GET_RESERVES.to_vec(),
                });
                false
            }
            PriceFetchKind::UniswapV3 | PriceFetchKind::Algebra => {
                // globalState() and slot0() share the same leading layout (word0 =
                // sqrtPriceX96/price, word1 = int24 tick), so the decode below is
                // identical; only the selector differs.
                let price_sel = if *kind == PriceFetchKind::Algebra {
                    GLOBAL_STATE
                } else {
                    SLOT0
                };
                calls.push(Call3 {
                    target: *addr,
                    allow_failure: true,
                    call_data: price_sel.to_vec(),
                });
                calls.push(Call3 {
                    target: *addr,
                    allow_failure: true,
                    call_data: LIQUIDITY.to_vec(),
                });
                true
            }
        };
        plan.push((start, is_v3));
    }
    let results = aggregate3(client, multicall3, &calls, block)?;
    let mut out: Vec<Option<PoolStateUpdate>> = Vec::with_capacity(pools.len());
    for (start, is_v3) in plan {
        let upd = if is_v3 {
            match (results.get(start), results.get(start + 1)) {
                (Some(s0), Some(liq))
                    if s0.success
                        && s0.return_data.len() >= 64
                        && liq.success
                        && liq.return_data.len() >= 32 =>
                {
                    let rd = &s0.return_data;
                    let mut sb = [0u8; 32];
                    sb.copy_from_slice(&rd[0..32]);
                    let sqrt_price_x96 = U256::from_be_bytes(sb);
                    let mut tb = [0u8; 4];
                    tb[1..4].copy_from_slice(&rd[61..64]);
                    let tick = crate::v3_snapshot::sign_extend_i24_pub(u32::from_be_bytes(tb));
                    let mut lb = [0u8; 16];
                    lb.copy_from_slice(&liq.return_data[16..32]);
                    let liquidity = u128::from_be_bytes(lb);
                    Some(PoolStateUpdate::V3 {
                        sqrt_price_x96,
                        tick,
                        liquidity,
                    })
                }
                _ => None,
            }
        } else {
            match results.get(start) {
                Some(r) if r.success && r.return_data.len() >= 96 => {
                    let rd = &r.return_data;
                    let mut b0 = [0u8; 16];
                    b0.copy_from_slice(&rd[16..32]);
                    let mut b1 = [0u8; 16];
                    b1.copy_from_slice(&rd[48..64]);
                    let mut tsb = [0u8; 4];
                    tsb.copy_from_slice(&rd[92..96]);
                    Some(PoolStateUpdate::V2 {
                        reserve0: u128::from_be_bytes(b0),
                        reserve1: u128::from_be_bytes(b1),
                        block_timestamp_last: u32::from_be_bytes(tsb),
                    })
                }
                _ => None,
            }
        };
        out.push(upd);
    }
    Ok(out)
}

/// Canonical Multicall3 deployment on Arbitrum One (and every other Multicall3
/// chain). Operators can override via the dedicated `multicall3_address`
/// helper if a fork ever lands at a different address, but at the time of
/// writing this is the universal deployment.
pub const MULTICALL3_ARBITRUM: [u8; 20] = [
    0xcA, 0x11, 0xbd, 0xe0, 0x59, 0x77, 0xb3, 0x63, 0x11, 0x67, 0x02, 0x88, 0x62, 0xbE, 0x2a, 0x17,
    0x39, 0x76, 0xCA, 0x11,
];

/// `aggregate3((address,bool,bytes)[])` selector.
const AGGREGATE3_SELECTOR: [u8; 4] = [0x82, 0xad, 0x56, 0xcb];

/// One element of a Multicall3 batch.
#[derive(Debug, Clone)]
pub struct Call3 {
    /// Target contract address.
    pub target: Address,
    /// If `true`, the batch continues even if this call reverts. Set to
    /// `true` for tick reads (some ticks in the requested window will be
    /// uninitialised; Uniswap V3's `ticks(int24)` returns zeroed structs for
    /// uninitialised ticks, never reverts, but we keep `allowFailure=true`
    /// defensively).
    pub allow_failure: bool,
    /// Selector + ABI-encoded args.
    pub call_data: Vec<u8>,
}

/// One element of a Multicall3 response.
#[derive(Debug, Clone)]
pub struct CallResult {
    /// Whether the inner call succeeded (did not revert).
    pub success: bool,
    /// Raw `returnData` bytes. Empty if the call reverted.
    pub return_data: Vec<u8>,
}

/// Issue a single Multicall3 `aggregate3` request. Returns the per-call result
/// list in the same order as `calls`.
pub fn aggregate3(
    client: &ArbitrumRpcClient,
    multicall3: &Address,
    calls: &[Call3],
    block: BlockTag,
) -> Result<Vec<CallResult>, RpcError> {
    let data = encode_aggregate3(calls);
    let resp = client.eth_call(multicall3, &data, block)?;
    decode_aggregate3_response(&resp, calls.len())
}

/// ABI-encode the `aggregate3` call.
fn encode_aggregate3(calls: &[Call3]) -> Vec<u8> {
    let n = calls.len();
    // Layout:
    //   selector (4)
    //   offset_to_array (32) = 0x20 (next word)
    //   array_len (32) = n
    //   for each call i in 0..n: head[i] (32) = offset_to_tuple_i (relative to start of array tail)
    //   for each call i: tuple_i tail data
    //
    // tuple_i layout (dynamic — Call3 has a `bytes` field):
    //   target (32, address right-aligned)
    //   allow_failure (32, bool)
    //   offset_to_callData (32) = 0x60 (third word — first byte after the three head slots)
    //   callData_len (32)
    //   callData (padded to 32)
    let array_head_start = 4 + 32 + 32; // selector + outer_offset + array_len
    let head_size = 32 * n;
    let mut tuple_offsets: Vec<usize> = Vec::with_capacity(n);
    let mut acc = head_size;
    let mut tuple_bodies: Vec<Vec<u8>> = Vec::with_capacity(n);
    for c in calls {
        // head[i] points to the start of tuple_i tail data, relative to the
        // start of the *array tail* (which begins right after array_len —
        // i.e. at offset `array_head_start - 32`, but we track it as if 0 is
        // the start of the array tail).
        tuple_offsets.push(acc);
        // Encode tuple body (target, allowFailure, offset_to_callData,
        // callData_len, callData_padded).
        let mut body = Vec::with_capacity(32 * 3 + 32 + c.call_data.len() + 32);
        // target
        let mut target_word = [0u8; 32];
        target_word[12..32].copy_from_slice(&c.target);
        body.extend_from_slice(&target_word);
        // allowFailure
        let mut bool_word = [0u8; 32];
        if c.allow_failure {
            bool_word[31] = 1;
        }
        body.extend_from_slice(&bool_word);
        // offset to callData (within the tuple, after the 3 head words)
        let mut off_word = [0u8; 32];
        off_word[31] = 0x60;
        body.extend_from_slice(&off_word);
        // callData_len
        let mut len_word = [0u8; 32];
        let len_be = (c.call_data.len() as u64).to_be_bytes();
        len_word[24..32].copy_from_slice(&len_be);
        body.extend_from_slice(&len_word);
        // callData padded to 32
        body.extend_from_slice(&c.call_data);
        let pad = (32 - (c.call_data.len() % 32)) % 32;
        for _ in 0..pad {
            body.push(0);
        }
        acc += body.len();
        tuple_bodies.push(body);
    }
    let array_tail_size = head_size + tuple_bodies.iter().map(|b| b.len()).sum::<usize>();
    let mut out = Vec::with_capacity(4 + 64 + array_tail_size);
    out.extend_from_slice(&AGGREGATE3_SELECTOR);
    // outer_offset = 0x20
    let mut off = [0u8; 32];
    off[31] = 0x20;
    out.extend_from_slice(&off);
    // array_len
    let mut len_w = [0u8; 32];
    let len_be = (n as u64).to_be_bytes();
    len_w[24..32].copy_from_slice(&len_be);
    out.extend_from_slice(&len_w);
    // heads
    for off in &tuple_offsets {
        let mut w = [0u8; 32];
        let be = (*off as u64).to_be_bytes();
        w[24..32].copy_from_slice(&be);
        out.extend_from_slice(&w);
    }
    // tails
    for body in &tuple_bodies {
        out.extend_from_slice(body);
    }
    let _ = array_head_start;
    out
}

/// Decode the `Result[] memory` returned by `aggregate3`.
fn decode_aggregate3_response(resp: &[u8], expected_n: usize) -> Result<Vec<CallResult>, RpcError> {
    if resp.len() < 64 {
        return Err(RpcError::Decode("multicall3 response too short".into()));
    }
    // resp[0..32] = offset to array (always 0x20 for non-nested top-level)
    let array_len = read_offset(&resp[32..64])?;
    if array_len != expected_n {
        return Err(RpcError::Decode(format!(
            "multicall3 length mismatch: expected {expected_n}, got {array_len}"
        )));
    }
    let array_tail_start = 64; // after offset + len
    if resp.len() < array_tail_start + array_len * 32 {
        return Err(RpcError::Decode("multicall3 head slot out of range".into()));
    }
    let mut out: Vec<CallResult> = Vec::with_capacity(array_len);
    for i in 0..array_len {
        let head_pos = array_tail_start + i * 32;
        let rel = read_offset(&resp[head_pos..head_pos + 32])?;
        // rel is relative to the start of the array tail (which begins right
        // after array_len, i.e. at `array_tail_start`).
        let tuple_start = array_tail_start + rel;
        if resp.len() < tuple_start + 32 * 2 {
            return Err(RpcError::Decode("multicall3 tuple head OOR".into()));
        }
        let success = resp[tuple_start + 31] != 0;
        // word[1] = offset to bytes (relative to tuple start). Per Multicall3
        // convention this is always 0x40 (two head words inside the tuple).
        let bytes_offset_rel = read_offset(&resp[tuple_start + 32..tuple_start + 64])?;
        let bytes_start = tuple_start + bytes_offset_rel;
        if resp.len() < bytes_start + 32 {
            return Err(RpcError::Decode("multicall3 returnData length OOR".into()));
        }
        let data_len = read_offset(&resp[bytes_start..bytes_start + 32])?;
        let data_pos = bytes_start + 32;
        if resp.len() < data_pos + data_len {
            return Err(RpcError::Decode("multicall3 returnData payload OOR".into()));
        }
        let return_data = resp[data_pos..data_pos + data_len].to_vec();
        out.push(CallResult {
            success,
            return_data,
        });
    }
    Ok(out)
}

fn read_offset(w: &[u8]) -> Result<usize, RpcError> {
    if w.len() < 32 {
        return Err(RpcError::Decode("multicall3 offset word short".into()));
    }
    // Reject offsets > 2^48 to avoid surprises (the Multicall3 contract never
    // returns offsets anywhere near this large; treating > 2^48 as malformed
    // catches accidental ABI-decoding drift).
    for &b in &w[0..26] {
        if b != 0 {
            return Err(RpcError::Decode("multicall3 offset too large".into()));
        }
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&w[24..32]);
    Ok(u64::from_be_bytes(buf) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_empty_batch() {
        let bytes = encode_aggregate3(&[]);
        // selector + outer_offset(0x20) + array_len(0)
        assert_eq!(bytes.len(), 4 + 32 + 32);
        assert_eq!(&bytes[0..4], &AGGREGATE3_SELECTOR);
        // outer_offset
        assert_eq!(bytes[35], 0x20);
        // array_len = 0
        for &b in &bytes[36..68] {
            assert_eq!(b, 0);
        }
    }

    #[test]
    fn encodes_single_call_with_4byte_calldata() {
        let call = Call3 {
            target: [0x11; 20],
            allow_failure: true,
            call_data: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let bytes = encode_aggregate3(&[call]);
        // selector(4) + outer_offset(32) + array_len(32) + head(32)
        //   + target(32) + allowFailure(32) + cd_offset(32) + cd_len(32) + cd_padded(32)
        // = 4 + 32 * 8 = 260
        assert_eq!(bytes.len(), 4 + 32 * 8);
    }

    #[test]
    fn round_trip_decode() {
        // Encode 2 dummy calls and decode a fake response.
        // Response layout (assume both succeed, each returns 4 bytes):
        //   offset_to_array (32) = 0x20
        //   array_len (32) = 2
        //   head[0] (32) = offset to tuple_0 (rel) = 0x40 (2 head slots)
        //   head[1] (32) = offset to tuple_1 (rel) = 0x40 + tuple_0_size
        //   tuple_0: success(32) + bytes_offset(32)=0x40 + bytes_len(32)=4 + bytes_padded(32)
        //   tuple_1: same
        let mut resp = Vec::new();
        // outer offset
        let mut w = [0u8; 32];
        w[31] = 0x20;
        resp.extend_from_slice(&w);
        // len
        let mut w = [0u8; 32];
        w[31] = 2;
        resp.extend_from_slice(&w);
        // head[0] = 0x40 (after the 2 heads)
        let mut w = [0u8; 32];
        w[31] = 0x40;
        resp.extend_from_slice(&w);
        // head[1] = 0x40 + 128 = 0xC0
        let mut w = [0u8; 32];
        w[31] = 0xC0;
        resp.extend_from_slice(&w);
        // tuple_0
        let mut w = [0u8; 32];
        w[31] = 1;
        resp.extend_from_slice(&w); // success
        let mut w = [0u8; 32];
        w[31] = 0x40;
        resp.extend_from_slice(&w); // bytes_offset
        let mut w = [0u8; 32];
        w[31] = 4;
        resp.extend_from_slice(&w); // bytes_len
        let mut w = [0u8; 32];
        w[0] = 0xAA;
        w[1] = 0xBB;
        w[2] = 0xCC;
        w[3] = 0xDD;
        resp.extend_from_slice(&w);
        // tuple_1
        let mut w = [0u8; 32];
        w[31] = 1;
        resp.extend_from_slice(&w);
        let mut w = [0u8; 32];
        w[31] = 0x40;
        resp.extend_from_slice(&w);
        let mut w = [0u8; 32];
        w[31] = 4;
        resp.extend_from_slice(&w);
        let mut w = [0u8; 32];
        w[0] = 0x11;
        w[1] = 0x22;
        w[2] = 0x33;
        w[3] = 0x44;
        resp.extend_from_slice(&w);

        let parsed = decode_aggregate3_response(&resp, 2).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].success);
        assert_eq!(parsed[0].return_data, vec![0xAA, 0xBB, 0xCC, 0xDD]);
        assert!(parsed[1].success);
        assert_eq!(parsed[1].return_data, vec![0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn rejects_length_mismatch() {
        let mut resp = vec![0u8; 32 * 2];
        resp[31] = 0x20;
        resp[63] = 5;
        assert!(decode_aggregate3_response(&resp, 2).is_err());
    }
}
