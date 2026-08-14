//! Maverick V2 pool lens reader — fetches the tick window around the active tick.
//!
//! The Maverick V2 math kernel quotes against an explicit per-tick state list
//! (reserves, sqrt-bounds, liquidity). This module wraps the **verified** Maverick V2
//! `MaverickV2PoolLens` + `IMaverickV2Pool` view ABIs (confirmed against docs.mav.xyz on
//! 2026-05-28) and converts the responses into the in-memory tick list the engine consumes.
//!
//! # Root-cause fix (2026-05-28)
//! The previous code called `getTicksAroundActiveWLiquidity(address,int32)` — a function that
//! does NOT exist in the Maverick V2 PoolLens interface. The eth_call hit a bogus selector and
//! decoded garbage (`activeTick = 2.06e9` for near-1:1 pools). The real ABIs are:
//!
//! * `IMaverickV2PoolLens.getTicksAroundActive(IMaverickV2Pool pool, int32 tickRadius)`
//!   → `(int32[] ticks, IMaverickV2Pool.TickState[] tickStates)` — tick indices + per-tick
//!   reserves/totalSupply. NO sqrt prices or liquidity (those weren't in the real return).
//! * `IMaverickV2PoolLens.getTickSqrtPriceAndL(IMaverickV2Pool pool, int32 tick)`
//!   → `(uint256 sqrtPrice, uint256 liquidity)` — per-tick sqrt price + liquidity. One call
//!   per tick; cheap (startup/snapshot path, not hot path).
//! * `IMaverickV2Pool.getState()` → `State` struct (reserveA/B, activeTick, protocolFeeRatio…).
//!
//! `TickState = (uint128 reserveA, uint128 reserveB, uint128 totalSupply, uint32[4] binIdsByTick)`
//! — fixed-size 128-byte tuple per tick.

use alloy_primitives::U256;
use engine_amm::maverick_v2::MaverickTick;
use engine_feed::Address;
use tiny_keccak::{Hasher, Keccak};

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

fn selector(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

/// VERIFIED selector — `getTicksAroundActive(IMaverickV2Pool,int32)`. The `IMaverickV2Pool`
/// argument ABI-encodes as a plain `address`, so the canonical signature string for the
/// selector hash uses `address`.
fn get_ticks_around_active_selector() -> [u8; 4] {
    selector(b"getTicksAroundActive(address,int32)")
}

/// VERIFIED selector — `getTickSqrtPriceAndL(IMaverickV2Pool,int32)`.
fn get_tick_sqrt_price_and_l_selector() -> [u8; 4] {
    selector(b"getTickSqrtPriceAndL(address,int32)")
}

/// VERIFIED selector — `IMaverickV2Pool.getState()`.
fn get_state_selector() -> [u8; 4] {
    selector(b"getState()")
}

/// Decoded `IMaverickV2Pool.getState()` — the on-pool live state.
///
/// State struct (verified field order):
/// ```text
///   uint128 reserveA;          word[0]
///   uint128 reserveB;          word[1]
///   int64   lastTwaD8;         word[2]
///   int64   lastLogPriceD8;    word[3]
///   uint40  lastTimestamp;     word[4]
///   int32   activeTick;        word[5]
///   bool    isLocked;          word[6]
///   uint32  binCounter;        word[7]
///   uint8   protocolFeeRatioD3 word[8]
/// ```
/// Static tuple → 9 words = 288 bytes, encoded inline (no offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaverickPoolState {
    /// Token-A reserve (raw pool units).
    pub reserve_a: u128,
    /// Token-B reserve (raw pool units).
    pub reserve_b: u128,
    /// Unix seconds of the last state update.
    pub last_timestamp: u64,
    /// Current active tick.
    pub active_tick: i32,
    /// Whether the pool is mid-swap-locked (reentrancy guard).
    pub is_locked: bool,
    /// Monotonic bin counter.
    pub bin_counter: u32,
    /// Protocol fee ratio in 1e3 domain.
    pub protocol_fee_ratio_d3: u8,
}

