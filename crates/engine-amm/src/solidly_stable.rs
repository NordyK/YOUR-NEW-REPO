//! Solidly stable-curve AMM math: `x³y + xy³ = k`.
//!
//! Used by Chronos / Ramses / Velodrome stable pools on Arbitrum. Operates
//! over a Newton-Raphson solver with dual convergence (residual + step-size).
//! Pure math kernel — fee-fetching and discovery live in separate modules.
//!
//! A5 (verified-correct, locked): this kernel computes the invariant on the RAW (unscaled) reserve
//! magnitudes (`k = x³y + xy³` in [`invariant`]), whereas Velodrome's Solidity `_k`/`_d` first
//! divide each balance by `1e18`. Those two forms are QUOTE-EQUIVALENT: every Newton step here is a
//! ratio `f(y) / f'(y)` (see [`newton_raphson_solve_y`]) in which the uniform `1e18` scale factor
//! appears in both numerator and denominator and CANCELS, so the converged `y` — and therefore
//! `dy = reserve_out - y` — is identical to the scaled form. The unscaled products are larger, so
//! the kernel is fail-CLOSED on magnitude: callers gate every operand with `stable_norm_ok`
//! (`<= MAX_STABLE_NORM = 1e36`, ~1e6× above any real pool yet far below the U512 `x³y` overflow at
//! x≈1.07e38). An input above the bound returns `AmmError::Overflow` rather than wrapping into a
//! phantom-profitable quote. Do not "scale to match Velodrome": it changes nothing but the rounding
//! and removes the headroom the magnitude guard relies on.

use alloy_primitives::{U256, U512};

use crate::error::{AmmError, AmmResult};

/// 1e18 fixed-point precision.
pub const E18: U256 = U256::from_limbs([1_000_000_000_000_000_000u64, 0, 0, 0]);

/// Default cap on reserve ratio (`max / min`). Pools exceeding this ratio are
/// rejected as imbalanced — typically a sign of a depegged stable or hostile
/// state. Override at runtime via `SOLIDLY_MAX_RESERVE_RATIO` env var.
pub const DEFAULT_MAX_RESERVE_RATIO: u64 = 10_000;

fn max_reserve_ratio() -> u64 {
    static CACHED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("SOLIDLY_MAX_RESERVE_RATIO")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_RESERVE_RATIO)
    })
}

#[inline]
fn u512_to_u256(val: U512) -> AmmResult<U256> {
    if val > U512::from(U256::MAX) {
        Err(AmmError::Overflow)
    } else {
        Ok(U256::from_limbs(
            val.as_limbs()[..4].try_into().expect("U512 has 8 limbs"),
        ))
    }
}

#[inline]
fn pow10(decimals: u8) -> U256 {
    let mut out = U256::from(1u64);
    for _ in 0..decimals {
        out = out.checked_mul(U256::from(10u64)).expect("pow10");
    }
    out
}

/// Normalize a raw token amount to 1e18 domain (matches Velodrome `_getAmountOut`).
#[inline]
pub fn normalize_to_1e18(value: U256, decimals: u8) -> AmmResult<U256> {
    if decimals == 18 {
        return Ok(value);
    }
    value
        .checked_mul(E18)
        .ok_or(AmmError::Overflow)?
        .checked_div(pow10(decimals))
        .ok_or(AmmError::InvariantViolation)
}

/// Denormalize from 1e18 domain back to native token decimals.
#[inline]
pub fn denormalize_from_1e18(value: U256, decimals: u8) -> AmmResult<U256> {
    if decimals == 18 {
        return Ok(value);
    }
    value
        .checked_mul(pow10(decimals))
        .ok_or(AmmError::Overflow)?
        .checked_div(E18)
        .ok_or(AmmError::InvariantViolation)
}

/// Conservative fail-closed bound on a normalized reserve (or post-swap balance). The Solidly
/// invariant `x³y` is computed in U512 (max ≈1.34e154), so it overflows around x≈1.07e38; this
/// bound (1e36) sits far below that yet ~1e6× above the largest realistic normalized reserve
/// (~1e30 for a billion-dollar 18-dp pool). Inputs above it are rejected rather than wrapped.
const MAX_STABLE_NORM: u128 = 1_000_000_000_000_000_000_000_000_000_000_000_000; // 1e36

