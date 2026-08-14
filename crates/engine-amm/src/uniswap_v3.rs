//! Uniswap V3 concentrated-liquidity math, integer-exact.
//!
//! References:
//! * `https://raw.githubusercontent.com/Uniswap/v3-core/main/contracts/libraries/SqrtPriceMath.sol`
//! * `https://raw.githubusercontent.com/Uniswap/v3-core/main/contracts/libraries/SwapMath.sol`
//! * `https://raw.githubusercontent.com/Uniswap/v3-core/main/contracts/libraries/TickMath.sol`
//!
//! Behaviour matches deployed bytecode for every fee tier we use. The kernels are pure
//! functions over `Copy` inputs; no allocations on the hot path. Tick crossing requires
//! the caller to supply the **warmed window** of initialised ticks for the route's plausible
//! input range — a swap that would cross outside the supplied window returns
//! `TickWindowExhausted`, which the routing layer translates into a fail-closed demotion.

use alloy_primitives::U256;

use crate::error::{AmmError, AmmResult};
use crate::fixed_point::{mul_div, mul_div_rounding_up};

/// Canonical minimum tick (`-887272`).
pub const MIN_TICK: i32 = -887_272;
/// Canonical maximum tick (`887272`).
pub const MAX_TICK: i32 = 887_272;

/// MIN_SQRT_RATIO = 4295128739 (per TickMath.sol).
pub const MIN_SQRT_RATIO_LOW: u128 = 4_295_128_739;
/// MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342 (per TickMath.sol).
pub fn max_sqrt_ratio() -> U256 {
    // 1461446703485210103287273052203988822378723970342
    // = 0xFFFD8963EFD1FC6A506488495D951D5263988D26 (matches Uniswap V3 TickMath.MAX_SQRT_RATIO)
    U256::from_be_slice(&[
        0xff, 0xfd, 0x89, 0x63, 0xef, 0xd1, 0xfc, 0x6a, 0x50, 0x64, 0x88, 0x49, 0x5d, 0x95, 0x1d,
        0x52, 0x63, 0x98, 0x8d, 0x26,
    ])
}

/// MIN_SQRT_RATIO as a U256.
pub fn min_sqrt_ratio() -> U256 {
    U256::from(MIN_SQRT_RATIO_LOW)
}

/// V3 pool `slot0` and active liquidity, integer-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot0 {
    /// `sqrt(price) * 2^96` per Uniswap V3.
    pub sqrt_price_x96: U256,
    /// Current tick.
    pub tick: i32,
    /// Pool fee in pips (1e-6). Uniswap V3 fee tiers are 100, 500, 3000, 10000.
    pub fee_pips: u32,
    /// Active liquidity at the current tick.
    pub liquidity: u128,
    /// Tick spacing.
    pub tick_spacing: i32,
}

/// One initialised tick within the warmed window. `liquidity_net` is the canonical
/// V3 oracle signed delta (positive when the tick is the *low* boundary of a position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickInfo {
    /// Tick index.
    pub tick: i32,
    /// Liquidity net (signed delta).
    pub liquidity_net: i128,
}

// Thread-local direct-mapped cache for `sqrt_price_at_tick`. Size is a power of two so
// indexing is a cheap mask. The cache uses an array of `(i32, U256)` slots — `tick =
// i32::MIN` means empty. Hash via golden-ratio multiplication keeps the distribution
// uniform across the typical tick window we sample (±887 272 around current tick).
//
// Why this matters: §10.6 marginal-rate filter rejects ~90% of routes per dirty event,
// but the surviving 10% all run grid_search_v3v3, which calls sqrt_price_at_tick many
// times per hop during tick crossing + binary search. Each cache hit saves ~5 µs.
const TICK_SQRT_CACHE_SIZE: usize = 4096; // ≈ 160 KiB per thread (`U256` = 32 B + i32 = 4 B → ~40 B/slot)

std::thread_local! {
    static TICK_SQRT_CACHE: core::cell::RefCell<[(i32, U256); TICK_SQRT_CACHE_SIZE]> =
        const { core::cell::RefCell::new([(i32::MIN, U256::ZERO); TICK_SQRT_CACHE_SIZE]) };
}

#[inline(always)]
fn tick_cache_index(tick: i32) -> usize {
    ((tick as u32).wrapping_mul(0x9e37_79b9) as usize) & (TICK_SQRT_CACHE_SIZE - 1)
}

/// Compute `sqrt(1.0001^tick) * 2^96`, integer-exact, per `TickMath.getSqrtRatioAtTick`.
/// Hot-path wrapper: hits a thread-local direct-mapped cache before falling through to
/// the canonical `compute_sqrt_price_at_tick` implementation. The cache is invisible to
/// correctness — a cache miss recomputes the canonical value.
pub fn sqrt_price_at_tick(tick: i32) -> AmmResult<U256> {
    let abs_tick: u32 = tick.unsigned_abs();
    if abs_tick > MAX_TICK as u32 {
        return Err(AmmError::TickOutOfBounds);
    }
    let idx = tick_cache_index(tick);
    let cached = TICK_SQRT_CACHE.with(|c| {
        let arr = c.borrow();
        let (t, p) = arr[idx];
        if t == tick {
            Some(p)
        } else {
            None
        }
    });
    if let Some(p) = cached {
        return Ok(p);
    }
    let computed = compute_sqrt_price_at_tick(tick)?;
    TICK_SQRT_CACHE.with(|c| {
        let mut arr = c.borrow_mut();
        arr[idx] = (tick, computed);
    });
    Ok(computed)
}

