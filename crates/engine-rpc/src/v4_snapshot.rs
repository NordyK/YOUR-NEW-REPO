//! Uniswap V4 pool state snapshots via the `StateView` lens contract.
//!
//! V4 keeps every pool's state inside the singleton `PoolManager`'s transient/packed
//! storage; the canonical way to read it off-chain is the `StateView` helper, which
//! exposes per-`poolId` getters that mirror Uniswap V3's slot0/liquidity/ticks ABI:
//!
//! * `getSlot0(bytes32 poolId) -> (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee,
//!   uint24 lpFee)` — selector `0xc815641c`.
//! * `getLiquidity(bytes32 poolId) -> uint128` — selector `0xfa6793d5`.
//! * `getTickBitmap(bytes32 poolId, int16 wordPos) -> uint256` — selector `0x1c7ccb4c`.
//! * `getTickInfo(bytes32 poolId, int24 tick) -> (uint128 liquidityGross,
//!   int128 liquidityNet, uint256 feeGrowthOutside0X128, uint256 feeGrowthOutside1X128)`
//!   — selector `0x7c40f1fe` (same return layout as V3 `ticks(int24)` but a distinct
//!   selector — the StateView getters take a leading `bytes32 poolId` arg).
//!
//! Hookless V4 pools use the SAME concentrated-liquidity math as Uniswap V3, so the
//! decoded state populates the existing `V3State` store and is quoted by the V3 kernel.
//!
//! Live-validated 2026-05 against the Arbitrum StateView
//! (`0x76fd297e2d437cd7f76d50f01afe6160f86e9990`) on the ETH/USDC fee-500 pool
//! `0x864abca0…e903a8`: getSlot0 → sqrtPriceX96=3624747913788467314356174, tick=-199857,
//! lpFee=500; getLiquidity → 440907248635455009.

use alloy_primitives::U256;

use engine_feed::Address;

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};
use crate::v3_snapshot::{sign_extend_i24_pub, V3TickSnapshot};

/// `keccak256("getSlot0(bytes32)")[0..4]`.
const GET_SLOT0_SELECTOR: [u8; 4] = [0xc8, 0x15, 0x64, 0x1c];
/// `keccak256("getLiquidity(bytes32)")[0..4]`.
const GET_LIQUIDITY_SELECTOR: [u8; 4] = [0xfa, 0x67, 0x93, 0xd5];
/// `keccak256("getTickBitmap(bytes32,int16)")[0..4]`.
const GET_TICK_BITMAP_SELECTOR: [u8; 4] = [0x1c, 0x7c, 0xcb, 0x4c];
/// `keccak256("getTickInfo(bytes32,int24)")[0..4]`.
const GET_TICK_INFO_SELECTOR: [u8; 4] = [0x7c, 0x40, 0xf1, 0xfe];

/// V4 `getSlot0` snapshot. Mirrors the V3 slot0 fields the engine consumes (sqrt price +
/// tick) plus the live `lpFee` (V4 fees can be dynamic).
#[derive(Debug, Clone, Copy)]
pub struct V4Slot0Snapshot {
    /// sqrtPriceX96.
    pub sqrt_price_x96: U256,
    /// Current tick.
    pub tick: i32,
    /// LP fee in pips (10⁻⁶). For static-fee pools this equals the PoolKey fee.
    pub lp_fee: u32,
}

/// Decode a `getSlot0(bytes32)` return blob into a [`V4Slot0Snapshot`]. Shared by
/// the per-pool [`fetch_v4_slot0`] and the batched [`fetch_v4_states_batched`] so
/// the two paths produce byte-for-byte identical state.
fn decode_v4_slot0(resp: &[u8]) -> Result<V4Slot0Snapshot, RpcError> {
    if resp.len() < 128 {
        return Err(RpcError::Decode("v4 getSlot0 response too short".into()));
    }
    let mut sqrt_buf = [0u8; 32];
    sqrt_buf.copy_from_slice(&resp[0..32]);
    let sqrt_price_x96 = U256::from_be_bytes(sqrt_buf);
    // tick: int24 right-aligned in word[32..64].
    let mut tick_buf = [0u8; 4];
    tick_buf[1..4].copy_from_slice(&resp[61..64]);
    let tick = sign_extend_i24_pub(u32::from_be_bytes(tick_buf));
    // protocolFee: word[64..96] (ignored). lpFee: uint24 right-aligned in word[96..128].
    let mut fee_buf = [0u8; 4];
    fee_buf[1..4].copy_from_slice(&resp[125..128]);
    let lp_fee = u32::from_be_bytes(fee_buf);
    Ok(V4Slot0Snapshot {
        sqrt_price_x96,
        tick,
        lp_fee,
    })
}