#[inline]
fn stable_norm_ok(v: U256) -> bool {
    v <= U256::from(MAX_STABLE_NORM)
}

/// Compute the Solidly invariant `k = x³y + xy³`. Callers MUST gate `x`/`y` with `stable_norm_ok`
/// first (the U512 products are unguarded by design — bounding the inputs is the fail-closed path).
#[inline]
fn invariant(x: U256, y: U256) -> U512 {
    let xw = U512::from(x);
    let yw = U512::from(y);
    (xw * xw * xw * yw) + (xw * yw * yw * yw)
}

/// Newton-Raphson solver for the Solidly stable curve.
///
/// Solves `f(y) = x³y + xy³ - k = 0` for `y`. Max 255 iterations with dual
/// convergence (residual ≤ y/1e6 or step size ≤ 1).
fn newton_raphson_solve_y(x: U256, k: U512, initial_guess: U256) -> AmmResult<U256> {
    const MAX_ITERATIONS: u8 = 255;

    if x.is_zero() {
        return Err(AmmError::InvariantViolation);
    }

    let mut y = initial_guess;
    let xw = U512::from(x);
    let x_cubed_w = xw * xw * xw;

    for _ in 0..MAX_ITERATIONS {
        let yw = U512::from(y);
        let x_cubed_y = x_cubed_w * yw;
        let xy_cubed = xw * yw * yw * yw;
        let f_y = x_cubed_y + xy_cubed;

        let tolerance = U512::from((y / U256::from(1_000_000u32)).max(U256::from(1u32)));

        if f_y >= k {
            let diff = f_y - k;
            if diff <= tolerance {
                return Ok(y);
            }
        } else {
            let diff = k - f_y;
            if diff <= tolerance {
                return Ok(y);
            }
        }

        // f'(y) = x³ + 3xy²
        let three_xy_squared = xw * yw * yw * U512::from(3u64);
        let f_prime_y = x_cubed_w + three_xy_squared;
        if f_prime_y.is_zero() {
            return Err(AmmError::InvariantViolation);
        }

        let (delta_w, decreasing) = if f_y >= k {
            ((f_y - k) / f_prime_y, true)
        } else {
            ((k - f_y) / f_prime_y, false)
        };
        let delta = u512_to_u256(delta_w)?;

        if delta <= U256::from(1u32) {
            return Ok(y);
        }

        y = if decreasing {
            y.checked_sub(delta).ok_or(AmmError::Underflow)?
        } else {
            y.checked_add(delta).ok_or(AmmError::Overflow)?
        };
    }

    Err(AmmError::InvariantViolation)
}

/// Per-Velodrome fee deduction (`Pool.getAmountOut`): `amountIn - (amountIn * fee) / 10000`.
/// The pool floors the FEE then subtracts. The earlier `amountIn * (10000 - fee) / 10000` form
/// floors the *product* and can under-quote the post-fee input by 1 wei (conservative, but not
/// byte-exact vs the deployed pool). Kept overflow/underflow-checked like the original.
#[inline]
fn solidly_amount_in_after_fee(amount_in: U256, fee_bps: u16) -> AmmResult<U256> {
    // Velodrome Pool.getAmountOut: amountIn -= (amountIn * fee) / 10000.
    let fee = amount_in
        .checked_mul(U256::from(fee_bps as u64))
        .ok_or(AmmError::Overflow)?
        / U256::from(10_000u64);
    amount_in.checked_sub(fee).ok_or(AmmError::Underflow)
}