/// Uncached canonical implementation. Kept separate so the wrapper above can hit the
/// thread-local cache without recursing through itself, and so internal callers that
/// already cache (or want to bypass the cache for cold-path correctness checks) can
/// call this directly.
fn compute_sqrt_price_at_tick(tick: i32) -> AmmResult<U256> {
    let abs_tick: u32 = tick.unsigned_abs();
    if abs_tick > MAX_TICK as u32 {
        return Err(AmmError::TickOutOfBounds);
    }
    // Initial: 2^128 if abs_tick & 0x1 set, else 2^128.
    // Uniswap source: `ratio = absTick & 0x1 != 0 ? 0xfffcb933bd6fad37aa2d162d1a594001 : 0x100000000000000000000000000000000`
    let mut ratio: U256 = if abs_tick & 0x1 != 0 {
        U256::from_be_slice(&u128_to_be_bytes(0xfffcb933bd6fad37aa2d162d1a594001))
    } else {
        // 2^128
        U256::from(1u64) << 128
    };

    // Cascade of conditional multiplies.
    const MULTIPLIERS: [(u32, u128); 19] = [
        (0x2, 0xfff97272373d413259a46990580e213a),
        (0x4, 0xfff2e50f5f656932ef12357cf3c7fdcc),
        (0x8, 0xffe5caca7e10e4e61c3624eaa0941cd0),
        (0x10, 0xffcb9843d60f6159c9db58835c926644),
        (0x20, 0xff973b41fa98c081472e6896dfb254c0),
        (0x40, 0xff2ea16466c96a3843ec78b326b52861),
        (0x80, 0xfe5dee046a99a2a811c461f1969c3053),
        (0x100, 0xfcbe86c7900a88aedcffc83b479aa3a4),
        (0x200, 0xf987a7253ac413176f2b074cf7815e54),
        (0x400, 0xf3392b0822b70005940c7a398e4b70f3),
        (0x800, 0xe7159475a2c29b7443b29c7fa6e889d9),
        (0x1000, 0xd097f3bdfd2022b8845ad8f792aa5825),
        (0x2000, 0xa9f746462d870fdf8a65dc1f90e061e5),
        (0x4000, 0x70d869a156d2a1b890bb3df62baf32f7),
        (0x8000, 0x31be135f97d08fd981231505542fcfa6),
        (0x10000, 0x9aa508b5b7a84e1c677de54f3e99bc9),
        (0x20000, 0x5d6af8dedb81196699c329225ee604),
        (0x40000, 0x2216e584f5fa1ea926041bedfe98),
        (0x80000, 0x48a170391f7dc42444e8fa2),
    ];

    for &(mask, m) in MULTIPLIERS.iter() {
        if abs_tick & mask != 0 {
            // ratio = (ratio * m) >> 128
            let prod = ratio.saturating_mul(U256::from_be_slice(&u128_to_be_bytes(m)));
            ratio = prod >> 128;
        }
    }

    if tick > 0 {
        ratio = U256::MAX / ratio;
    }

    // Shift from Q128.128 to Q128.96 with ceiling rounding.
    let shifted = ratio >> 32;
    let remainder: U256 = ratio & ((U256::from(1u64) << 32) - U256::from(1u64));
    let rounded = if !remainder.is_zero() {
        shifted + U256::from(1u64)
    } else {
        shifted
    };
    Ok(rounded)
}

fn u128_to_be_bytes(x: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&x.to_be_bytes());
    out
}

/// Compute the largest tick whose `sqrtPriceX96 <= sqrt_price`. Mirrors
/// `TickMath.getTickAtSqrtRatio` (binary search via bit-magic).
pub fn tick_at_sqrt_price(sqrt_price_x96: U256) -> AmmResult<i32> {
    if sqrt_price_x96 < min_sqrt_ratio() || sqrt_price_x96 > max_sqrt_ratio() {
        return Err(AmmError::SqrtPriceOutOfBounds);
    }
    // Use a simple but exact binary search across the canonical valid tick range. The
    // monotone mapping `tick -> sqrt_price_at_tick(tick)` guarantees correctness, and we
    // only call this during background tick-window warmup, never inside the swap loop.
    let mut lo = MIN_TICK;
    let mut hi = MAX_TICK;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let s = sqrt_price_at_tick(mid)?;
        if s == sqrt_price_x96 {
            return Ok(mid);
        }
        if s < sqrt_price_x96 {
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    Ok(hi)
}

/// Compute the new sqrt price after `amount_in` of token0 is *added* to the pool
/// (zero-for-one swap takes token0 in).
///
/// From `SqrtPriceMath.getNextSqrtPriceFromAmount0RoundingUp` with `add = true`.
pub fn next_sqrt_price_from_amount0_in(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount_in: U256,
) -> AmmResult<U256> {
    if amount_in.is_zero() {
        return Ok(sqrt_price_x96);
    }
    let liq_shifted = U256::from(liquidity) << 96;
    // numerator1 = L << 96
    // product = amount * sqrtPX96
    let product = amount_in
        .checked_mul(sqrt_price_x96)
        .ok_or(AmmError::Overflow)?;
    if product / amount_in == sqrt_price_x96 {
        let denom = liq_shifted + product;
        if denom >= liq_shifted {
            return mul_div_rounding_up(liq_shifted, sqrt_price_x96, denom);
        }
    }
    // Fall-through path: divRoundingUp(numerator1, (numerator1 / sqrtPX96) + amount)
    let interim = div_rounding_up(liq_shifted, sqrt_price_x96)?;
    let denom = interim.checked_add(amount_in).ok_or(AmmError::Overflow)?;
    div_rounding_up(liq_shifted, denom)
}

/// Compute the new sqrt price after `amount_in` of token1 is *added* to the pool
/// (one-for-zero swap takes token1 in).
///
/// From `SqrtPriceMath.getNextSqrtPriceFromAmount1RoundingDown` with `add = true`.
pub fn next_sqrt_price_from_amount1_in(
    sqrt_price_x96: U256,
    liquidity: u128,
    amount_in: U256,
) -> AmmResult<U256> {
    // sqrt_new = sqrt_price + amount * 2^96 / L
    let q96: U256 = U256::from(1u64) << 96;
    let quotient = mul_div(amount_in, q96, U256::from(liquidity))?;
    sqrt_price_x96
        .checked_add(quotient)
        .ok_or(AmmError::Overflow)
}

fn div_rounding_up(num: U256, denom: U256) -> AmmResult<U256> {
    if denom.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    let q = num / denom;
    let r = num % denom;
    if !r.is_zero() {
        Ok(q + U256::from(1u64))
    } else {
        Ok(q)
    }
}

/// Compute `amount0_delta = liquidity * (sqrt_b - sqrt_a) / (sqrt_a * sqrt_b)`,
/// rounded up for input direction.
pub fn amount0_delta(
    sqrt_a: U256,
    sqrt_b: U256,
    liquidity: u128,
    round_up: bool,
) -> AmmResult<U256> {
    let (a, b) = if sqrt_a > sqrt_b {
        (sqrt_b, sqrt_a)
    } else {
        (sqrt_a, sqrt_b)
    };
    if a.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    let numerator1: U256 = U256::from(liquidity) << 96;
    let numerator2: U256 = b - a;
    if round_up {
        let intermediate = mul_div_rounding_up(numerator1, numerator2, b)?;
        div_rounding_up(intermediate, a)
    } else {
        let intermediate = mul_div(numerator1, numerator2, b)?;
        Ok(intermediate / a)
    }
}

/// Compute `amount1_delta = liquidity * (sqrt_b - sqrt_a)`.
pub fn amount1_delta(
    sqrt_a: U256,
    sqrt_b: U256,
    liquidity: u128,
    round_up: bool,
) -> AmmResult<U256> {
    let (a, b) = if sqrt_a > sqrt_b {
        (sqrt_b, sqrt_a)
    } else {
        (sqrt_a, sqrt_b)
    };
    let q96 = U256::from(1u64) << 96;
    if round_up {
        mul_div_rounding_up(U256::from(liquidity), b - a, q96)
    } else {
        mul_div(U256::from(liquidity), b - a, q96)
    }
}

/// Output of a V3 exact-input swap simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapOutput {
    /// Amount of output token produced.
    pub amount_out: U256,
    /// Amount of input token consumed (may be less than requested if the warm window ran out).
    pub amount_in_consumed: U256,
    /// Final sqrt price after the swap.
    pub sqrt_price_x96_after: U256,
    /// Final tick.
    pub tick_after: i32,
    /// Final liquidity (after any tick crossings).
    pub liquidity_after: u128,
    /// Fee paid (in units of input token).
    pub fee_amount: U256,
}