/// Fetch + decode `IMaverickV2Pool.getState()`.
pub fn fetch_maverick_state_struct(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<MaverickPoolState, RpcError> {
    let resp = client.eth_call(pool, &get_state_selector(), block)?;
    decode_get_state(&resp)
}

/// Decode the 288-byte `State` tuple. Fail-closed on short input.
pub fn decode_get_state(resp: &[u8]) -> Result<MaverickPoolState, RpcError> {
    if resp.len() < 288 {
        return Err(RpcError::Decode("maverick getState head too short".into()));
    }
    let reserve_a = read_u128_low(&resp[0..32]);
    let reserve_b = read_u128_low(&resp[32..64]);
    // word[4] = bytes [128..160]; uint40 lastTimestamp lives in its low 5 bytes. Reading the
    // low 8 bytes [152..160] is safe (the upper 3 of those are zero for a uint40).
    let last_timestamp = {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&resp[152..160]);
        u64::from_be_bytes(buf)
    };
    let active_tick = decode_i32_word(&resp[160..192])?;
    let is_locked = resp[223] != 0; // low byte of word[6]
    let bin_counter = {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&resp[252..256]); // low 4 bytes of word[7]
        u32::from_be_bytes(buf)
    };
    let protocol_fee_ratio_d3 = resp[287]; // low byte of word[8]
    Ok(MaverickPoolState {
        reserve_a,
        reserve_b,
        last_timestamp,
        active_tick,
        is_locked,
        bin_counter,
        protocol_fee_ratio_d3,
    })
}

/// Decoded lens result: per-tick state (reserves + sqrt bounds + liquidity) plus the pool's
/// current active tick + sqrt price.
#[derive(Debug, Clone)]
pub struct MaverickLensResult {
    /// Per-tick state, sorted ascending by tick.
    pub ticks: Vec<MaverickTick>,
    /// Current active tick (from `getState`).
    pub active_tick: i32,
    /// Current sqrt price at the active tick (1e18 fixed-point, from `getTickSqrtPriceAndL`).
    pub sqrt_price_d18: U256,
    /// Protocol fee ratio (1e3 domain) from `getState`.
    pub protocol_fee_ratio_d3: u8,
}

/// Fetch the tick window around the active tick + each tick's sqrt price & liquidity.
///
/// Three-stage fetch (all startup/snapshot — not hot path):
///   1. `getState()` → activeTick + reserves + protocolFeeRatio.
///   2. `getTicksAroundActive(pool, radius)` → tick indices + per-tick reserves.
///   3. `getTickSqrtPriceAndL(pool, tick)` per tick → sqrtPrice + liquidity, plus the lower/upper
///      bounds (lower = this tick's sqrtPrice, upper = next tick's sqrtPrice).
pub fn fetch_maverick_lens(
    client: &ArbitrumRpcClient,
    lens_address: &Address,
    pool: &Address,
    tick_radius: i32,
    block: BlockTag,
) -> Result<MaverickLensResult, RpcError> {
    let state = fetch_maverick_state_struct(client, pool, block)?;

    // getTicksAroundActive(pool, radius)
    let mut data = Vec::with_capacity(4 + 64);
    data.extend_from_slice(&get_ticks_around_active_selector());
    data.extend_from_slice(&encode_address_word(pool));
    data.extend_from_slice(&encode_i32_word(tick_radius));
    let resp = client.eth_call(lens_address, &data, block)?;
    let (tick_indices, reserve_a, reserve_b, total_supply) = decode_get_ticks_around_active(&resp)?;

    // Per-tick sqrt price + liquidity. The tick's lower bound is its own sqrtPrice; the upper
    // bound is the next tick's sqrtPrice (Maverick ticks tile the price axis contiguously).
    let n = tick_indices.len();
    let mut sqrt_at: Vec<U256> = Vec::with_capacity(n);
    let mut liq_at: Vec<U256> = Vec::with_capacity(n);
    for &t in &tick_indices {
        let (sp, l) = fetch_tick_sqrt_price_and_l(client, lens_address, pool, t, block)?;
        sqrt_at.push(sp);
        liq_at.push(l);
    }

    let mut ticks: Vec<MaverickTick> = Vec::with_capacity(n);
    for i in 0..n {
        let lower = sqrt_at[i];
        // Upper bound = next tick's sqrt price if contiguous; else fall back to lower (degenerate
        // single-tick window — the kernel treats lower==upper as a zero-width tick and skips it).
        let upper = if i + 1 < n {
            sqrt_at[i + 1]
        } else {
            sqrt_at[i]
        };
        ticks.push(MaverickTick {
            tick: tick_indices[i],
            reserve0_d18: U256::from(reserve_a[i]),
            reserve1_d18: U256::from(reserve_b[i]),
            total_supply: U256::from(total_supply[i]),
            liquidity_d18: liq_at[i],
            sqrt_lower_price_d18: lower,
            sqrt_upper_price_d18: upper,
        });
    }
    ticks.sort_by_key(|t| t.tick);

    // Active-tick sqrt price: locate the active tick in the window; fall back to first.
    let sqrt_price_d18 = ticks
        .iter()
        .find(|t| t.tick == state.active_tick)
        .map(|t| t.sqrt_lower_price_d18)
        .or_else(|| ticks.first().map(|t| t.sqrt_lower_price_d18))
        .unwrap_or(U256::ZERO);

    Ok(MaverickLensResult {
        ticks,
        active_tick: state.active_tick,
        sqrt_price_d18,
        protocol_fee_ratio_d3: state.protocol_fee_ratio_d3,
    })
}