/// Calculate output amount for a Solidly stable curve swap (single-decimals case).
///
/// `fee_bps` in basis points (e.g. 4 = 0.04%).
pub fn calculate_stable_swap_output(
    reserve_in: U256,
    reserve_out: U256,
    amount_in: U256,
    fee_bps: u16,
) -> AmmResult<U256> {
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(AmmError::ZeroLiquidity);
    }
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }

    // Reserve imbalance check.
    let max_reserve = reserve_in.max(reserve_out);
    let min_reserve = reserve_in.min(reserve_out);
    if max_reserve > min_reserve.saturating_mul(U256::from(max_reserve_ratio())) {
        return Err(AmmError::ZeroLiquidity);
    }

    if fee_bps > 10_000 {
        return Err(AmmError::InvalidFee);
    }
    let amount_in_with_fee = solidly_amount_in_after_fee(amount_in, fee_bps)?;

    if !stable_norm_ok(reserve_in) || !stable_norm_ok(reserve_out) {
        return Err(AmmError::Overflow);
    }
    let k = invariant(reserve_in, reserve_out);
    let x_new = reserve_in
        .checked_add(amount_in_with_fee)
        .ok_or(AmmError::Overflow)?;
    if !stable_norm_ok(x_new) {
        return Err(AmmError::Overflow);
    }
    let y_new = newton_raphson_solve_y(x_new, k, reserve_out)?;

    reserve_out.checked_sub(y_new).ok_or(AmmError::Underflow)
}