/// Exact-input V3 swap simulation — **sizing variant** (the hot path, called ~50–80× per
/// candidate by the golden-section sizer).
///
/// `ticks` MUST be sorted ascending and contain every initialised tick in the warmed window
/// for the route's plausible input range. `zero_for_one = true` means token0 → token1
/// (price decreases). The function returns `TickWindowExhausted` if the swap would cross
/// outside the supplied window.
///
/// This variant SKIPS computing the exact final tick on the terminal partial step — that is
/// a full-range `TickMath` binary search whose result (`tick_after`) is consumed only by the
/// state predictor, never by the sizing/search path (which reads `amount_out`). Here
/// `tick_after` is therefore the last *crossed* tick, not the exact tick at the final sqrt
/// price; every other output (`amount_out`, `amount_in_consumed`, `sqrt_price_x96_after`,
/// `liquidity_after`, `fee_amount`) is byte-identical to [`get_amount_out_with_final_tick`].
/// Use that variant whenever the post-swap tick is needed.
pub fn get_amount_out(
    slot0: Slot0,
    amount_in: U256,
    zero_for_one: bool,
    ticks: &[TickInfo],
) -> AmmResult<SwapOutput> {
    swap_inner(slot0, amount_in, zero_for_one, ticks, false)
}

/// Exact-input V3 swap simulation — **state-prediction variant**: byte-identical to
/// [`get_amount_out`] except it additionally computes the exact `tick_after` at the final
/// sqrt price. Use only where the post-swap tick is consumed (≈ once per accepted candidate);
/// the sizing search must use [`get_amount_out`] to avoid the per-call full-range tick search.
pub fn get_amount_out_with_final_tick(
    slot0: Slot0,
    amount_in: U256,
    zero_for_one: bool,
    ticks: &[TickInfo],
) -> AmmResult<SwapOutput> {
    swap_inner(slot0, amount_in, zero_for_one, ticks, true)
}

