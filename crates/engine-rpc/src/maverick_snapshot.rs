//! Maverick V2 pool state snapshot reader.
//!
//! Reads the basic per-pool views and constructs a `MaverickState` with an
//! EMPTY tick window (so quote_d18 returns TickWindowExhausted on any non-zero
//! amount). Full lens-based tick fetch lives in a follow-up — this surface
//! exists so discovery + state-store wiring can be exercised end-to-end while
//! lens integration is iterated.

use alloy_primitives::{Address as AmmAddress, U256};
use engine_amm::maverick_v2::{MaverickState, TokenScale};
use engine_feed::Address;

#[inline]
fn to_amm_addr(a: [u8; 20]) -> AmmAddress {
    AmmAddress::from(a)
}
use tiny_keccak::{Hasher, Keccak};

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

fn selector(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

fn token_a_selector() -> [u8; 4] {
    selector(b"tokenA()")
}

fn token_b_selector() -> [u8; 4] {
    selector(b"tokenB()")
}

fn token_a_scale_selector() -> [u8; 4] {
    selector(b"tokenAScale()")
}

fn token_b_scale_selector() -> [u8; 4] {
    selector(b"tokenBScale()")
}

fn fee_selector() -> [u8; 4] {
    selector(b"fee(bool)")
}

fn tick_spacing_selector() -> [u8; 4] {
    selector(b"tickSpacing()")
}

fn lookback_selector() -> [u8; 4] {
    selector(b"lookback()")
}

fn kinds_selector() -> [u8; 4] {
    selector(b"kinds()")
}

fn permissioned_swap_selector() -> [u8; 4] {
    selector(b"permissionedSwap()")
}

fn lending_fee_selector() -> [u8; 4] {
    selector(b"lendingFeeRateD18()")
}

fn decode_address(resp: &[u8]) -> Result<Address, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode("maverick address word short".into()));
    }
    if resp[0..12].iter().any(|&b| b != 0) {
        return Err(RpcError::Decode("maverick address padding".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Ok(a)
}

fn decode_u256(resp: &[u8]) -> Result<U256, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode("maverick u256 word short".into()));
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&resp[0..32]);
    Ok(U256::from_be_bytes(buf))
}

/// Fetch a Maverick V2 pool's basic config + scale + fees. Tick window is left
/// empty here — fill via the lens fetcher in a follow-up.
pub fn fetch_maverick_state_basic(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
    block_number: u64,
) -> Result<MaverickState, RpcError> {
    let token_a = decode_address(&client.eth_call(pool, &token_a_selector(), block)?)?;
    let token_b = decode_address(&client.eth_call(pool, &token_b_selector(), block)?)?;
    let token_a_scale_raw =
        decode_u256(&client.eth_call(pool, &token_a_scale_selector(), block)?)?;
    let token_b_scale_raw =
        decode_u256(&client.eth_call(pool, &token_b_scale_selector(), block)?)?;

    // fee(bool true) = zto, fee(bool false) = otz
    let mut fee_zto_call = Vec::with_capacity(4 + 32);
    fee_zto_call.extend_from_slice(&fee_selector());
    let mut bool_true = [0u8; 32];
    bool_true[31] = 1;
    fee_zto_call.extend_from_slice(&bool_true);
    let fee_zto = decode_u256(&client.eth_call(pool, &fee_zto_call, block)?)?;

    let mut fee_otz_call = Vec::with_capacity(4 + 32);
    fee_otz_call.extend_from_slice(&fee_selector());
    fee_otz_call.extend_from_slice(&[0u8; 32]);
    let fee_otz = decode_u256(&client.eth_call(pool, &fee_otz_call, block)?)?;

    let tick_spacing_raw = decode_u256(&client.eth_call(pool, &tick_spacing_selector(), block)?)?;
    let lookback_raw = decode_u256(&client.eth_call(pool, &lookback_selector(), block)?)?;
    let kinds_raw = decode_u256(&client.eth_call(pool, &kinds_selector(), block)?)?;
    let lending_fee = decode_u256(&client.eth_call(pool, &lending_fee_selector(), block)?)?;
    let permissioned_raw = client.eth_call(pool, &permissioned_swap_selector(), block)?;
    let permissioned = permissioned_raw.len() >= 32 && permissioned_raw[31] != 0;

    let token0_scale = TokenScale::from_pool_scale(token_a_scale_raw)
        .map_err(|e| RpcError::Decode(format!("maverick scale a: {:?}", e)))?;
    let token1_scale = TokenScale::from_pool_scale(token_b_scale_raw)
        .map_err(|e| RpcError::Decode(format!("maverick scale b: {:?}", e)))?;

    Ok(MaverickState {
        address: to_amm_addr(*pool),
        token0: to_amm_addr(token_a),
        token1: to_amm_addr(token_b),
        token0_scale,
        token1_scale,
        fee_zto_d18: fee_zto,
        fee_otz_d18: fee_otz,
        tick_spacing: tick_spacing_raw
            .try_into()
            .map_err(|_| RpcError::Decode("maverick tickSpacing > u16".into()))?,
        lookback: lookback_raw
            .try_into()
            .map_err(|_| RpcError::Decode("maverick lookback > u32".into()))?,
        kinds: kinds_raw
            .try_into()
            .map_err(|_| RpcError::Decode("maverick kinds > u8".into()))?,
        lending_fee_rate_d18: lending_fee,
        protocol_fee_ratio_d3: 0, // requires getState() — defer until lens is wired
        permissioned_swap: permissioned,
        sqrt_price_d18: U256::ZERO, // populated by lens fetch
        active_tick: 0,
        reserve0_d18: U256::ZERO,
        reserve1_d18: U256::ZERO,
        last_timestamp: 0,
        block_number,
        log_index: 0,
        tick_radius: 0,
        ticks: Vec::new(), // empty — kernel returns TickWindowExhausted on quote
    })
}