/// Calculate output amount with decimal normalization (matches Velodrome
/// `_getAmountOut`). Use this for cross-decimal pairs (e.g. USDC 6dp ↔ USDe 18dp).
pub fn calculate_stable_swap_output_normalized(
    reserve_in: U256,
    reserve_out: U256,
    amount_in: U256,
    fee_bps: u16,
    decimals_in: u8,
    decimals_out: u8,
) -> AmmResult<U256> {
    if decimals_in == decimals_out {
        return calculate_stable_swap_output(reserve_in, reserve_out, amount_in, fee_bps);
    }
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(AmmError::ZeroLiquidity);
    }
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }

    let x_norm = normalize_to_1e18(reserve_in, decimals_in)?;
    let y_norm = normalize_to_1e18(reserve_out, decimals_out)?;
    let max_norm = x_norm.max(y_norm);
    let min_norm = x_norm.min(y_norm);
    if !min_norm.is_zero() && max_norm > min_norm.saturating_mul(U256::from(max_reserve_ratio())) {
        return Err(AmmError::ZeroLiquidity);
    }

    if fee_bps > 10_000 {
        return Err(AmmError::InvalidFee);
    }
    let amount_in_with_fee = solidly_amount_in_after_fee(amount_in, fee_bps)?;
    let dx_norm = normalize_to_1e18(amount_in_with_fee, decimals_in)?;

    if !stable_norm_ok(x_norm) || !stable_norm_ok(y_norm) {
        return Err(AmmError::Overflow);
    }
    let k = invariant(x_norm, y_norm);
    let x_new = x_norm.checked_add(dx_norm).ok_or(AmmError::Overflow)?;
    if !stable_norm_ok(x_new) {
        return Err(AmmError::Overflow);
    }
    let y_new = newton_raphson_solve_y(x_new, k, y_norm)?;
    let dy = y_norm.checked_sub(y_new).ok_or(AmmError::Underflow)?;

    denormalize_from_1e18(dy, decimals_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Velodrome `Pool.getAmountOut` deducts the fee as `amountIn - (amountIn * fee) / 10000`
    /// (floor the fee, then subtract). The kernel must match this BYTE-EXACT — the old
    /// `amountIn * (10000 - fee) / 10000` form floors the product and under-quotes by up to 1 wei.
    #[test]
    fn solidly_fee_matches_velodrome_subtract_form() {
        for (amt, fee) in [
            (3u64, 1u16),
            (10_001, 3),
            (99_999, 5),
            (7, 9_999),
            (1_000_003, 30),
        ] {
            let a = U256::from(amt);
            let expected = a - (a * U256::from(fee as u64)) / U256::from(10_000u64);
            assert_eq!(
                solidly_amount_in_after_fee(a, fee).unwrap(),
                expected,
                "amount_in={amt} fee_bps={fee}: kernel must match Velodrome amountIn-(amountIn*fee)/10000"
            );
        }
    }

    /// Manual T2.3 ROI probe (RELEASE only):
    /// `cargo test -p engine-amm --release solidly_stable::tests::solidly_k_cache_ceiling -- --ignored --nocapture`
    ///
    /// Times the size-independent `invariant()` (the only thing a k-cache could save) against the
    /// full quote. Unlike Curve's D (a Newton solve = 39%), Solidly's `k` is a closed form, so this
    /// ratio is the *ceiling* on any k-cache win — used to decide the wiring isn't worth the churn.
    #[test]
    #[ignore]
    fn solidly_k_cache_ceiling() {
        use std::time::Instant;
        let ri = U256::from(1_000_000u64) * E18;
        let ro = U256::from(1_100_000u64) * E18; // mildly imbalanced → realistic Newton work
        let iters = 1_000_000u64;
        let base_amt = U256::from(1_000u64) * E18;
        let step = E18;
        // Full quote (recomputes k each call).
        let t0 = Instant::now();
        let mut acc = U256::ZERO;
        for k in 0..iters {
            let amt = base_amt + step * U256::from(k);
            if let Ok(o) = calculate_stable_swap_output(ri, ro, amt, 4) {
                acc ^= o;
            }
        }
        let full = t0.elapsed();
        // invariant() alone (what the cache would remove).
        let t1 = Instant::now();
        let mut acc2 = U512::ZERO;
        for k in 0..iters {
            let bump = U256::from(k % 7);
            acc2 ^= invariant(ri + bump, ro);
        }
        let inv = t1.elapsed();
        println!(
            "solidly full {:.0} ns/q, invariant() {:.0} ns/q => k-cache ceiling ~{:.1}% (acc {} {})",
            full.as_nanos() as f64 / iters as f64,
            inv.as_nanos() as f64 / iters as f64,
            100.0 * inv.as_nanos() as f64 / full.as_nanos() as f64,
            acc,
            acc2
        );
    }

    #[test]
    fn zero_reserves_returns_zero_liquidity() {
        assert!(matches!(
            calculate_stable_swap_output(U256::ZERO, U256::from(1u64), U256::from(1u64), 4),
            Err(AmmError::ZeroLiquidity)
        ));
    }

    #[test]
    fn crafted_oversized_reserve_fails_closed_no_wrap() {
        // 1e37 > MAX_STABLE_NORM (1e36): must fail closed (Overflow), NOT wrap the U512 x³y
        // invariant into a small phantom-profitable quote.
        // Balanced oversized pair (ratio 1:1 passes the imbalance check, so we actually reach
        // the magnitude guard rather than tripping ZeroLiquidity first).
        let huge = U256::from(10u64).pow(U256::from(37));
        assert!(matches!(
            calculate_stable_swap_output(huge, huge, U256::from(10u64).pow(U256::from(18)), 4),
            Err(AmmError::Overflow)
        ));
        // A genuinely large pool (~1e27 normalized ≈ $1B at 18dp) must STILL quote — the guard
        // only rejects the unreachable-for-real-pools regime.
        let big_ok = U256::from(10u64).pow(U256::from(27));
        let r =
            calculate_stable_swap_output(big_ok, big_ok, U256::from(10u64).pow(U256::from(18)), 4);
        assert!(r.is_ok(), "normal large pool must still quote: {r:?}");
    }

    #[test]
    fn zero_amount_returns_zero() {
        assert_eq!(
            calculate_stable_swap_output(
                U256::from(1_000_000u64),
                U256::from(1_000_000u64),
                U256::ZERO,
                4
            ),
            Ok(U256::ZERO)
        );
    }

    #[test]
    fn invalid_fee_rejected() {
        assert!(matches!(
            calculate_stable_swap_output(
                U256::from(1_000_000u64),
                U256::from(1_000_000u64),
                U256::from(1_000u64),
                10_001
            ),
            Err(AmmError::InvalidFee)
        ));
    }

    #[test]
    fn balanced_pool_produces_output() {
        let r = U256::from(1_000_000u64) * E18;
        let amount_in = U256::from(1_000u64) * E18;
        let out = calculate_stable_swap_output(r, r, amount_in, 4).unwrap();
        assert!(out > U256::ZERO);
        // For a stable pool near equilibrium, output ≈ input (minus fee + curvature).
        assert!(out < amount_in);
    }

    #[test]
    fn a5_input_just_above_max_stable_norm_fails_closed() {
        // A5 lock: the magnitude guard is fail-CLOSED at exactly MAX_STABLE_NORM (1e36). One wei
        // above the bound on either reserve (or the post-swap balance) must return Overflow, never
        // a wrapped phantom quote. 1e36 + 1 is balanced (1:1) so it clears the imbalance check and
        // actually reaches the magnitude guard.
        let over = U256::from(MAX_STABLE_NORM) + U256::ONE;
        assert_eq!(
            calculate_stable_swap_output(over, over, U256::from(10u64).pow(U256::from(18)), 4),
            Err(AmmError::Overflow),
            "reserve 1e36+1 must fail closed",
        );
        // The bound itself is inclusive: `stable_norm_ok` accepts exactly 1e36 and rejects 1e36+1.
        assert!(
            stable_norm_ok(U256::from(MAX_STABLE_NORM)),
            "1e36 must be accepted (inclusive)"
        );
        assert!(!stable_norm_ok(over), "1e36+1 must be rejected");
        // A genuinely large but in-band pool (and its post-swap balance) still quotes.
        let big = U256::from(10u64).pow(U256::from(30));
        assert!(
            calculate_stable_swap_output(big, big, U256::from(10u64).pow(U256::from(18)), 4)
                .is_ok(),
            "large in-band pool must still quote",
        );
    }

    #[test]
    fn a5_normal_swap_matches_expected_output_exactly() {
        // A5 lock: a normal, deterministic 18-dp stable swap returns a fixed, byte-exact quote.
        // Pins the unscaled-U512-Newton output so any accidental change to the invariant scaling
        // (which would silently shift the quote) is caught. Balanced 1M:1M pool, 1000 in, 4bps fee.
        let r = U256::from(1_000_000u64) * E18;
        let amount_in = U256::from(1_000u64) * E18;
        let out = calculate_stable_swap_output(r, r, amount_in, 4).unwrap();
        assert_eq!(
            out,
            U256::from(999_599_999_500_799_520_626u128),
            "stable swap quote must be byte-exact (unscaled invariant == scaled, per A5)",
        );
    }

    #[test]
    fn imbalanced_pool_rejected() {
        let big = U256::from(1_000_000u64) * E18;
        let tiny = U256::from(1u64) * E18;
        assert!(matches!(
            calculate_stable_swap_output(big, tiny, U256::from(1_000u64) * E18, 4),
            Err(AmmError::ZeroLiquidity)
        ));
    }

    #[test]
    fn normalized_cross_decimals() {
        // USDC (6dp) → USDe-like (18dp), both near $1.00 with 1M depth.
        let r_in = U256::from(1_000_000u64) * pow10(6);
        let r_out = U256::from(1_000_000u64) * E18;
        let amount_in = U256::from(1_000u64) * pow10(6);
        let out =
            calculate_stable_swap_output_normalized(r_in, r_out, amount_in, 4, 6, 18).unwrap();
        // Output denomination = 18 decimals → roughly 1000 * 1e18, less fee/curvature.
        assert!(out > U256::from(990u64) * E18);
        assert!(out < amount_in.checked_mul(pow10(12)).unwrap());
    }
}