/// Decode a `getLiquidity(bytes32)` return blob into the active liquidity `u128`.
/// Shared by [`fetch_v4_liquidity`] and [`fetch_v4_states_batched`].
fn decode_v4_liquidity(resp: &[u8]) -> Result<u128, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode(
            "v4 getLiquidity response too short".into(),
        ));
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&resp[16..32]);
    Ok(u128::from_be_bytes(buf))
}

/// Fetch `getSlot0` for a V4 pool by `poolId`.
pub fn fetch_v4_slot0(
    client: &ArbitrumRpcClient,
    state_view: &Address,
    pool_id: &[u8; 32],
    block: BlockTag,
) -> Result<V4Slot0Snapshot, RpcError> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&GET_SLOT0_SELECTOR);
    data.extend_from_slice(pool_id);
    let resp = client.eth_call(state_view, &data, block)?;
    decode_v4_slot0(&resp)
}

/// Fetch active `getLiquidity` for a V4 pool by `poolId`.
pub fn fetch_v4_liquidity(
    client: &ArbitrumRpcClient,
    state_view: &Address,
    pool_id: &[u8; 32],
    block: BlockTag,
) -> Result<u128, RpcError> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&GET_LIQUIDITY_SELECTOR);
    data.extend_from_slice(pool_id);
    let resp = client.eth_call(state_view, &data, block)?;
    decode_v4_liquidity(&resp)
}

/// Cheap V4 state (slot0 + active liquidity) for one pool. Mirrors the V3 slot0+
/// liquidity pair the engine snapshots in Phase 1; tick windows are deferred to
/// Phase 2 exactly as for V3.
#[derive(Debug, Clone, Copy)]
pub struct V4CheapState {
    /// Decoded `getSlot0` view.
    pub slot0: V4Slot0Snapshot,
    /// Decoded `getLiquidity`.
    pub liquidity: u128,
}

/// Batch the cheap V4 reads (`getSlot0` + `getLiquidity`) for many pools through ONE
/// Multicall3 `aggregate3` against the `StateView` lens. Returns one `Option` per
/// input `pool_id` (`None` if either sub-call failed or decoded short), in input
/// order.
///
/// The transport is batched; each sub-return is decoded with the SAME
/// [`decode_v4_slot0`] / [`decode_v4_liquidity`] logic the per-pool
/// [`fetch_v4_slot0`] / [`fetch_v4_liquidity`] use, so the decoded state is
/// byte-for-byte identical to the per-pool path. Tick windows are NOT fetched here
/// (Phase 2 handles those for survivors only).
pub fn fetch_v4_states_batched(
    client: &ArbitrumRpcClient,
    state_view: &Address,
    pool_ids: &[[u8; 32]],
    block: BlockTag,
) -> Result<Vec<Option<V4CheapState>>, RpcError> {
    use crate::multicall::{aggregate3, Call3};
    let mut calls: Vec<Call3> = Vec::with_capacity(pool_ids.len() * 2);
    for pool_id in pool_ids {
        let mut slot0_data = Vec::with_capacity(4 + 32);
        slot0_data.extend_from_slice(&GET_SLOT0_SELECTOR);
        slot0_data.extend_from_slice(pool_id);
        calls.push(Call3 {
            target: *state_view,
            allow_failure: true,
            call_data: slot0_data,
        });
        let mut liq_data = Vec::with_capacity(4 + 32);
        liq_data.extend_from_slice(&GET_LIQUIDITY_SELECTOR);
        liq_data.extend_from_slice(pool_id);
        calls.push(Call3 {
            target: *state_view,
            allow_failure: true,
            call_data: liq_data,
        });
    }
    let results = aggregate3(
        client,
        &crate::multicall::MULTICALL3_ARBITRUM,
        &calls,
        block,
    )?;
    let mut out: Vec<Option<V4CheapState>> = Vec::with_capacity(pool_ids.len());
    for i in 0..pool_ids.len() {
        let s0 = results.get(i * 2);
        let liq = results.get(i * 2 + 1);
        let decoded = match (s0, liq) {
            (Some(s0), Some(liq)) if s0.success && liq.success => {
                match (
                    decode_v4_slot0(&s0.return_data),
                    decode_v4_liquidity(&liq.return_data),
                ) {
                    (Ok(slot0), Ok(liquidity)) => Some(V4CheapState { slot0, liquidity }),
                    _ => None,
                }
            }
            _ => None,
        };
        out.push(decoded);
    }
    Ok(out)
}

