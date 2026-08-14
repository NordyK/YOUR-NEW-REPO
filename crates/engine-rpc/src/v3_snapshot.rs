//! Uniswap V3 (and Algebra-compatible) pool state snapshots.
//!
//! Reads three pool fields the engine needs:
//! * `slot0() -> (uint160 sqrtPriceX96, int24 tick, uint16 obsIndex, uint16 obsCardinality,
//!                uint16 obsCardinalityNext, uint8 feeProtocol, bool unlocked)`
//!   Selector `0x3850c7bd`.
//! * `liquidity() -> uint128`. Selector `0x1a686502`.
//! * `ticks(int24 tick) -> (uint128 liquidityGross, int128 liquidityNet, ...)`.
//!   Selector `0xf30dba93`.

use alloy_primitives::U256;

use engine_feed::Address;

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

const SLOT0_SELECTOR: [u8; 4] = [0x38, 0x50, 0xc7, 0xbd];
const LIQUIDITY_SELECTOR: [u8; 4] = [0x1a, 0x68, 0x65, 0x02];
const TICKS_SELECTOR: [u8; 4] = [0xf3, 0x0d, 0xba, 0x93];
/// `keccak256("globalState()")[0..4]` — Algebra V1.9 (Camelot V3) replacement for
/// Uniswap V3's `slot0()`. Returns `(uint160 price, int24 tick, uint16 lastFee,
/// uint16 timepointIndex, uint8 communityFeeToken0, uint8 communityFeeToken1, bool unlocked)`,
/// ABI-encoded to 7 × 32 bytes = 224 bytes.
const GLOBAL_STATE_SELECTOR: [u8; 4] = [0xe7, 0x6c, 0x01, 0xe4];

/// [row-7] Getters for the shadow post-state of an Algebra (Camelot V3) pool: `globalState()` +
/// `liquidity()`. Feed the raw returns (in this order) to [`decode_algebra_shadow`].
pub fn algebra_shadow_getters() -> [Vec<u8>; 2] {
    [GLOBAL_STATE_SELECTOR.to_vec(), LIQUIDITY_SELECTOR.to_vec()]
}

/// [row-7] Decode Algebra `globalState()` (+ optional `liquidity()`) raw returns into
/// `(sqrtPriceX96, tick, fee_pips, Option<liquidity>)` — the same fields `fetch_algebra_global_state`
/// reads, for the shadow post-state path (byte-identical decode).
pub fn decode_algebra_shadow(
    global_state: &[u8],
    liquidity: Option<&[u8]>,
) -> Option<(U256, i32, u32, Option<u128>)> {
    if global_state.len() < 96 {
        return None;
    }
    let sqrt_price_x96 = U256::from_be_bytes(<[u8; 32]>::try_from(&global_state[0..32]).ok()?);
    let mut tick_buf = [0u8; 4];
    tick_buf[1..4].copy_from_slice(&global_state[61..64]);
    let tick = sign_extend_i24(u32::from_be_bytes(tick_buf));
    let mut fee_buf = [0u8; 4];
    fee_buf[2..4].copy_from_slice(&global_state[94..96]);
    let raw_fee = u32::from_be_bytes(fee_buf);
    let fee_pips = if raw_fee == 0 || raw_fee > 100_000 {
        3_000
    } else {
        raw_fee
    };
    let liq = liquidity.and_then(|l| {
        (l.len() >= 32).then(|| {
            let mut b = [0u8; 16];
            b.copy_from_slice(&l[16..32]);
            u128::from_be_bytes(b)
        })
    });
    Some((sqrt_price_x96, tick, fee_pips, liq))
}

/// V3 `slot0` snapshot.
#[derive(Debug, Clone, Copy)]
pub struct V3Slot0Snapshot {
    /// sqrtPriceX96.
    pub sqrt_price_x96: U256,
    /// Current tick.
    pub tick: i32,
}

/// V3 `ticks(int24)` snapshot for one tick.
#[derive(Debug, Clone, Copy)]
pub struct V3TickSnapshot {
    /// Tick index.
    pub tick: i32,
    /// liquidityGross.
    pub liquidity_gross: u128,
    /// liquidityNet (signed).
    pub liquidity_net: i128,
}