/// K2: randomized property tests for the Solidly stable (`x³y + xy³`) swap kernel.
/// No panic on any input; output is monotonic in amount, bounded by the output reserve, and a
/// lower fee never yields less output.
#[cfg(test)]
mod solidly_differential {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(3000))]

        #[test]
        fn monotonic_and_bounded(
            r_in in 1_000_000_000_000u128..=1_000_000_000_000_000_000_000_000u128,
            ratio in 1u128..=8u128,
            amount in 1u128..=1_000_000_000_000_000_000u128,
            bump in 1u128..=1_000_000_000_000_000_000u128,
            fee_bps in 2u16..1_000u16,
        ) {
            let ri = U256::from(r_in);
            let ro = U256::from(r_in.saturating_mul(ratio));
            let o1 = calculate_stable_swap_output(ri, ro, U256::from(amount), fee_bps);
            let o2 = calculate_stable_swap_output(ri, ro, U256::from(amount) + U256::from(bump), fee_bps);
            if let (Ok(a), Ok(b)) = (&o1, &o2) {
                prop_assert!(b >= a, "more input ⇒ not less output");
                prop_assert!(*a < ro, "output cannot exceed the output reserve");
            }
            let lo = calculate_stable_swap_output(ri, ro, U256::from(amount), fee_bps - 1);
            if let (Ok(a), Ok(c)) = (&o1, &lo) {
                prop_assert!(c >= a, "lower fee ⇒ not less output");
            }
        }
    }
}