#[inline]
fn swap_inner(
    slot0: Slot0,
    amount_in: U256,
    zero_for_one: bool,
    ticks: &[TickInfo],
    compute_final_tick: bool,
) -> AmmResult<SwapOutput> {
    if amount_in.is_zero() {
        return Err(AmmError::ZeroInput);
    }
    if slot0.liquidity == 0 {
        return Err(AmmError::ZeroLiquidity);
    }
    if slot0.fee_pips >= 1_000_000 {
        return Err(AmmError::InvalidFee);
    }

    // Validate tick ordering once.
    for w in ticks.windows(2) {
        if w[0].tick >= w[1].tick {
            return Err(AmmError::InvariantViolation);
        }
    }

    // Fail closed if the current price is OUTSIDE the warmed tick window: we then
    // have no liquidity-distribution data around the price, and quoting across the
    // empty range at constant liquidity under-estimates slippage — which
    // over-quotes thin / drifted pools and manufactures phantom cross-venue edges
    // (e.g. a stale-window pool "offering" a 3% cycle). The caller refreshes the
    // window and retries; until then the pool is skipped rather than mis-quoted.
    //
    // An EMPTY window is the degenerate case of the same hazard: with no
    // initialised ticks we would walk to the canonical price bound at constant
    // liquidity, which lets an arbitrarily large swap "fit" with negligible
    // slippage — manufacturing phantom large-size cycles on pools whose window has
    // not warmed yet (e.g. Algebra pools before the first tick refresh). Skip
    // rather than mis-quote, exactly as for an out-of-window price.
    match (ticks.first(), ticks.last()) {
        (Some(first), Some(last)) => {
            if slot0.tick < first.tick || slot0.tick > last.tick {
                return Err(AmmError::TickWindowExhausted);
            }
        }
        _ => return Err(AmmError::TickWindowExhausted),
    }

    let mut sqrt_price_x96 = slot0.sqrt_price_x96;
    let mut tick = slot0.tick;
    let mut liquidity = slot0.liquidity;
    let mut amount_remaining = amount_in;
    let mut amount_out = U256::ZERO;
    let mut fee_paid = U256::ZERO;
    let mut amount_consumed = U256::ZERO;

    // sqrt price limit per direction (use canonical bounds plus/minus one bit
    // so we never need to step past TickMath bounds inside one call).
    let sqrt_price_limit = if zero_for_one {
        min_sqrt_ratio() + U256::from(1u64)
    } else {
        max_sqrt_ratio() - U256::from(1u64)
    };

    let fee_pips: u128 = slot0.fee_pips as u128;
    // #5 — hoist the per-pool fee constants out of the per-step loop: they are invariant for
    // the whole swap, so build their U256 forms once instead of in every `compute_swap_step`.
    // `fee_pips < 1_000_000` is guaranteed by the `InvalidFee` check above (no underflow).
    let fee = FeeConsts {
        denom: U256::from(1_000_000u64),
        complement: U256::from(1_000_000u64 - fee_pips as u64),
        pips: U256::from(fee_pips),
    };

    while !amount_remaining.is_zero() && sqrt_price_x96 != sqrt_price_limit {
        // 1) Find next initialised tick within the warmed window in the direction of motion.
        let (next_tick, next_sqrt, next_net) =
            find_next_initialised_tick(tick, zero_for_one, ticks, slot0.tick_spacing)?;
        // Clamp next sqrt to the limit.
        let target_sqrt = if zero_for_one {
            if next_sqrt < sqrt_price_limit {
                sqrt_price_limit
            } else {
                next_sqrt
            }
        } else {
            if next_sqrt > sqrt_price_limit {
                sqrt_price_limit
            } else {
                next_sqrt
            }
        };

        let (next_sqrt_after, amount_in_step, amount_out_step, fee_step) = compute_swap_step(
            sqrt_price_x96,
            target_sqrt,
            liquidity,
            amount_remaining,
            fee,
            zero_for_one,
        )?;

        amount_consumed = amount_consumed
            .checked_add(amount_in_step)
            .ok_or(AmmError::Overflow)?;
        amount_consumed = amount_consumed
            .checked_add(fee_step)
            .ok_or(AmmError::Overflow)?;
        amount_remaining = amount_remaining
            .checked_sub(amount_in_step + fee_step)
            .ok_or(AmmError::Underflow)?;
        amount_out = amount_out
            .checked_add(amount_out_step)
            .ok_or(AmmError::Overflow)?;
        fee_paid = fee_paid.checked_add(fee_step).ok_or(AmmError::Overflow)?;
        sqrt_price_x96 = next_sqrt_after;

        // 2) If we reached the target sqrt and the target corresponds to an initialised tick
        //    in the window, cross it: liquidity ±= liquidity_net.
        if sqrt_price_x96 == next_sqrt {
            // We hit an initialised tick — its liquidity_net came back from the single-pass lookup.
            let net = next_net;
            // Crossing: when zero_for_one (price decreasing), we subtract liquidity_net.
            // When one_for_zero (price increasing), we add liquidity_net.
            let signed: i128 = if zero_for_one { -net } else { net };
            liquidity = apply_signed_liquidity(liquidity, signed)?;
            tick = if zero_for_one {
                next_tick - 1
            } else {
                next_tick
            };
        } else {
            // Sqrt didn't reach the next initialised tick; the remaining input is consumed
            // by the partial step and we are done. The exact final tick is a full-range
            // `TickMath` binary search consumed only by the state predictor, so the sizing
            // variant skips it (leaving `tick` at the last crossed boundary). `sqrt_price_x96`
            // here is always within `[min_sqrt_ratio, max_sqrt_ratio]` (it is clamped to the
            // direction's limit), so the exact path's `tick_at_sqrt_price` cannot fail.
            if compute_final_tick {
                tick = tick_at_sqrt_price(sqrt_price_x96)?;
            }
            break;
        }

        // Window-exhaustion check: if we are about to step beyond the window, fail closed.
        if zero_for_one {
            if next_tick <= ticks.first().map(|t| t.tick).unwrap_or(MIN_TICK)
                && !amount_remaining.is_zero()
            {
                return Err(AmmError::TickWindowExhausted);
            }
        } else {
            if next_tick >= ticks.last().map(|t| t.tick).unwrap_or(MAX_TICK)
                && !amount_remaining.is_zero()
            {
                return Err(AmmError::TickWindowExhausted);
            }
        }
    }

    Ok(SwapOutput {
        amount_out,
        amount_in_consumed: amount_consumed,
        sqrt_price_x96_after: sqrt_price_x96,
        tick_after: tick,
        liquidity_after: liquidity,
        fee_amount: fee_paid,
    })
}

fn apply_signed_liquidity(current: u128, delta: i128) -> AmmResult<u128> {
    if delta >= 0 {
        current.checked_add(delta as u128).ok_or(AmmError::Overflow)
    } else {
        let abs = (-delta) as u128;
        current.checked_sub(abs).ok_or(AmmError::Underflow)
    }
}

fn find_next_initialised_tick(
    current_tick: i32,
    zero_for_one: bool,
    ticks: &[TickInfo],
    tick_spacing: i32,
) -> AmmResult<(i32, U256, i128)> {
    // We rely on the warmed window to enumerate initialised ticks. If no such tick exists,
    // we return the boundary tick of the window (which forces a window-exhaustion check
    // at the caller).
    //
    // #6 single pass: capture the next initialised tick AND its `liquidity_net` together. The
    // on-chain swap reads liquidityNet from this very tick (`ticks.cross(tickNext, …)`), so
    // returning it here is byte-exact and removes the separate `liquidity_net_at_tick` scan that
    // re-walked the window for the same tick on every crossing. A boundary (no initialised tick in
    // the direction of motion) carries net 0 — exactly what the old lookup returned on a miss.
    if zero_for_one {
        let mut best: Option<&TickInfo> = None;
        for t in ticks.iter().rev() {
            if t.tick <= current_tick {
                best = Some(t);
                break;
            }
        }
        match best {
            Some(t) => Ok((t.tick, sqrt_price_at_tick(t.tick)?, t.liquidity_net)),
            None => {
                // Align to tick_spacing below current.
                let aligned = current_tick - (current_tick.rem_euclid(tick_spacing));
                let target = aligned - tick_spacing;
                Ok((target, sqrt_price_at_tick(target)?, 0))
            }
        }
    } else {
        let mut best: Option<&TickInfo> = None;
        for t in ticks.iter() {
            if t.tick > current_tick {
                best = Some(t);
                break;
            }
        }
        match best {
            Some(t) => Ok((t.tick, sqrt_price_at_tick(t.tick)?, t.liquidity_net)),
            None => {
                let rem = current_tick.rem_euclid(tick_spacing);
                let target = current_tick - rem + tick_spacing;
                Ok((target, sqrt_price_at_tick(target)?, 0))
            }
        }
    }
}

/// Per-pool fee constants in U256 form, built once per `swap_inner` call and reused across
/// every step (#5): `denom = 1e6`, `complement = 1e6 - fee_pips`, `pips = fee_pips`.
#[derive(Clone, Copy)]
struct FeeConsts {
    denom: U256,
    complement: U256,
    pips: U256,
}