/// Fetch slot0 for a V3 pool.
pub fn fetch_v3_slot0(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<V3Slot0Snapshot, RpcError> {
    let resp = client.eth_call(pool, &SLOT0_SELECTOR, block)?;
    if resp.len() < 64 {
        return Err(RpcError::Decode("v3 slot0 response too short".into()));
    }
    let mut sqrt_buf = [0u8; 32];
    sqrt_buf.copy_from_slice(&resp[0..32]);
    let sqrt_price_x96 = U256::from_be_bytes(sqrt_buf);
    // tick: int24 right-aligned in word[32..64]; sign-extend from 24 bits.
    let mut tick_buf = [0u8; 4];
    tick_buf[1..4].copy_from_slice(&resp[61..64]);
    let raw_tick = u32::from_be_bytes(tick_buf);
    let tick = sign_extend_i24(raw_tick);
    Ok(V3Slot0Snapshot {
        sqrt_price_x96,
        tick,
    })
}

/// Algebra V1.9 (Camelot V3) snapshot from `globalState()`.
///
/// Returns the same fields the engine needs from a V3 pool (`sqrt_price_x96`, `tick`)
/// plus the live `fee` value (in 1e-6 units). The fee is recorded so the per-route
/// evaluator can use the up-to-date dynamic fee instead of a stale config value.
#[derive(Debug, Clone, Copy)]
pub struct AlgebraGlobalStateSnapshot {
    /// `price` field — equivalent to V3's `sqrtPriceX96`.
    pub sqrt_price_x96: U256,
    /// Current tick.
    pub tick: i32,
    /// Live fee, units 1e-6 (hundredths of a bip). 3000 = 30 bps. Algebra fees are
    /// dynamic; this is the value the plugin set on the most recent swap.
    pub fee_pips: u32,
}

/// Fetch the `globalState()` view from an Algebra V1.9 pool (Camelot V3).
pub fn fetch_algebra_global_state(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<AlgebraGlobalStateSnapshot, RpcError> {
    let resp = client.eth_call(pool, &GLOBAL_STATE_SELECTOR, block)?;
    // ABI-encodes 7 fields × 32 bytes = 224 bytes.
    if resp.len() < 96 {
        return Err(RpcError::Decode(
            "algebra globalState response too short".into(),
        ));
    }
    // word[0..32]: price (uint160, right-aligned in a uint256 slot).
    let mut sqrt_buf = [0u8; 32];
    sqrt_buf.copy_from_slice(&resp[0..32]);
    let sqrt_price_x96 = U256::from_be_bytes(sqrt_buf);
    // word[32..64]: tick (int24, right-aligned, sign-extended into the high bytes).
    let mut tick_buf = [0u8; 4];
    tick_buf[1..4].copy_from_slice(&resp[61..64]);
    let raw_tick = u32::from_be_bytes(tick_buf);
    let tick = sign_extend_i24(raw_tick);
    // word[64..96]: fee (uint16, right-aligned). Treat values > 100_000 as anomalous
    // and clamp to 30 bps to avoid sending nonsense to the evaluator.
    let mut fee_buf = [0u8; 4];
    fee_buf[2..4].copy_from_slice(&resp[94..96]);
    let raw_fee = u32::from_be_bytes(fee_buf);
    let fee_pips = if raw_fee == 0 || raw_fee > 100_000 {
        3_000
    } else {
        raw_fee
    };
    Ok(AlgebraGlobalStateSnapshot {
        sqrt_price_x96,
        tick,
        fee_pips,
    })
}

/// Camelot Algebra V1.9 `tickSpacing()` — MUTABLE at runtime (admin `setTickSpacing`), unlike
/// Uniswap V3 where spacing is immutable per fee tier. Discovery records the CREATION-time
/// value; a pool re-spaced later (observed live 2026-07-10: WETH/USDC `0xb1026b8e…` created at
/// 60, live at 10) has real initialized ticks OFF the stale grid — a spacing-stepped tick walk
/// misses them and the kernel over-quotes as soon as a swap crosses the first missed tick
/// (exactly the 28/220 size-growing Algebra parity failures; small non-crossing swaps stayed
/// byte-exact). Always read the LIVE value at snapshot time.
pub fn fetch_algebra_tick_spacing(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<i32, RpcError> {
    // tickSpacing() selector 0xd0c93a7c (verified live against the re-spaced Camelot pool).
    const SEL: [u8; 4] = [0xd0, 0xc9, 0x3a, 0x7c];
    let resp = client.eth_call(pool, &SEL, block)?;
    decode_tick_spacing(&resp)
}

/// Decode + plausibility-check a `tickSpacing()` return blob (int24, right-aligned).
fn decode_tick_spacing(resp: &[u8]) -> Result<i32, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode("tickSpacing response too short".into()));
    }
    let mut raw = [0u8; 4];
    raw[1..4].copy_from_slice(&resp[29..32]);
    let spacing = sign_extend_i24(u32::from_be_bytes(raw));
    if spacing <= 0 || spacing > 16_384 {
        return Err(RpcError::Decode(format!("implausible tickSpacing {spacing}")));
    }
    Ok(spacing)
}

