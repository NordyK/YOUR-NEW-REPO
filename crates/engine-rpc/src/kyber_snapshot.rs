//! KyberSwap Elastic pool state snapshot reader.
//!
//! KyberSwap Elastic differs from Uniswap V3 at the ABI level:
//! * `getPoolState()` returns `(uint160 sqrtP, int24 currentTick, int24 nearestCurrentTick, bool locked)`
//!   — replaces `slot0()` (selector `0x217ac237`).
//! * `getLiquidityState()` returns `(uint128 baseL, uint128 reinvestL, uint128 reinvestLLast)`
//!   — replaces `liquidity()` (selector `0xab612f2b`).
//! * `ticks(int24)` exists with the same selector as V3 and returns the same first
//!   two fields the engine needs (`liquidityGross`, `liquidityNet`).
//!
//! We snapshot KyberSwap pools into the engine's `V3State` so the V3 evaluator
//! can quote them. The math is approximate (V3 evaluator doesn't reinvest the
//! fee delta into liquidity per swap step) but in the right ballpark for arb
//! detection.

use alloy_primitives::U256;
use engine_feed::Address;

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};
use crate::v3_snapshot::V3Slot0Snapshot;

/// `keccak256("getPoolState()")[0..4]`.
const GET_POOL_STATE_SELECTOR: [u8; 4] = [0x21, 0x7a, 0xc2, 0x37];
/// `keccak256("getLiquidityState()")[0..4]`.
const GET_LIQUIDITY_STATE_SELECTOR: [u8; 4] = [0xab, 0x61, 0x2f, 0x2b];
/// `keccak256("initializedTicks(int24)")[0..4]` — Kyber's LINKED LIST of initialized ticks:
/// `initializedTicks(t) -> (int24 prev, int24 next)`. This is the ONLY correct tick discovery
/// for Elastic: a window scan around the current tick finds nothing when the active tick sits
/// in a liquidity desert (observed live: nearest→next gap of 130,240 ticks on the WETH/USDC.e
/// pool — no window covers that).
const INITIALIZED_TICKS_SELECTOR: [u8; 4] = [0xc0, 0xac, 0x75, 0xcf];
/// `keccak256("ticks(int24)")[0..4]` — same selector as V3; word1 = `int128 liquidityNet`.
const KYBER_TICKS_SELECTOR: [u8; 4] = [0xf3, 0x0d, 0xba, 0x93];
/// Kyber Elastic list terminators (global MIN/MAX tick).
const KYBER_MIN_TICK: i32 = -887_272;
/// See [`KYBER_MIN_TICK`].
const KYBER_MAX_TICK: i32 = 887_272;

/// Convert Kyber Elastic `swapFeeUnits` (denominated in `FEE_UNITS = 1e5`) into the engine's
/// ppm (1e6) fee convention — a 10× scale. Kyber's real tiers are 8/10/40/300/1000 feeUnits
/// = 0.008%/0.01%/0.04%/0.3%/1% = 80/100/400/3000/10000 ppm. Feeding raw feeUnits as ppm
/// under-charges the fee 10× and OVER-quotes output by 0.9×fee (measured live: +3.6 bps on
/// the 40-unit pool — exactly the missing 3.6 bps of a 4 bps fee). [AUD-012]
pub fn kyber_fee_units_to_ppm(fee_units: u32) -> u32 {
    fee_units.saturating_mul(10)
}

/// Decode an int24 returned as a full int256-extended ABI word into `i32`.
fn i32_from_int_word(w: &[u8]) -> Option<i32> {
    if w.len() < 32 {
        return None;
    }
    let sign = w[0];
    if (sign != 0x00 && sign != 0xFF) || w[0..28].iter().any(|&b| b != sign) {
        return None;
    }
    Some(i32::from_be_bytes([w[28], w[29], w[30], w[31]]))
}