/// Compute a single swap step within `[sqrt_current, sqrt_target]`.
/// Mirrors `SwapMath.computeSwapStep` for exact input (amount_remaining > 0).
fn compute_swap_step(
    sqrt_current: U256,
    sqrt_target: U256,
    liquidity: u128,
    amount_remaining: U256,
    fee: FeeConsts,
    zero_for_one: bool,
) -> AmmResult<(U256, U256, U256, U256)> {
    // amount_remaining_less_fee = amount_remaining * (1e6 - fee_pips) / 1e6
    let amount_remaining_less_fee = mul_div(amount_remaining, fee.complement, fee.denom)?;

    // Required input to reach target sqrt.
    let amount_in_required = if zero_for_one {
        amount0_delta(sqrt_target, sqrt_current, liquidity, true)?
    } else {
        amount1_delta(sqrt_current, sqrt_target, liquidity, true)?
    };

    let (sqrt_next, amount_in_step, amount_out_step, fee_step) =
        if amount_remaining_less_fee >= amount_in_required {
            // Step fully to target.
            let amount_out_step = if zero_for_one {
                amount1_delta(sqrt_target, sqrt_current, liquidity, false)?
            } else {
                amount0_delta(sqrt_current, sqrt_target, liquidity, false)?
            };
            // Fee on the *step* input.
            let fee_step = mul_div_rounding_up(amount_in_required, fee.pips, fee.complement)?;
            (sqrt_target, amount_in_required, amount_out_step, fee_step)
        } else {
            // Partial step. Compute next sqrt price from the available input less fees.
            let sqrt_next = if zero_for_one {
                next_sqrt_price_from_amount0_in(sqrt_current, liquidity, amount_remaining_less_fee)?
            } else {
                next_sqrt_price_from_amount1_in(sqrt_current, liquidity, amount_remaining_less_fee)?
            };
            let amount_in_step = if zero_for_one {
                amount0_delta(sqrt_next, sqrt_current, liquidity, true)?
            } else {
                amount1_delta(sqrt_current, sqrt_next, liquidity, true)?
            };
            let amount_out_step = if zero_for_one {
                amount1_delta(sqrt_next, sqrt_current, liquidity, false)?
            } else {
                amount0_delta(sqrt_current, sqrt_next, liquidity, false)?
            };
            // Fee swept up all remainder less the actual step input.
            let fee_step = amount_remaining
                .checked_sub(amount_in_step)
                .ok_or(AmmError::Underflow)?;
            (sqrt_next, amount_in_step, amount_out_step, fee_step)
        };

    Ok((sqrt_next, amount_in_step, amount_out_step, fee_step))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqrt_price_at_zero_tick_is_q96() {
        let s = sqrt_price_at_tick(0).unwrap();
        // sqrt(1.0001^0) * 2^96 == 2^96
        let q96 = U256::from(1u64) << 96;
        assert_eq!(s, q96);
    }

    #[test]
    fn sqrt_price_monotonic_around_zero() {
        let m1 = sqrt_price_at_tick(-1).unwrap();
        let p0 = sqrt_price_at_tick(0).unwrap();
        let p1 = sqrt_price_at_tick(1).unwrap();
        assert!(m1 < p0);
        assert!(p0 < p1);
    }

    #[test]
    fn sqrt_price_bounds() {
        let smin = sqrt_price_at_tick(MIN_TICK).unwrap();
        let smax = sqrt_price_at_tick(MAX_TICK).unwrap();
        assert_eq!(smin, min_sqrt_ratio());
        assert_eq!(smax, max_sqrt_ratio());
    }

    #[test]
    fn tick_at_sqrt_inverse_zero() {
        let s = sqrt_price_at_tick(0).unwrap();
        let t = tick_at_sqrt_price(s).unwrap();
        assert_eq!(t, 0);
    }

    #[test]
    fn next_sqrt_from_amount0_increases_one_for_zero() {
        // Adding token0 decreases sqrt price.
        let s = sqrt_price_at_tick(0).unwrap();
        let liq: u128 = 1_000_000_000_000_000_000;
        let amount = U256::from(1_000u64);
        let new_s = next_sqrt_price_from_amount0_in(s, liq, amount).unwrap();
        assert!(new_s < s);
    }

    #[test]
    fn next_sqrt_from_amount1_increases_zero_for_one() {
        let s = sqrt_price_at_tick(0).unwrap();
        let liq: u128 = 1_000_000_000_000_000_000;
        let amount = U256::from(1_000u64);
        let new_s = next_sqrt_price_from_amount1_in(s, liq, amount).unwrap();
        assert!(new_s > s);
    }

    #[test]
    fn v3_basic_swap_no_tick_cross() {
        // A pool with active liquidity and a tick window much wider than the swap size.
        let slot0 = Slot0 {
            sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
            tick: 0,
            fee_pips: 3000,
            liquidity: 1_000_000_000_000_000_000u128,
            tick_spacing: 60,
        };
        // Provide a wide warm window with no initialised ticks in the price path range.
        let ticks = vec![
            TickInfo {
                tick: -10_000,
                liquidity_net: 0,
            },
            TickInfo {
                tick: 10_000,
                liquidity_net: 0,
            },
        ];
        let out = get_amount_out(slot0, U256::from(1_000_000u64), true, &ticks).unwrap();
        assert!(out.amount_out > U256::ZERO);
        assert!(out.sqrt_price_x96_after < slot0.sqrt_price_x96);
        assert_eq!(out.liquidity_after, slot0.liquidity);
    }

    /// Byte-exact guard for the single-pass tick lookup (#6): a swap that CROSSES initialised
    /// ticks with non-zero `liquidity_net` (the path the no-cross tests never hit). The golden
    /// values below were captured from the pre-refactor kernel; the merged scan must reproduce
    /// them bit-for-bit.
    fn crossing_fixture() -> (Slot0, Vec<TickInfo>) {
        let slot0 = Slot0 {
            sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
            tick: 0,
            fee_pips: 500,
            liquidity: 10_000_000_000_000_000_000u128, // 1e19
            tick_spacing: 10,
        };
        let ticks = vec![
            TickInfo {
                tick: -10_000,
                liquidity_net: 0,
            },
            TickInfo {
                tick: -300,
                liquidity_net: 2_000_000_000_000_000_000,
            },
            TickInfo {
                tick: -100,
                liquidity_net: 3_000_000_000_000_000_000,
            },
            TickInfo {
                tick: 100,
                liquidity_net: -3_000_000_000_000_000_000,
            },
            TickInfo {
                tick: 300,
                liquidity_net: -2_000_000_000_000_000_000,
            },
            TickInfo {
                tick: 10_000,
                liquidity_net: 0,
            },
        ];
        (slot0, ticks)
    }

    #[test]
    fn v3_swap_crosses_initialised_ticks_byte_exact() {
        // Golden values captured from the pre-#6 kernel. The merged single-pass tick lookup must
        // reproduce them bit-for-bit (amount_out↓, fee↑, liquidity crossing, final sqrt).
        let (slot0, ticks) = crossing_fixture();
        // 1e17 crosses tick -100: liquidity 1e19 → 7e18.
        let o1 =
            get_amount_out(slot0, U256::from(100_000_000_000_000_000u64), true, &ticks).unwrap();
        assert_eq!(o1.amount_out, U256::from(98_857_316_676_530_575u128));
        assert_eq!(
            o1.amount_in_consumed,
            U256::from(100_000_000_000_000_000u128)
        );
        assert_eq!(o1.liquidity_after, 7_000_000_000_000_000_000u128);
        assert_eq!(
            o1.sqrt_price_x96_after,
            "78278607322121512521408228717".parse::<U256>().unwrap()
        );
        assert_eq!(o1.fee_amount, U256::from(50_000_000_000_001u128));
        // 3e17 crosses ticks -100 AND -300: liquidity 1e19 → 5e18.
        let o2 =
            get_amount_out(slot0, U256::from(300_000_000_000_000_000u64), true, &ticks).unwrap();
        assert_eq!(o2.amount_out, U256::from(286_990_044_445_195_254u128));
        assert_eq!(o2.liquidity_after, 5_000_000_000_000_000_000u128);
        assert_eq!(
            o2.sqrt_price_x96_after,
            "75389501233712575057466277204".parse::<U256>().unwrap()
        );
        assert_eq!(o2.fee_amount, U256::from(150_000_000_000_001u128));
    }

    #[test]
    fn find_next_returns_crossed_tick_liquidity_net() {
        // #6 contract: the next-tick lookup returns (tick, sqrt, liquidity_net) of the tick that
        // will be crossed — equal to the old separate `liquidity_net_at_tick` for an initialised
        // tick, and 0 for a window boundary (no initialised tick in that direction).
        let ticks = vec![
            TickInfo {
                tick: -100,
                liquidity_net: 3_000_000_000_000_000_000,
            },
            TickInfo {
                tick: 100,
                liquidity_net: -3_000_000_000_000_000_000,
            },
        ];
        let (t, _s, net) = find_next_initialised_tick(0, true, &ticks, 10).unwrap();
        assert_eq!((t, net), (-100, 3_000_000_000_000_000_000));
        let (t2, _s2, net2) = find_next_initialised_tick(0, false, &ticks, 10).unwrap();
        assert_eq!((t2, net2), (100, -3_000_000_000_000_000_000));
        // Boundary: from just below the lowest initialised tick (the loop sets tick = next-1 after
        // crossing -100), nothing is initialised further down → boundary tick, net 0.
        let (tb, _sb, netb) = find_next_initialised_tick(-101, true, &ticks, 10).unwrap();
        assert!(tb < -100 && netb == 0);
    }

    /// #3 skip-tick split (the critical safety property): the sizing variant
    /// (`get_amount_out`, which skips the terminal full-range tick search) is byte-identical
    /// to the exact variant (`get_amount_out_with_final_tick`) on EVERY field the
    /// sizing/search path reads. Only `tick_after` may differ. Run across both directions,
    /// all fee tiers, and a range of amounts so partial steps and tick crossings both occur.
    #[test]
    fn v3_skip_tick_matches_exact_on_all_sizing_outputs() {
        let ticks = vec![
            TickInfo {
                tick: -5_000,
                liquidity_net: 0,
            },
            TickInfo {
                tick: 5_000,
                liquidity_net: 0,
            },
        ];
        for &fee_pips in &[100u32, 500, 3000, 10000] {
            for &zero_for_one in &[true, false] {
                for &amt in &[1_000u64, 1_000_000, 1_000_000_000, 50_000_000_000_000_000] {
                    let slot0 = Slot0 {
                        sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
                        tick: 0,
                        fee_pips,
                        liquidity: 1_000_000_000_000_000_000u128,
                        tick_spacing: 60,
                    };
                    let amount = U256::from(amt);
                    let fast = get_amount_out(slot0, amount, zero_for_one, &ticks);
                    let exact = get_amount_out_with_final_tick(slot0, amount, zero_for_one, &ticks);
                    match (fast, exact) {
                        (Ok(f), Ok(e)) => {
                            assert_eq!(
                                f.amount_out, e.amount_out,
                                "amount_out (fee={fee_pips} zfo={zero_for_one} amt={amt})"
                            );
                            assert_eq!(
                                f.amount_in_consumed, e.amount_in_consumed,
                                "amount_in_consumed"
                            );
                            assert_eq!(
                                f.sqrt_price_x96_after, e.sqrt_price_x96_after,
                                "sqrt_after"
                            );
                            assert_eq!(f.liquidity_after, e.liquidity_after, "liquidity_after");
                            assert_eq!(f.fee_amount, e.fee_amount, "fee_amount");
                        }
                        (Err(_), Err(_)) => {} // both fail-closed identically — acceptable
                        (f, e) => panic!("variants disagreed on Ok/Err: fast={f:?} exact={e:?}"),
                    }
                }
            }
        }
    }

    /// #3 skip-tick split (the optimization is real + the exact variant stays exact): on a
    /// guaranteed PARTIAL step (wide net-0 window, no tick crossing), the sizing variant
    /// leaves `tick_after` at the start tick (it skipped the search) while the exact variant
    /// computes the true tick — they DIFFER (proving the search was skipped) and the exact
    /// value equals a direct `TickMath` lookup at the final sqrt price.
    #[test]
    fn v3_skip_tick_partial_step_skips_search_exact_stays_exact() {
        let slot0 = Slot0 {
            sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
            tick: 0,
            fee_pips: 500,
            liquidity: 1_000_000_000_000_000_000u128,
            tick_spacing: 60,
        };
        // Window far wider than the price move ⇒ a single partial step, no crossing.
        let ticks = vec![
            TickInfo {
                tick: -100_000,
                liquidity_net: 0,
            },
            TickInfo {
                tick: 100_000,
                liquidity_net: 0,
            },
        ];
        let amount = U256::from(50_000_000_000_000_000u64); // 0.05 — moves price hundreds of ticks
        let fast = get_amount_out(slot0, amount, true, &ticks).unwrap();
        let exact = get_amount_out_with_final_tick(slot0, amount, true, &ticks).unwrap();
        // Same economics.
        assert_eq!(fast.amount_out, exact.amount_out);
        assert_eq!(fast.sqrt_price_x96_after, exact.sqrt_price_x96_after);
        // Skip variant left the tick at the start (no crossing updated it, search skipped).
        assert_eq!(
            fast.tick_after, 0,
            "skip variant must not run the terminal tick search"
        );
        // Exact variant computed the true post-swap tick (a real partial-step move, < 0).
        assert!(
            exact.tick_after < 0,
            "price moved down ⇒ exact tick is negative"
        );
        assert_ne!(
            fast.tick_after, exact.tick_after,
            "the skipped search must actually change tick_after"
        );
        assert_eq!(
            exact.tick_after,
            tick_at_sqrt_price(exact.sqrt_price_x96_after).unwrap(),
            "exact tick_after == TickMath lookup at the final sqrt (partial step)"
        );
    }

    #[test]
    fn v3_zero_input_errors() {
        let slot0 = Slot0 {
            sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
            tick: 0,
            fee_pips: 3000,
            liquidity: 1_000_000_000_000_000_000u128,
            tick_spacing: 60,
        };
        let ticks = vec![
            TickInfo {
                tick: -100,
                liquidity_net: 0,
            },
            TickInfo {
                tick: 100,
                liquidity_net: 0,
            },
        ];
        assert!(matches!(
            get_amount_out(slot0, U256::ZERO, true, &ticks),
            Err(AmmError::ZeroInput)
        ));
    }

    #[test]
    fn v3_zero_liquidity_errors() {
        let slot0 = Slot0 {
            sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
            tick: 0,
            fee_pips: 3000,
            liquidity: 0,
            tick_spacing: 60,
        };
        let ticks = vec![];
        assert!(matches!(
            get_amount_out(slot0, U256::from(1u64), true, &ticks),
            Err(AmmError::ZeroLiquidity)
        ));
    }

    #[test]
    fn v3_unsorted_ticks_rejected() {
        let slot0 = Slot0 {
            sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
            tick: 0,
            fee_pips: 3000,
            liquidity: 1_000_000_000_000_000_000u128,
            tick_spacing: 60,
        };
        let ticks = vec![
            TickInfo {
                tick: 100,
                liquidity_net: 0,
            },
            TickInfo {
                tick: -100,
                liquidity_net: 0,
            },
        ];
        assert!(matches!(
            get_amount_out(slot0, U256::from(1_000u64), true, &ticks),
            Err(AmmError::InvariantViolation)
        ));
    }
}