/// Fetch one `getTickInfo` for a V4 pool. Same return layout as V3 `ticks(int24)`.
pub fn fetch_v4_tick(
    client: &ArbitrumRpcClient,
    state_view: &Address,
    pool_id: &[u8; 32],
    tick: i32,
    block: BlockTag,
) -> Result<V3TickSnapshot, RpcError> {
    let mut data = Vec::with_capacity(4 + 64);
    data.extend_from_slice(&GET_TICK_INFO_SELECTOR);
    data.extend_from_slice(pool_id);
    data.extend_from_slice(&encode_i24_word(tick));
    let resp = client.eth_call(state_view, &data, block)?;
    if resp.len() < 64 {
        return Err(RpcError::Decode("v4 getTickInfo response too short".into()));
    }
    let mut gross_buf = [0u8; 16];
    gross_buf.copy_from_slice(&resp[16..32]);
    let liquidity_gross = u128::from_be_bytes(gross_buf);
    let mut net_buf = [0u8; 16];
    net_buf.copy_from_slice(&resp[48..64]);
    let liquidity_net = i128::from_be_bytes(net_buf);
    Ok(V3TickSnapshot {
        tick,
        liquidity_gross,
        liquidity_net,
    })
}

/// Fetch one `getTickBitmap` word for a V4 pool.
pub fn fetch_v4_tick_bitmap(
    client: &ArbitrumRpcClient,
    state_view: &Address,
    pool_id: &[u8; 32],
    word_pos: i16,
    block: BlockTag,
) -> Result<U256, RpcError> {
    let mut data = Vec::with_capacity(4 + 64);
    data.extend_from_slice(&GET_TICK_BITMAP_SELECTOR);
    data.extend_from_slice(pool_id);
    // int16 word_pos, sign-extended into a 32-byte word.
    let mut word = [0u8; 32];
    if word_pos < 0 {
        word.fill(0xff);
    }
    word[30..32].copy_from_slice(&word_pos.to_be_bytes());
    data.extend_from_slice(&word);
    let resp = client.eth_call(state_view, &data, block)?;
    if resp.len() < 32 {
        return Err(RpcError::Decode(
            "v4 getTickBitmap response too short".into(),
        ));
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&resp[0..32]);
    Ok(U256::from_be_bytes(buf))
}