/// Decode `getPoolState()`'s third word — `nearestCurrentTick`, the entry point into the
/// initialized-tick linked list (an initialized tick ≤ currentTick, or the MIN sentinel).
pub fn decode_nearest_current_tick(pool_state: &[u8]) -> Option<i32> {
    if pool_state.len() < 96 {
        return None;
    }
    i32_from_int_word(&pool_state[64..96])
}

/// Decode `initializedTicks(int24) -> (int24 prev, int24 next)`.
pub fn decode_initialized_ticks_pair(resp: &[u8]) -> Option<(i32, i32)> {
    if resp.len() < 64 {
        return None;
    }
    Some((
        i32_from_int_word(&resp[0..32])?,
        i32_from_int_word(&resp[32..64])?,
    ))
}

/// Decode `ticks(int24)` word1 — `int128 liquidityNet` (word0 is `uint128 liquidityGross`).
pub fn decode_tick_liquidity_net(resp: &[u8]) -> Option<i128> {
    if resp.len() < 64 {
        return None;
    }
    let w = &resp[32..64];
    let sign = w[0];
    if (sign != 0x00 && sign != 0xFF) || w[0..16].iter().any(|&b| b != sign) {
        return None;
    }
    Some(i128::from_be_bytes(<[u8; 16]>::try_from(&w[16..32]).ok()?))
}

/// Fetch Kyber Elastic's REAL initialized ticks by walking the on-chain linked list from
/// `nearestCurrentTick`: up to `per_side` nodes below (via `prev`) and above (via `next`),
/// then their `liquidityNet` in ONE batched `ticks()` round-trip. Returns `(tick, liquidityNet)`
/// sorted ascending — exactly the tick set the Elastic swap loop crosses, regardless of how far
/// from the current tick the liquidity actually sits.
pub fn fetch_kyber_initialized_ticks(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
    per_side: usize,
) -> Result<Vec<(i32, i128)>, RpcError> {
    let ps = client.eth_call(pool, &GET_POOL_STATE_SELECTOR, block)?;
    let nearest = decode_nearest_current_tick(&ps)
        .ok_or_else(|| RpcError::Decode("kyber getPoolState nearestCurrentTick".into()))?;
    let tick_arg = |t: i32| -> Vec<u8> {
        let mut cd = INITIALIZED_TICKS_SELECTOR.to_vec();
        let mut w = if t < 0 { [0xFFu8; 32] } else { [0u8; 32] };
        w[28..32].copy_from_slice(&t.to_be_bytes());
        cd.extend_from_slice(&w);
        cd
    };
    let mut found: Vec<i32> = Vec::with_capacity(per_side * 2 + 1);
    if nearest > KYBER_MIN_TICK && nearest < KYBER_MAX_TICK {
        found.push(nearest);
    }
    // Walk UP through `next` pointers.
    let mut up = nearest;
    for _ in 0..per_side {
        let resp = client.eth_call(pool, &tick_arg(up), block)?;
        let Some((_prev, next)) = decode_initialized_ticks_pair(&resp) else {
            break;
        };
        if next <= up || next >= KYBER_MAX_TICK {
            break;
        }
        found.push(next);
        up = next;
    }
    // Walk DOWN through `prev` pointers.
    let mut down = nearest;
    for _ in 0..per_side {
        let resp = client.eth_call(pool, &tick_arg(down), block)?;
        let Some((prev, _next)) = decode_initialized_ticks_pair(&resp) else {
            break;
        };
        if prev >= down || prev <= KYBER_MIN_TICK {
            break;
        }
        found.push(prev);
        down = prev;
    }
    found.sort_unstable();
    found.dedup();
    // liquidityNet for every found tick — one batched round-trip.
    let params: Vec<serde_json::Value> = found
        .iter()
        .map(|t| {
            let mut cd = KYBER_TICKS_SELECTOR.to_vec();
            let mut w = if *t < 0 { [0xFFu8; 32] } else { [0u8; 32] };
            w[28..32].copy_from_slice(&t.to_be_bytes());
            cd.extend_from_slice(&w);
            serde_json::json!([
                {"to": crate::client::encode_address(pool), "data": crate::client::encode_hex_with_prefix(&cd)},
                block.to_json_param()
            ])
        })
        .collect();
    let calls: Vec<(&str, serde_json::Value)> =
        params.into_iter().map(|p| ("eth_call", p)).collect();
    let items = client.call_batch(&calls)?;
    let mut out: Vec<(i32, i128)> = Vec::with_capacity(found.len());
    for (t, item) in found.iter().zip(items) {
        let v = item?;
        let s = v
            .as_str()
            .ok_or_else(|| RpcError::Decode("expected hex string".into()))?;
        let raw = crate::client::decode_hex_with_prefix(s)?;
        let net = decode_tick_liquidity_net(&raw)
            .ok_or_else(|| RpcError::Decode("kyber ticks() liquidityNet".into()))?;
        out.push((*t, net));
    }
    Ok(out)
}