#[cfg(test)]
mod tick_spacing_tests {
    use super::*;

    /// The live Camelot case (0x…0a = 10), a bigger legal value, and the fail-closed rejects
    /// (zero, negative via sign extension, short blob) — a wrong grid must never decode quietly.
    #[test]
    fn decode_tick_spacing_is_fail_closed() {
        let word = |v: u32| {
            let mut b = [0u8; 32];
            b[29..32].copy_from_slice(&v.to_be_bytes()[1..4]);
            b
        };
        assert_eq!(decode_tick_spacing(&word(10)).unwrap(), 10);
        assert_eq!(decode_tick_spacing(&word(60)).unwrap(), 60);
        assert_eq!(decode_tick_spacing(&word(200)).unwrap(), 200);
        assert!(decode_tick_spacing(&word(0)).is_err(), "zero spacing");
        assert!(
            decode_tick_spacing(&word(0x00FF_FFF6)).is_err(),
            "negative (-10) via int24 sign extension"
        );
        assert!(decode_tick_spacing(&[0u8; 16]).is_err(), "short blob");
        assert!(decode_tick_spacing(&word(20_000)).is_err(), "implausibly wide");
    }
}

/// Fetch active liquidity for a V3 pool.
pub fn fetch_v3_liquidity(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<u128, RpcError> {
    let resp = client.eth_call(pool, &LIQUIDITY_SELECTOR, block)?;
    if resp.len() < 32 {
        return Err(RpcError::Decode("v3 liquidity response too short".into()));
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&resp[16..32]);
    Ok(u128::from_be_bytes(buf))
}

/// Fetch a window of initialised ticks around `centre_tick` aligned to `tick_spacing`. The
/// window walks `±width_ticks` from the current tick, calls `ticks(int24)` for each
/// candidate, and keeps only those with non-zero `liquidityGross` (i.e. initialised).
///
/// `width_ticks` should be sized so the warm window covers the largest plausible input
/// amount on the route (plan §10.3 V3 fail-closed rule). For Arbitrum production an
/// initial value of ±100 tick-spacings (~1% price range for 60-spacing pools) is
/// reasonable; the background-core score recompute can widen the window as needed.
pub fn fetch_v3_tick_window(
    client: &ArbitrumRpcClient,
    pool: &Address,
    centre_tick: i32,
    tick_spacing: i32,
    width_ticks: i32,
    block: BlockTag,
) -> Result<Vec<V3TickSnapshot>, RpcError> {
    if tick_spacing <= 0 {
        return Err(RpcError::Decode("non-positive tick spacing".into()));
    }
    // Align centre to spacing.
    let aligned = centre_tick - centre_tick.rem_euclid(tick_spacing);
    let half = width_ticks * tick_spacing;
    let lo = aligned - half;
    let hi = aligned + half;
    let mut out = Vec::with_capacity((2 * width_ticks as usize) + 1);
    let mut t = lo;
    while t <= hi {
        match fetch_v3_tick(client, pool, t, block) {
            Ok(snap) => {
                if snap.liquidity_gross != 0 {
                    out.push(snap);
                }
            }
            Err(_) => {
                // Non-existent ticks return zero gross; tolerate transient RPC errors
                // (we get retried by the caller).
            }
        }
        t = t.saturating_add(tick_spacing);
    }
    Ok(out)
}

/// Fetch one initialised tick.
pub fn fetch_v3_tick(
    client: &ArbitrumRpcClient,
    pool: &Address,
    tick: i32,
    block: BlockTag,
) -> Result<V3TickSnapshot, RpcError> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&TICKS_SELECTOR);
    // int24 right-aligned in 32-byte word, sign-extended.
    let mut tick_word = [0u8; 32];
    if tick < 0 {
        tick_word.fill(0xff);
    }
    let raw = (tick as i64).to_be_bytes();
    // int24 occupies the low 3 bytes of the 32-byte word; sign-extended high bytes.
    tick_word[29..32].copy_from_slice(&raw[5..8]);
    data.extend_from_slice(&tick_word);
    let resp = client.eth_call(pool, &data, block)?;
    if resp.len() < 64 {
        return Err(RpcError::Decode("v3 ticks response too short".into()));
    }
    // liquidityGross: uint128 in word[0..32], right-aligned.
    let mut gross_buf = [0u8; 16];
    gross_buf.copy_from_slice(&resp[16..32]);
    let liquidity_gross = u128::from_be_bytes(gross_buf);
    // liquidityNet: int128 in word[32..64], right-aligned with sign extension.
    let mut net_buf = [0u8; 16];
    net_buf.copy_from_slice(&resp[48..64]);
    let liquidity_net = i128::from_be_bytes(net_buf);
    Ok(V3TickSnapshot {
        tick,
        liquidity_gross,
        liquidity_net,
    })
}