/// Fetch + decode `getTickSqrtPriceAndL(pool, tick)` → `(sqrtPrice, liquidity)`.
pub fn fetch_tick_sqrt_price_and_l(
    client: &ArbitrumRpcClient,
    lens_address: &Address,
    pool: &Address,
    tick: i32,
    block: BlockTag,
) -> Result<(U256, U256), RpcError> {
    let mut data = Vec::with_capacity(4 + 64);
    data.extend_from_slice(&get_tick_sqrt_price_and_l_selector());
    data.extend_from_slice(&encode_address_word(pool));
    data.extend_from_slice(&encode_i32_word(tick));
    let resp = client.eth_call(lens_address, &data, block)?;
    decode_tick_sqrt_price_and_l(&resp)
}

/// Decode the `(uint256 sqrtPrice, uint256 liquidity)` return — two static words.
pub fn decode_tick_sqrt_price_and_l(resp: &[u8]) -> Result<(U256, U256), RpcError> {
    if resp.len() < 64 {
        return Err(RpcError::Decode("getTickSqrtPriceAndL too short".into()));
    }
    Ok((read_u256(&resp[0..32]), read_u256(&resp[32..64])))
}

/// Decode `getTicksAroundActive` → `(int32[] ticks, TickState[] tickStates)`.
///
/// Two dynamic arrays, so the head is two offset words:
/// ```text
///   [0..32]   offset to ticks[]
///   [32..64]  offset to tickStates[]
/// ```
/// Returns `(tick_indices, reserve_a, reserve_b, total_supply)`, all length-aligned.
pub fn decode_get_ticks_around_active(
    resp: &[u8],
) -> Result<(Vec<i32>, Vec<u128>, Vec<u128>, Vec<u128>), RpcError> {
    if resp.len() < 64 {
        return Err(RpcError::Decode(
            "getTicksAroundActive head too short".into(),
        ));
    }
    let off_ticks = read_offset(&resp[0..32])?;
    let off_tick_states = read_offset(&resp[32..64])?;

    let tick_indices = decode_i32_array(resp, off_ticks)?;
    let (reserve_a, reserve_b, total_supply) = decode_tick_states(resp, off_tick_states)?;

    let n = tick_indices.len();
    if reserve_a.len() != n || reserve_b.len() != n || total_supply.len() != n {
        return Err(RpcError::Decode(format!(
            "maverick getTicksAroundActive length mismatch: ticks={} ra={} rb={} ts={}",
            n,
            reserve_a.len(),
            reserve_b.len(),
            total_supply.len(),
        )));
    }
    Ok((tick_indices, reserve_a, reserve_b, total_supply))
}

// ---------- ABI encode/decode helpers ----------

fn encode_address_word(a: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(a);
    w
}

fn encode_i32_word(v: i32) -> [u8; 32] {
    let mut w = [0u8; 32];
    if v < 0 {
        w.fill(0xff);
    }
    w[28..32].copy_from_slice(&v.to_be_bytes());
    w
}

fn read_u128_low(w: &[u8]) -> u128 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&w[16..32]);
    u128::from_be_bytes(buf)
}

fn read_offset(w: &[u8]) -> Result<usize, RpcError> {
    if w.len() < 32 {
        return Err(RpcError::Decode("lens offset word short".into()));
    }
    if w[..24].iter().any(|&b| b != 0) {
        return Err(RpcError::Decode("lens offset > usize".into()));
    }
    Ok(u64::from_be_bytes(w[24..32].try_into().expect("8")) as usize)
}

fn read_u256(w: &[u8]) -> U256 {
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&w[0..32]);
    U256::from_be_bytes(buf)
}

fn decode_i32_word(w: &[u8]) -> Result<i32, RpcError> {
    if w.len() < 32 {
        return Err(RpcError::Decode("lens i32 word short".into()));
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&w[28..32]);
    Ok(i32::from_be_bytes(bytes))
}