/// Fetch the KyberSwap-equivalent of `slot0` (sqrtPriceX96 + tick) via
/// `getPoolState()`.
pub fn fetch_kyber_pool_state(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<V3Slot0Snapshot, RpcError> {
    let resp = client.eth_call(pool, &GET_POOL_STATE_SELECTOR, block)?;
    // ABI: 4 fields × 32 bytes = 128 bytes.
    if resp.len() < 64 {
        return Err(RpcError::Decode("kyber getPoolState too short".into()));
    }
    // word[0..32]: sqrtP (uint160 padded high zero).
    let mut sqrt_buf = [0u8; 32];
    sqrt_buf.copy_from_slice(&resp[0..32]);
    let sqrt_price_x96 = U256::from_be_bytes(sqrt_buf);
    // word[32..64]: currentTick (int24 right-aligned, sign-extended).
    let mut tick_buf = [0u8; 4];
    tick_buf[1..4].copy_from_slice(&resp[61..64]);
    let raw_tick = u32::from_be_bytes(tick_buf);
    let tick = crate::v3_snapshot::sign_extend_i24_pub(raw_tick);
    Ok(V3Slot0Snapshot {
        sqrt_price_x96,
        tick,
    })
}

/// [row-7] Getters for the shadow post-state of a Kyber Elastic pool: `getPoolState()` +
/// `getLiquidityState()`. Feed the raw returns (in this order) to [`decode_kyber_shadow`].
pub fn kyber_shadow_getters() -> [Vec<u8>; 2] {
    [
        GET_POOL_STATE_SELECTOR.to_vec(),
        GET_LIQUIDITY_STATE_SELECTOR.to_vec(),
    ]
}

/// [row-7] Decode Kyber `getPoolState()` (+ optional `getLiquidityState()`) into
/// `(sqrtPriceX96, tick, Option<total_liquidity>)` — sqrt/tick per `fetch_kyber_pool_state`, and the
/// TOTAL active liquidity (`baseL + reinvestL`) the Kyber kernel prices against, for the shadow
/// post-state path.
pub fn decode_kyber_shadow(
    pool_state: &[u8],
    liq: Option<&[u8]>,
) -> Option<(U256, i32, Option<u128>)> {
    if pool_state.len() < 64 {
        return None;
    }
    let sqrt = U256::from_be_bytes(<[u8; 32]>::try_from(&pool_state[0..32]).ok()?);
    let mut tick_buf = [0u8; 4];
    tick_buf[1..4].copy_from_slice(&pool_state[61..64]);
    let tick = crate::v3_snapshot::sign_extend_i24_pub(u32::from_be_bytes(tick_buf));
    let liquidity = liq.and_then(|l| decode_kyber_total_liquidity(l).ok());
    Some((sqrt, tick, liquidity))
}

/// Fetch the active liquidity from `getLiquidityState()`. Returns the **baseL**
/// component — `reinvestL` is implicit in the fee-reinvestment swap math
/// (which our V3 evaluator doesn't model precisely), and `reinvestLLast` is
/// historical accounting.
pub fn fetch_kyber_liquidity(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<u128, RpcError> {
    let resp = client.eth_call(pool, &GET_LIQUIDITY_STATE_SELECTOR, block)?;
    if resp.len() < 32 {
        return Err(RpcError::Decode("kyber getLiquidityState too short".into()));
    }
    // word[0..32]: baseL (uint128 right-aligned in 32-byte word).
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&resp[16..32]);
    Ok(u128::from_be_bytes(buf))
}

/// Decode `getLiquidityState() -> (uint128 baseL, uint128 reinvestL, uint128 reinvestLLast)` and
/// return **`baseL + reinvestL`** — the TOTAL active liquidity Kyber Elastic prices a swap against
/// (reinvested fees are live liquidity). Feeding only `baseL` (as the V3 path does) understates
/// liquidity and mis-prices — this is the audit's root cause. Used by the gated Kyber kernel path.
fn decode_kyber_total_liquidity(resp: &[u8]) -> Result<u128, RpcError> {
    if resp.len() < 64 {
        return Err(RpcError::Decode("kyber getLiquidityState too short".into()));
    }
    let mut b = [0u8; 16];
    b.copy_from_slice(&resp[16..32]);
    let base_l = u128::from_be_bytes(b);
    b.copy_from_slice(&resp[48..64]); // reinvestL (2nd uint128 word, right-aligned)
    let reinvest_l = u128::from_be_bytes(b);
    Ok(base_l.saturating_add(reinvest_l))
}

/// Fetch Kyber Elastic's TOTAL active liquidity (`baseL + reinvestL`) — what the Kyber kernel must
/// price against (vs the `baseL`-only `fetch_kyber_liquidity` the legacy V3 path used).
pub fn fetch_kyber_liquidity_total(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<u128, RpcError> {
    let resp = client.eth_call(pool, &GET_LIQUIDITY_STATE_SELECTOR, block)?;
    decode_kyber_total_liquidity(&resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_kyber_shadow_extracts_sqrt_tick_total_liquidity() {
        let sqrt = U256::from(79_228_162_514_264_337_593_543_950_336u128) + U256::from(11u64);
        let tick: i32 = 2_222;
        let mut ps = Vec::new();
        ps.extend_from_slice(&sqrt.to_be_bytes::<32>());
        let mut w1 = [0u8; 32];
        let raw_tick = (tick as u32) & 0x00FF_FFFF;
        w1[29..32].copy_from_slice(&raw_tick.to_be_bytes()[1..4]);
        ps.extend_from_slice(&w1);
        // getLiquidityState(): baseL @ word0[16..32], reinvestL @ word1[16..32].
        let (base_l, reinvest_l): (u128, u128) = (1_000_000, 250_000);
        let mut ls = Vec::new();
        let mut lw0 = [0u8; 32];
        lw0[16..32].copy_from_slice(&base_l.to_be_bytes());
        let mut lw1 = [0u8; 32];
        lw1[16..32].copy_from_slice(&reinvest_l.to_be_bytes());
        ls.extend_from_slice(&lw0);
        ls.extend_from_slice(&lw1);
        let (g_sqrt, g_tick, g_liq) = decode_kyber_shadow(&ps, Some(&ls)).unwrap();
        assert_eq!(g_sqrt, sqrt);
        assert_eq!(g_tick, tick);
        assert_eq!(g_liq, Some(base_l + reinvest_l));
    }

    #[test]
    fn total_liquidity_sums_basel_and_reinvestl() {
        // getLiquidityState() returns 3 uint128 words: baseL=1000, reinvestL=250, reinvestLLast=…
        let mut resp = [0u8; 96];
        resp[16..32].copy_from_slice(&1000u128.to_be_bytes());
        resp[48..64].copy_from_slice(&250u128.to_be_bytes());
        resp[80..96].copy_from_slice(&999u128.to_be_bytes()); // reinvestLLast, ignored
        assert_eq!(decode_kyber_total_liquidity(&resp).unwrap(), 1250);
    }

    #[test]
    fn total_liquidity_rejects_short_response() {
        assert!(decode_kyber_total_liquidity(&[0u8; 32]).is_err());
    }

    /// int256-extended ABI word for a signed value (test helper).
    fn int_word(v: i64) -> [u8; 32] {
        let mut w = if v < 0 { [0xFFu8; 32] } else { [0u8; 32] };
        w[24..32].copy_from_slice(&v.to_be_bytes());
        w
    }

    // Real vectors captured live from pool 0xdb0f…c949 @ block 465,686,728 (2026-07-02 cast):
    // getPoolState → nearestCurrentTick = -346584;
    // initializedTicks(-346584) → (prev=-346992, next=-216344)  [a 130,240-tick gap!];
    // ticks(-346584) → liquidityGross=620283703625806, liquidityNet=-620283703625806.

    #[test]
    fn fee_units_convert_to_ppm_ten_x() {
        // Kyber's real fee tiers (FEE_UNITS = 1e5) → engine ppm (1e6).
        assert_eq!(kyber_fee_units_to_ppm(8), 80); // 0.008%
        assert_eq!(kyber_fee_units_to_ppm(10), 100); // 0.01%
        assert_eq!(kyber_fee_units_to_ppm(40), 400); // 0.04% — the live parity pool
        assert_eq!(kyber_fee_units_to_ppm(300), 3_000); // 0.3%
        assert_eq!(kyber_fee_units_to_ppm(1000), 10_000); // 1%
        assert_eq!(kyber_fee_units_to_ppm(u32::MAX), u32::MAX, "saturating");
    }

    #[test]
    fn decode_nearest_current_tick_real_vector() {
        let mut ps = Vec::new();
        ps.extend_from_slice(&U256::from(2_796_324_898_253_065_743_593u128).to_be_bytes::<32>());
        ps.extend_from_slice(&int_word(-343_208));
        ps.extend_from_slice(&int_word(-346_584));
        ps.extend_from_slice(&[0u8; 32]); // locked = false
        assert_eq!(decode_nearest_current_tick(&ps), Some(-346_584));
        assert_eq!(decode_nearest_current_tick(&ps[..64]), None, "needs word 2");
    }

    #[test]
    fn decode_initialized_ticks_pair_real_vector() {
        let mut resp = Vec::new();
        resp.extend_from_slice(&int_word(-346_992));
        resp.extend_from_slice(&int_word(-216_344));
        assert_eq!(
            decode_initialized_ticks_pair(&resp),
            Some((-346_992, -216_344)),
            "prev/next straddle a 130k-tick liquidity desert — the linked list is the only loader that sees this"
        );
        assert_eq!(decode_initialized_ticks_pair(&resp[..32]), None);
    }

    #[test]
    fn decode_tick_liquidity_net_real_vector() {
        let mut resp = Vec::new();
        let gross: u128 = 620_283_703_625_806;
        let mut w0 = [0u8; 32];
        w0[16..32].copy_from_slice(&gross.to_be_bytes());
        resp.extend_from_slice(&w0);
        resp.extend_from_slice(&int_word(-620_283_703_625_806));
        resp.extend_from_slice(&[0u8; 64]); // feeGrowthOutside + secondsPerLiquidityOutside
        assert_eq!(decode_tick_liquidity_net(&resp), Some(-620_283_703_625_806));
        // Positive net decodes too.
        let mut resp2 = Vec::new();
        resp2.extend_from_slice(&w0);
        resp2.extend_from_slice(&int_word(42));
        assert_eq!(decode_tick_liquidity_net(&resp2), Some(42));
        assert_eq!(decode_tick_liquidity_net(&resp2[..32]), None);
    }
}