/// Multicall3-batched variant of `fetch_v3_tick_window`. Instead of `2 *
/// width_ticks + 1` separate JSON-RPC roundtrips, this packs every tick read
/// into a single `aggregate3` call against the Multicall3 contract. On
/// Arbitrum that drops snapshot phase from ~8 minutes (rate-limited by
/// Alchemy's ~25 RPS) to ~25 seconds because the per-pool tick fanout
/// collapses into one network roundtrip.
///
/// Falls back gracefully on per-tick decode errors (Multicall3 with
/// `allowFailure=true` returns `success=false` for reverted calls; we treat
/// those as uninitialised and skip them, same as `fetch_v3_tick_window`).
pub fn fetch_v3_tick_window_multicall(
    client: &ArbitrumRpcClient,
    multicall3: &Address,
    pool: &Address,
    centre_tick: i32,
    tick_spacing: i32,
    width_ticks: i32,
    block: BlockTag,
) -> Result<Vec<V3TickSnapshot>, RpcError> {
    if tick_spacing <= 0 {
        return Err(RpcError::Decode("non-positive tick spacing".into()));
    }
    let aligned = centre_tick - centre_tick.rem_euclid(tick_spacing);
    let half = width_ticks * tick_spacing;
    let lo = aligned - half;
    let hi = aligned + half;
    let mut ticks: Vec<i32> = Vec::with_capacity((2 * width_ticks as usize) + 1);
    let mut t = lo;
    while t <= hi {
        ticks.push(t);
        t = t.saturating_add(tick_spacing);
    }
    let mut calls: Vec<crate::multicall::Call3> = Vec::with_capacity(ticks.len());
    for &tick in &ticks {
        let mut data = Vec::with_capacity(4 + 32);
        data.extend_from_slice(&TICKS_SELECTOR);
        let mut tick_word = [0u8; 32];
        if tick < 0 {
            tick_word.fill(0xff);
        }
        let raw = (tick as i64).to_be_bytes();
        tick_word[29..32].copy_from_slice(&raw[5..8]);
        data.extend_from_slice(&tick_word);
        calls.push(crate::multicall::Call3 {
            target: *pool,
            allow_failure: true,
            call_data: data,
        });
    }
    let results = crate::multicall::aggregate3(client, multicall3, &calls, block)?;
    let mut out = Vec::with_capacity(ticks.len());
    for (tick, result) in ticks.iter().zip(results.iter()) {
        if !result.success || result.return_data.len() < 64 {
            // Uninitialised tick or transient failure — skip.
            continue;
        }
        let mut gross_buf = [0u8; 16];
        gross_buf.copy_from_slice(&result.return_data[16..32]);
        let liquidity_gross = u128::from_be_bytes(gross_buf);
        if liquidity_gross == 0 {
            continue;
        }
        let mut net_buf = [0u8; 16];
        net_buf.copy_from_slice(&result.return_data[48..64]);
        let liquidity_net = i128::from_be_bytes(net_buf);
        out.push(V3TickSnapshot {
            tick: *tick,
            liquidity_gross,
            liquidity_net,
        });
    }
    Ok(out)
}