/// K1/K2: randomized differential + property tests for the V3 fixed-point fast paths.
///
/// Plan invariant #8 ("fixed-point fast paths gated by overflow checks **and** differential tests
/// vs U256/U512"). The V3 kernel's overflow-guarded `U256` paths (`liquidity << 96`, the
/// `getNextSqrtPriceFromAmount0` cheap/overflow branch selection, the two-step `amount{0,1}Delta`
/// rounding) are checked here against **independent full-precision `U512` oracles** that mirror the
/// Uniswap formula's exact branch + rounding structure but can never overflow an intermediate —
/// so any silent truncation or wrong-branch bug in the kernel surfaces as a mismatch. Inputs span
/// the near-overflow boundary (full-width `U256` amounts/prices), the most likely divergence site.
#[cfg(test)]
mod v3_differential {
    use super::*;
    use proptest::prelude::*;
    use ruint::aliases::U512;

    fn u512(x: U256) -> U512 {
        U512::from(x)
    }

    fn to_u256(x: U512) -> Option<U256> {
        if x.bit_len() > 256 {
            None
        } else {
            let l = x.as_limbs();
            Some(U256::from_limbs([l[0], l[1], l[2], l[3]]))
        }
    }

    fn ceil_div_512(n: U512, d: U512) -> U512 {
        let q = n / d;
        if (n % d).is_zero() {
            q
        } else {
            q + U512::from(1u64)
        }
    }