fn decode_i32_array(resp: &[u8], offset: usize) -> Result<Vec<i32>, RpcError> {
    if offset.checked_add(32).is_none_or(|e| e > resp.len()) {
        return Err(RpcError::Decode("i32[] offset OOB".into()));
    }
    let len = read_offset(&resp[offset..offset + 32])?;
    let elements_start = offset + 32;
    let elements_end = elements_start
        .checked_add(
            len.checked_mul(32)
                .ok_or_else(|| RpcError::Decode("len*32".into()))?,
        )
        .ok_or_else(|| RpcError::Decode("elements end".into()))?;
    if elements_end > resp.len() {
        return Err(RpcError::Decode("i32[] truncated".into()));
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let w = &resp[elements_start + i * 32..elements_start + (i + 1) * 32];
        out.push(decode_i32_word(w)?);
    }
    Ok(out)
}

fn decode_tick_states(
    resp: &[u8],
    offset: usize,
) -> Result<(Vec<u128>, Vec<u128>, Vec<u128>), RpcError> {
    if offset.checked_add(32).is_none_or(|e| e > resp.len()) {
        return Err(RpcError::Decode("tickStates offset OOB".into()));
    }
    let len = read_offset(&resp[offset..offset + 32])?;
    // Each TickState tuple is 4 × 32 = 128 bytes.
    let elements_start = offset + 32;
    let elements_end = elements_start
        .checked_add(
            len.checked_mul(128)
                .ok_or_else(|| RpcError::Decode("len*128".into()))?,
        )
        .ok_or_else(|| RpcError::Decode("tickStates end".into()))?;
    if elements_end > resp.len() {
        return Err(RpcError::Decode("tickStates truncated".into()));
    }
    let mut ra: Vec<u128> = Vec::with_capacity(len);
    let mut rb: Vec<u128> = Vec::with_capacity(len);
    let mut ts: Vec<u128> = Vec::with_capacity(len);
    for i in 0..len {
        let base = elements_start + i * 128;
        ra.push(read_u128_low(&resp[base..base + 32]));
        rb.push(read_u128_low(&resp[base + 32..base + 64]));
        ts.push(read_u128_low(&resp[base + 64..base + 96]));
        // word[3] is `uint32[4] binIdsByTick` — engine doesn't need it.
    }
    Ok((ra, rb, ts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_u128(v: u128) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[16..32].copy_from_slice(&v.to_be_bytes());
        w
    }
    fn word_i32(v: i32) -> [u8; 32] {
        encode_i32_word(v)
    }
    fn word_u256(v: u64) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[24..32].copy_from_slice(&v.to_be_bytes());
        w
    }

    #[test]
    fn selectors_match_verified_signatures() {
        // Lock the selectors so a typo can't silently re-introduce the bogus-function bug.
        assert_eq!(
            get_ticks_around_active_selector(),
            selector(b"getTicksAroundActive(address,int32)")
        );
        assert_eq!(
            get_tick_sqrt_price_and_l_selector(),
            selector(b"getTickSqrtPriceAndL(address,int32)")
        );
        assert_eq!(get_state_selector(), selector(b"getState()"));
        // And specifically NOT the old bogus one.
        assert_ne!(
            get_ticks_around_active_selector(),
            selector(b"getTicksAroundActiveWLiquidity(address,int32)")
        );
    }

    #[test]
    fn decode_get_state_extracts_all_fields() {
        // Build a 9-word State tuple. activeTick = -57 (negative, sign-extended) so we prove the
        // i32 decode handles the case that produced "2.06e9 garbage" before.
        let mut resp = Vec::new();
        resp.extend_from_slice(&word_u128(1_000_000_000_000_000_000)); // reserveA
        resp.extend_from_slice(&word_u128(2_500_000_000)); // reserveB
        resp.extend_from_slice(&[0u8; 32]); // lastTwaD8 (ignored)
        resp.extend_from_slice(&[0u8; 32]); // lastLogPriceD8 (ignored)
        resp.extend_from_slice(&word_u256(1_716_000_000)); // lastTimestamp (uint40)
        resp.extend_from_slice(&word_i32(-57)); // activeTick
        let mut w_locked = [0u8; 32];
        w_locked[31] = 1;
        resp.extend_from_slice(&w_locked); // isLocked = true
        resp.extend_from_slice(&word_u256(42)); // binCounter
        let mut w_pfr = [0u8; 32];
        w_pfr[31] = 7;
        resp.extend_from_slice(&w_pfr); // protocolFeeRatioD3 = 7

        let s = decode_get_state(&resp).unwrap();
        assert_eq!(s.reserve_a, 1_000_000_000_000_000_000);
        assert_eq!(s.reserve_b, 2_500_000_000);
        assert_eq!(s.last_timestamp, 1_716_000_000);
        assert_eq!(
            s.active_tick, -57,
            "negative activeTick must sign-extend (was the garbage bug)"
        );
        assert!(s.is_locked);
        assert_eq!(s.bin_counter, 42);
        assert_eq!(s.protocol_fee_ratio_d3, 7);
    }

    #[test]
    fn decode_get_state_rejects_short() {
        assert!(decode_get_state(&[0u8; 100]).is_err());
    }

    #[test]
    fn decode_tick_sqrt_price_and_l_extracts_pair() {
        let mut resp = Vec::new();
        resp.extend_from_slice(&word_u256(1_500_000_000_000_000_000u64));
        resp.extend_from_slice(&word_u256(9_000_000_000u64));
        let (sp, l) = decode_tick_sqrt_price_and_l(&resp).unwrap();
        assert_eq!(sp, U256::from(1_500_000_000_000_000_000u64));
        assert_eq!(l, U256::from(9_000_000_000u64));
        assert!(decode_tick_sqrt_price_and_l(&[0u8; 32]).is_err());
    }

    #[test]
    fn decode_get_ticks_around_active_two_tick_window() {
        // Build (int32[] ticks, TickState[] tickStates) with 2 ticks.
        // Head: 2 offset words. ticks[] at 0x40, tickStates[] after ticks payload.
        let mut resp = Vec::new();
        // head
        resp.extend_from_slice(&word_u256(0x40)); // off_ticks = 64
                                                  // tickStates offset: 64 + (32 len + 2*32 elems) = 64 + 96 = 160
        resp.extend_from_slice(&word_u256(0xa0)); // off_tick_states = 160
                                                  // ticks[] payload @64: len=2, then -3, 5
        resp.extend_from_slice(&word_u256(2));
        resp.extend_from_slice(&word_i32(-3));
        resp.extend_from_slice(&word_i32(5));
        // tickStates[] payload @160: len=2, then two 128-byte TickStates
        resp.extend_from_slice(&word_u256(2));
        // tick0: reserveA=100, reserveB=200, totalSupply=300, binIds (zeroed)
        resp.extend_from_slice(&word_u128(100));
        resp.extend_from_slice(&word_u128(200));
        resp.extend_from_slice(&word_u128(300));
        resp.extend_from_slice(&[0u8; 32]);
        // tick1: reserveA=400, reserveB=500, totalSupply=600
        resp.extend_from_slice(&word_u128(400));
        resp.extend_from_slice(&word_u128(500));
        resp.extend_from_slice(&word_u128(600));
        resp.extend_from_slice(&[0u8; 32]);

        let (ticks, ra, rb, ts) = decode_get_ticks_around_active(&resp).unwrap();
        assert_eq!(ticks, vec![-3, 5]);
        assert_eq!(ra, vec![100, 400]);
        assert_eq!(rb, vec![200, 500]);
        assert_eq!(ts, vec![300, 600]);
    }

    #[test]
    fn decode_get_ticks_around_active_rejects_length_mismatch() {
        // ticks len=2 but tickStates len=1 → mismatch error.
        let mut resp = Vec::new();
        resp.extend_from_slice(&word_u256(0x40));
        resp.extend_from_slice(&word_u256(0xa0));
        resp.extend_from_slice(&word_u256(2)); // ticks len 2
        resp.extend_from_slice(&word_i32(1));
        resp.extend_from_slice(&word_i32(2));
        resp.extend_from_slice(&word_u256(1)); // tickStates len 1
        resp.extend_from_slice(&word_u128(1));
        resp.extend_from_slice(&word_u128(2));
        resp.extend_from_slice(&word_u128(3));
        resp.extend_from_slice(&[0u8; 32]);
        assert!(decode_get_ticks_around_active(&resp).is_err());
    }

    #[test]
    fn encode_helpers_roundtrip() {
        // address word: low 20 bytes.
        let a: Address = [0xab; 20];
        let w = encode_address_word(&a);
        assert_eq!(&w[12..32], &a[..]);
        assert!(w[..12].iter().all(|&b| b == 0));
        // i32 word: negative sign-extends.
        let neg = encode_i32_word(-1);
        assert!(neg.iter().all(|&b| b == 0xff));
        let pos = encode_i32_word(500);
        assert_eq!(decode_i32_word(&pos).unwrap(), 500);
    }
}