/// Refresh the tick window for MANY V3 pools in ONE `aggregate3` multicall (same
/// block, one RPC). `pools` is `(pool, centre_tick, tick_spacing)`. Returns one
/// window (`Vec<V3TickSnapshot>`, initialised ticks only) per input pool, in
/// order. Used by the resync poller to refresh every pool that crossed a tick in
/// a cycle without the per-pool RPC burst that spiked sim tail latency.
pub fn fetch_v3_windows_batched(
    client: &ArbitrumRpcClient,
    multicall3: &Address,
    pools: &[(Address, i32, i32)],
    width_ticks: i32,
    block: BlockTag,
) -> Result<Vec<Vec<V3TickSnapshot>>, RpcError> {
    let mut calls: Vec<crate::multicall::Call3> = Vec::new();
    let mut plan: Vec<(usize, Vec<i32>)> = Vec::with_capacity(pools.len());
    for (pool, centre, spacing) in pools {
        let spacing = (*spacing).max(1);
        let aligned = centre - centre.rem_euclid(spacing);
        let half = width_ticks * spacing;
        let mut ticks: Vec<i32> = Vec::with_capacity((2 * width_ticks as usize) + 1);
        let mut t = aligned - half;
        while t <= aligned + half {
            ticks.push(t);
            t = t.saturating_add(spacing);
        }
        let start = calls.len();
        for &tick in &ticks {
            let mut data = Vec::with_capacity(4 + 32);
            data.extend_from_slice(&TICKS_SELECTOR);
            let mut tick_word = [0u8; 32];
            if tick < 0 {
                tick_word.fill(0xff);
            }
            let raw = (tick as i64).to_be_bytes();
            tick_word[29..32].copy_from_slice(&raw[5..8]);
            data.extend_from_slice(&tick_word);
            calls.push(crate::multicall::Call3 {
                target: *pool,
                allow_failure: true,
                call_data: data,
            });
        }
        plan.push((start, ticks));
    }
    let results = crate::multicall::aggregate3(client, multicall3, &calls, block)?;
    let mut out: Vec<Vec<V3TickSnapshot>> = Vec::with_capacity(pools.len());
    for (start, ticks) in plan {
        let mut win = Vec::new();
        for (i, &tick) in ticks.iter().enumerate() {
            let r = match results.get(start + i) {
                Some(r) => r,
                None => continue,
            };
            if !r.success || r.return_data.len() < 64 {
                continue;
            }
            let mut gross_buf = [0u8; 16];
            gross_buf.copy_from_slice(&r.return_data[16..32]);
            let liquidity_gross = u128::from_be_bytes(gross_buf);
            if liquidity_gross == 0 {
                continue;
            }
            let mut net_buf = [0u8; 16];
            net_buf.copy_from_slice(&r.return_data[48..64]);
            win.push(V3TickSnapshot {
                tick,
                liquidity_gross,
                liquidity_net: i128::from_be_bytes(net_buf),
            });
        }
        out.push(win);
    }
    Ok(out)
}

/// Sign-extend a 24-bit value packed in the low 3 bytes of a `u32` into a
/// proper `i32`. Public so sibling modules (Algebra, Kyber) can decode their
/// own `int24` returns without re-implementing the bit math.
pub fn sign_extend_i24_pub(raw: u32) -> i32 {
    sign_extend_i24(raw)
}

fn sign_extend_i24(raw: u32) -> i32 {
    let masked = raw & 0xff_ffff;
    if masked & 0x80_0000 != 0 {
        // Negative: extend the high bits with ones.
        (masked | 0xff00_0000) as i32
    } else {
        masked as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_algebra_shadow_extracts_sqrt_tick_fee_liquidity() {
        // globalState(): sqrtP @ word0, int24 tick @ word1[61..64], uint16 fee @ word2[94..96].
        let sqrt = U256::from(79_228_162_514_264_337_593_543_950_336u128) + U256::from(7u64);
        let tick: i32 = -5_000;
        let fee: u32 = 500;
        let mut gs = Vec::new();
        gs.extend_from_slice(&sqrt.to_be_bytes::<32>());
        let mut w1 = [0u8; 32];
        let raw_tick = (tick as u32) & 0x00FF_FFFF;
        w1[29..32].copy_from_slice(&raw_tick.to_be_bytes()[1..4]);
        gs.extend_from_slice(&w1);
        let mut w2 = [0u8; 32];
        w2[30..32].copy_from_slice(&(fee as u16).to_be_bytes());
        gs.extend_from_slice(&w2);
        let liq: u128 = 123_456_789;
        let mut lw = [0u8; 32];
        lw[16..32].copy_from_slice(&liq.to_be_bytes());
        let (g_sqrt, g_tick, g_fee, g_liq) = decode_algebra_shadow(&gs, Some(&lw)).unwrap();
        assert_eq!(g_sqrt, sqrt);
        assert_eq!(g_tick, tick);
        assert_eq!(g_fee, fee);
        assert_eq!(g_liq, Some(liq));
    }

    #[test]
    fn sign_extend_positive_tick() {
        assert_eq!(sign_extend_i24(0x000001), 1);
        assert_eq!(sign_extend_i24(0x7fffff), 8_388_607);
    }

    #[test]
    fn sign_extend_negative_tick() {
        // -1 in two's complement int24 = 0xffffff.
        assert_eq!(sign_extend_i24(0xffffff), -1);
        // MIN_TICK = -887272. In int24 two's complement that is
        // 2^24 - 887272 = 15889944 = 0xF27618; sign-extended to i32 = 0xFFF27618.
        assert_eq!(sign_extend_i24(0xf27618), -887_272);
    }
}