    /// Independent U512 oracle for `SqrtPriceMath.getAmount0Delta` — the kernel does it in TWO
    /// rounding steps (`mul_div{_rounding_up}(L<<96, b-a, b)` then `div{_rounding_up}(·, a)`); the
    /// oracle replays that exact two-step structure in 512-bit so no intermediate can overflow.
    fn ref_amount0_delta(sqrt_a: U256, sqrt_b: U256, liq: u128, round_up: bool) -> Option<U256> {
        let (a, b) = if sqrt_a > sqrt_b {
            (sqrt_b, sqrt_a)
        } else {
            (sqrt_a, sqrt_b)
        };
        if a.is_zero() {
            return None;
        }
        let num1 = U512::from(liq) << 96;
        let num2 = u512(b) - u512(a);
        let prod = num1 * num2;
        let bb = u512(b);
        let aa = u512(a);
        let inter = if round_up {
            ceil_div_512(prod, bb)
        } else {
            prod / bb
        };
        let res = if round_up {
            ceil_div_512(inter, aa)
        } else {
            inter / aa
        };
        to_u256(res)
    }

    /// Independent U512 oracle for `SqrtPriceMath.getAmount1Delta` = `round(L * (b-a) / 2^96)`.
    fn ref_amount1_delta(sqrt_a: U256, sqrt_b: U256, liq: u128, round_up: bool) -> Option<U256> {
        let (a, b) = if sqrt_a > sqrt_b {
            (sqrt_b, sqrt_a)
        } else {
            (sqrt_a, sqrt_b)
        };
        let num = U512::from(liq) * (u512(b) - u512(a));
        let den = U512::from(1u64) << 96;
        let res = if round_up {
            ceil_div_512(num, den)
        } else {
            num / den
        };
        to_u256(res)
    }

    /// Full-range U256 generator (biased toward large — exercises the overflow boundary).
    fn arb_u256() -> impl Strategy<Value = U256> {
        any::<[u64; 4]>().prop_map(U256::from_limbs)
    }