/// Fetch a window of initialised ticks around `centre_tick` using the StateView tick
/// bitmap. Walks the bitmap words covering `±width_ticks` tick-spacings from the current
/// tick, decodes the set bits into initialised tick indices, then reads each tick's
/// `liquidityGross`/`liquidityNet`. This is the V4 analogue of `fetch_v3_tick_window` but
/// bitmap-driven (one read per 256-tick word) rather than probing every candidate tick.
pub fn fetch_v4_tick_window(
    client: &ArbitrumRpcClient,
    state_view: &Address,
    pool_id: &[u8; 32],
    centre_tick: i32,
    tick_spacing: i32,
    width_ticks: i32,
    block: BlockTag,
) -> Result<Vec<V3TickSnapshot>, RpcError> {
    if tick_spacing <= 0 {
        return Err(RpcError::Decode("non-positive tick spacing".into()));
    }
    // Compressed tick = floor(tick / tickSpacing). The bitmap is indexed by
    // (wordPos = compressed >> 8, bitPos = compressed & 255).
    let compressed = (centre_tick as i64).div_euclid(tick_spacing as i64) as i32;
    let half = width_ticks; // window half-width in tick-spacings
    let lo_comp = compressed - half;
    let hi_comp = compressed + half;
    let lo_word = (lo_comp >> 8) as i16;
    let hi_word = (hi_comp >> 8) as i16;

    let mut out = Vec::with_capacity((2 * width_ticks as usize) + 1);
    let mut wp = lo_word;
    while wp <= hi_word {
        let bitmap = match fetch_v4_tick_bitmap(client, state_view, pool_id, wp, block) {
            Ok(b) => b,
            Err(_) => {
                wp = wp.saturating_add(1);
                continue;
            }
        };
        if !bitmap.is_zero() {
            for bit in 0u32..256u32 {
                if bitmap.bit(bit as usize) {
                    let comp = ((wp as i32) << 8) + bit as i32;
                    if comp < lo_comp || comp > hi_comp {
                        continue;
                    }
                    let tick = comp * tick_spacing;
                    if let Ok(snap) = fetch_v4_tick(client, state_view, pool_id, tick, block) {
                        if snap.liquidity_gross != 0 {
                            out.push(snap);
                        }
                    }
                }
            }
        }
        if wp == hi_word {
            break;
        }
        wp = wp.saturating_add(1);
    }
    out.sort_by_key(|s| s.tick);
    Ok(out)
}

/// Encode a signed `int24` into a 32-byte ABI word (sign-extended).
fn encode_i24_word(tick: i32) -> [u8; 32] {
    let mut word = [0u8; 32];
    if tick < 0 {
        word.fill(0xff);
    }
    let raw = (tick as i64).to_be_bytes();
    word[29..32].copy_from_slice(&raw[5..8]);
    word
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_keccak::{Hasher, Keccak};

    fn selector(sig: &str) -> [u8; 4] {
        let mut k = Keccak::v256();
        k.update(sig.as_bytes());
        let mut out = [0u8; 32];
        k.finalize(&mut out);
        [out[0], out[1], out[2], out[3]]
    }

    #[test]
    fn state_view_selectors_match_signatures() {
        // Locks the StateView selectors to their canonical signatures. The getters take a
        // leading `bytes32 poolId`, so they DIFFER from V3's bare `tickBitmap(int16)` /
        // `ticks(int24)`. Live-verified 2026-05: the bare-V3 selectors revert on StateView.
        assert_eq!(GET_SLOT0_SELECTOR, selector("getSlot0(bytes32)"));
        assert_eq!(GET_LIQUIDITY_SELECTOR, selector("getLiquidity(bytes32)"));
        assert_eq!(
            GET_TICK_BITMAP_SELECTOR,
            selector("getTickBitmap(bytes32,int16)")
        );
        assert_eq!(
            GET_TICK_INFO_SELECTOR,
            selector("getTickInfo(bytes32,int24)")
        );
        assert_eq!(GET_TICK_BITMAP_SELECTOR, [0x1c, 0x7c, 0xcb, 0x4c]);
        assert_eq!(GET_TICK_INFO_SELECTOR, [0x7c, 0x40, 0xf1, 0xfe]);
    }

    #[test]
    fn encode_i24_positive() {
        let w = encode_i24_word(60);
        assert_eq!(&w[29..32], &[0x00, 0x00, 0x3C]);
        assert!(w[0..29].iter().all(|&b| b == 0));
    }

    #[test]
    fn encode_i24_negative() {
        // -199857 → int24 two's complement = 0xFCF30F. div not needed; just bit pattern.
        let w = encode_i24_word(-1);
        assert_eq!(&w[29..32], &[0xFF, 0xFF, 0xFF]);
        assert!(w[0..29].iter().all(|&b| b == 0xFF));
    }
}