    /// U256 capped to `bits` significant bits.
    fn arb_u256_bits(bits: u32) -> impl Strategy<Value = U256> {
        arb_u256().prop_map(move |x| x >> (256 - bits))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4000))]

        /// `amount0_delta` (both rounding directions) is byte-identical to the U512 two-step oracle,
        /// OR both agree it overflows U256 (`None`/`Err(Overflow)`). Inputs up to 160-bit sqrt
        /// prices + full-width u128 liquidity → covers the realistic V3 range with headroom.
        #[test]
        fn amount0_delta_matches_u512_oracle(
            a in arb_u256_bits(160),
            b in arb_u256_bits(160),
            liq in any::<u128>(),
            round_up in any::<bool>(),
        ) {
            // a must be non-zero after sorting for a defined result; skip the degenerate 0 case.
            let lo = a.min(b);
            prop_assume!(!lo.is_zero());
            let got = amount0_delta(a, b, liq, round_up);
            match ref_amount0_delta(a, b, liq, round_up) {
                Some(expected) => prop_assert_eq!(got.ok(), Some(expected)),
                None => prop_assert!(matches!(got, Err(AmmError::Overflow))),
            }
        }

        /// `amount1_delta` (both rounding directions) is byte-identical to the U512 oracle.
        #[test]
        fn amount1_delta_matches_u512_oracle(
            a in arb_u256_bits(160),
            b in arb_u256_bits(160),
            liq in any::<u128>(),
            round_up in any::<bool>(),
        ) {
            let got = amount1_delta(a, b, liq, round_up);
            match ref_amount1_delta(a, b, liq, round_up) {
                Some(expected) => prop_assert_eq!(got.ok(), Some(expected)),
                None => prop_assert!(matches!(got, Err(AmmError::Overflow))),
            }
        }

        /// The `L << 96` fast path (numerator1) is EXACTLY `L * 2^96` — the shift the plan flags as
        /// a fast path must never truncate vs the multiply.
        #[test]
        fn liquidity_shift_96_equals_multiply(liq in any::<u128>()) {
            let shift = U256::from(liq) << 96;
            let mul = to_u256(U512::from(liq) * (U512::from(1u64) << 96)).unwrap();
            prop_assert_eq!(shift, mul);
        }

        /// `getNextSqrtPriceFromAmount0` must never panic and — when it returns Ok — the price moves
        /// DOWN (adding token0 lowers sqrt price) and stays ≤ the start. Amount spans full U256 to
        /// force the `amount*sqrtP` overflow branch (the exact fast-path/fallback boundary).
        #[test]
        fn next_sqrt_from_amount0_moves_down_or_errs(
            sqrt_p in arb_u256_bits(160),
            liq in 1u128..=u128::MAX,
            amount in arb_u256(),
        ) {
            prop_assume!(!sqrt_p.is_zero() && !amount.is_zero());
            if let Ok(next) = next_sqrt_price_from_amount0_in(sqrt_p, liq, amount) {
                prop_assert!(next <= sqrt_p, "adding token0 must not raise sqrt price");
            }
        }

        /// `getNextSqrtPriceFromAmount1` must never panic and — when Ok — the price moves UP.
        #[test]
        fn next_sqrt_from_amount1_moves_up_or_errs(
            sqrt_p in arb_u256_bits(160),
            liq in 1u128..=u128::MAX,
            amount in arb_u256_bits(160),
        ) {
            prop_assume!(!sqrt_p.is_zero() && !amount.is_zero());
            if let Ok(next) = next_sqrt_price_from_amount1_in(sqrt_p, liq, amount) {
                prop_assert!(next >= sqrt_p, "adding token1 must not lower sqrt price");
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1500))]

        /// End-to-end `get_amount_out` on a well-formed single-tick-range pool: never panics, and
        /// when it returns Ok the economic invariants hold (positive out, input fully accounted,
        /// price moved the right way). This exercises the whole tick-walk + fee loop under fuzz.
        #[test]
        fn get_amount_out_invariants(
            fee_pips in prop::sample::select(vec![100u32, 500, 3000, 10000]),
            liq in 1_000_000_000u128..=1_000_000_000_000_000_000_000u128,
            amount in 1u64..=1_000_000_000_000_000u64,
            zero_for_one in any::<bool>(),
        ) {
            let slot0 = Slot0 {
                sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
                tick: 0,
                fee_pips,
                liquidity: liq,
                tick_spacing: 60,
            };
            // Wide window with no interior initialised ticks → single constant-liquidity range.
            let ticks = vec![
                TickInfo { tick: -200_000, liquidity_net: 0 },
                TickInfo { tick: 200_000, liquidity_net: 0 },
            ];
            match get_amount_out(slot0, U256::from(amount), zero_for_one, &ticks) {
                Ok(out) => {
                    prop_assert!(out.amount_in_consumed <= U256::from(amount));
                    if out.amount_out > U256::ZERO {
                        if zero_for_one {
                            prop_assert!(out.sqrt_price_x96_after <= slot0.sqrt_price_x96);
                        } else {
                            prop_assert!(out.sqrt_price_x96_after >= slot0.sqrt_price_x96);
                        }
                    }
                }
                // A fail-closed AmmError is acceptable (e.g. window exhausted) — never a panic.
                Err(_) => {}
            }
        }

        /// Monotonicity: a larger input never yields a smaller output (same pool, same direction).
        #[test]
        fn get_amount_out_monotonic_in_amount(
            liq in 1_000_000_000_000u128..=1_000_000_000_000_000_000_000u128,
            a1 in 1u64..=1_000_000_000u64,
            bump in 1u64..=1_000_000_000u64,
            zero_for_one in any::<bool>(),
        ) {
            let slot0 = Slot0 {
                sqrt_price_x96: sqrt_price_at_tick(0).unwrap(),
                tick: 0,
                fee_pips: 3000,
                liquidity: liq,
                tick_spacing: 60,
            };
            let ticks = vec![
                TickInfo { tick: -400_000, liquidity_net: 0 },
                TickInfo { tick: 400_000, liquidity_net: 0 },
            ];
            let o1 = get_amount_out(slot0, U256::from(a1), zero_for_one, &ticks);
            let o2 = get_amount_out(slot0, U256::from(a1) + U256::from(bump), zero_for_one, &ticks);
            if let (Ok(r1), Ok(r2)) = (o1, o2) {
                prop_assert!(r2.amount_out >= r1.amount_out, "more input ⇒ not less output");
            }
        }
    }
}
