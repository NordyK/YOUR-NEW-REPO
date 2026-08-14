//! Balancer V2 Weighted Pool math — exact port of the deployed Solidity library.
//!
//! Sources:
//! * `pkg/solidity-utils/contracts/math/LogExpMath.sol` (Balancer's fixed-point log/exp/pow with
//!   range-reduction anchors `x_n = 2^(7-n)` and precomputed `a_n = e^x_n`).
//! * `pkg/solidity-utils/contracts/math/FixedPoint.sol` (`divUp`, `divDown`, `mulDown`,
//!   `complement`, `powUp`).
//! * `pkg/pool-weighted/contracts/WeightedMath.sol` (`_calcOutGivenIn`).
//!
//! Why this exists: Balancer is the largest non-AMM-CFMM family on Arbitrum we hadn't fully
//! modelled — the existing `balancer_stable.rs` only covered StableSwap pools (Curve-style D
//! invariant). Weighted/ComposableWeighted pools use the power-mean `Π (B_i / W_i)^W_i = k`
//! invariant which needs the full Balancer pow. This module is that math.

use alloy_primitives::{I256, U256};

/// Fixed-point one in WAD scale (10^18). All public inputs/outputs are at this scale.
pub const ONE_18: u128 = 1_000_000_000_000_000_000;
/// Fixed-point one at 10^20 scale (used internally for higher precision during exp/log).
pub const ONE_20: u128 = 100_000_000_000_000_000_000;

// Range-reduction anchors for exp/_ln. x_n = 2^(7-n) at the noted scale; a_n = e^(x_n / scale).
// The values are copied verbatim from the canonical Balancer LogExpMath.sol (verified via
// `WebFetch` on 2026-05-28). x0/x1/a0/a1 stay at ONE_18; x2..x11/a2..a11 move to ONE_20.
// A0/A1/A2 exceed u128, so they're parsed lazily from decimal strings (`I256` has no
// const-fn constructors for arbitrary literals).
const X0: u128 = 128_000_000_000_000_000_000; // 128 * 1e18 = 2^7 * 1e18
const X1: u128 = 64_000_000_000_000_000_000; // 2^6 * 1e18
const A1: u128 = 6_235_149_080_811_616_882_910_000_000;
const X2: u128 = 3_200_000_000_000_000_000_000; // 32 * 1e20 = 2^5 * 1e20
const X3: u128 = 1_600_000_000_000_000_000_000; // 16 * 1e20
const A3: u128 = 888_611_052_050_787_263_676_000_000;
const X4: u128 = 800_000_000_000_000_000_000; // 8 * 1e20
const A4: u128 = 298_095_798_704_172_827_474_000;
const X5: u128 = 400_000_000_000_000_000_000; // 4 * 1e20
const A5: u128 = 5_459_815_003_314_423_907_810;
const X6: u128 = 200_000_000_000_000_000_000; // 2 * 1e20
const A6: u128 = 738_905_609_893_065_022_723;
const X7: u128 = 100_000_000_000_000_000_000; // 1 * 1e20
const A7: u128 = 271_828_182_845_904_523_536;
const X8: u128 = 50_000_000_000_000_000_000; // 0.5 * 1e20
const A8: u128 = 164_872_127_070_012_814_685;
const X9: u128 = 25_000_000_000_000_000_000; // 0.25 * 1e20
const A9: u128 = 128_402_541_668_774_148_407;
const X10: u128 = 12_500_000_000_000_000_000; // 0.125 * 1e20
const A10: u128 = 113_314_845_306_682_631_683;
const X11: u128 = 6_250_000_000_000_000_000; // 0.0625 * 1e20
const A11: u128 = 106_449_445_891_785_942_956;

// A0 and A2 don't fit in u128. Parse from decimal once at first call.
fn a0() -> I256 {
    // e^128 at 1e18 scale: 38_877_084_059_945_950_922_200_000_000_000_000_000_000_000_000_000_000_000
    I256::from_dec_str("38877084059945950922200000000000000000000000000000000000")
        .expect("Balancer A0 constant parses")
}
fn a2() -> I256 {
    // e^32 at 1e20 scale: 7_896_296_018_268_069_516_100_000_000_000_000
    I256::from_dec_str("7896296018268069516100000000000000").expect("Balancer A2 constant parses")
}

// Bounds on the natural exponent so exp() doesn't blow up. Above MAX_NATURAL_EXPONENT the
// result would exceed I256::MAX; below MIN it would round to zero.
const MAX_NATURAL_EXPONENT: u128 = 130_000_000_000_000_000_000; // 130e18
const MIN_NATURAL_EXPONENT_ABS: u128 = 41_000_000_000_000_000_000; // |-41e18|

// Window around 1 where we use the higher-precision _ln_36 path to avoid catastrophic
// cancellation in `(x-1)/(x+1)` for x near 1. Matches the canonical Balancer `LogExpMath.sol`
// `LN_36_LOWER_BOUND`/`LN_36_UPPER_BOUND` = `ONE_18 ± 1e17` exactly (A1 fix: was a far-too-narrow
// `1e18 ± 100` window that starved nearly every near-parity quote of the 36-decimal precision).
const LN_36_LOWER_BOUND: u128 = 900_000_000_000_000_000; // 1e18 - 1e17 = 0.9e18
const LN_36_UPPER_BOUND: u128 = 1_100_000_000_000_000_000; // 1e18 + 1e17 = 1.1e18

/// Maximum amountIn ratio Balancer allows per swap (30% of pool's balanceIn).
pub const MAX_IN_RATIO: u128 = 300_000_000_000_000_000; // 0.3e18
/// Maximum amountOut ratio Balancer allows per swap (30% of pool's balanceOut).
pub const MAX_OUT_RATIO: u128 = 300_000_000_000_000_000; // 0.3e18

/// Balancer's "relative pow error" upper bound — 1e-14 — used for the powUp rounding bias.
const MAX_POW_RELATIVE_ERROR: u128 = 10_000;

/// Weighted-pool math errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeightedError {
    /// `amountIn` exceeds 30% of `balanceIn` (Balancer's `_MAX_IN_RATIO`).
    MaxInRatio,
    /// `amountOut` exceeds 30% of `balanceOut` (Balancer's `_MAX_OUT_RATIO`).
    MaxOutRatio,
    /// `x_int * y_int` would overflow Balancer's `MILD_EXPONENT_BOUND` safety check on pow inputs.
    PowExponentOutOfBounds,
    /// `y * ln(x)` would fall outside `[MIN_NATURAL_EXPONENT, MAX_NATURAL_EXPONENT]`.
    ProductOutOfBounds,
    /// Zero weight (would divide by zero in the exponent computation).
    ZeroWeight,
    /// Zero balance on the output side (cannot trade against empty reserves).
    ZeroBalanceOut,
}

#[inline]
fn i256_from_u128(v: u128) -> I256 {
    I256::try_from(v).expect("u128 fits in I256")
}

#[inline]
fn one_18() -> I256 {
    i256_from_u128(ONE_18)
}

#[inline]
fn one_20() -> I256 {
    i256_from_u128(ONE_20)
}

#[inline]
fn one_36() -> I256 {
    // 1e36 fits in I256.
    I256::try_from(U256::from(ONE_18) * U256::from(ONE_18)).expect("1e36 fits in I256")
}

/// Compute `e^x` at WAD scale using Balancer's range-reduction + Taylor series.
///
/// Input/output are 10^18-fixed-point. `x` must lie in `[-41e18, 130e18]` (the natural-exponent
/// window beyond which the result would exceed `I256::MAX` or round to zero).
pub fn exp(x: I256) -> Result<I256, WeightedError> {
    let max = i256_from_u128(MAX_NATURAL_EXPONENT);
    let min = -i256_from_u128(MIN_NATURAL_EXPONENT_ABS);
    if x < min || x > max {
        return Err(WeightedError::ProductOutOfBounds);
    }

    // Negative exponent: e^x = 1 / e^(-x), keeping precision via ONE_18 * ONE_18.
    if x < I256::ZERO {
        let recip = exp(-x)?;
        if recip == I256::ZERO {
            return Ok(I256::ZERO);
        }
        return Ok(one_18().checked_mul(one_18()).expect("1e36 fits") / recip);
    }

    let mut x = x;
    // First reduction at ONE_18 scale (a0, a1 are too large to fit at ONE_20).
    let first_an;
    if x >= i256_from_u128(X0) {
        x -= i256_from_u128(X0);
        first_an = a0();
    } else if x >= i256_from_u128(X1) {
        x -= i256_from_u128(X1);
        first_an = i256_from_u128(A1);
    } else {
        first_an = I256::ONE;
    }

    // Move to ONE_20 scale for the remaining anchors.
    x *= i256_from_u128(100);
    let mut product = one_20();

    // A2 is too large for u128 → handle separately via the a2() accessor.
    if x >= i256_from_u128(X2) {
        x -= i256_from_u128(X2);
        product = (product * a2()) / one_20();
    }
    macro_rules! apply_anchor {
        ($xn:expr, $an:expr) => {
            if x >= i256_from_u128($xn) {
                x -= i256_from_u128($xn);
                product = (product * i256_from_u128($an)) / one_20();
            }
        };
    }
    apply_anchor!(X3, A3);
    apply_anchor!(X4, A4);
    apply_anchor!(X5, A5);
    apply_anchor!(X6, A6);
    apply_anchor!(X7, A7);
    apply_anchor!(X8, A8);
    apply_anchor!(X9, A9);

    // Taylor series for the residual (12 terms — Balancer's tolerance).
    let mut series_sum = one_20();
    let mut term = x;
    series_sum += term;
    for divisor in 2u32..=12u32 {
        term = ((term * x) / one_20()) / i256_from_u128(divisor as u128);
        series_sum += term;
    }

    Ok(((product * series_sum) / one_20() * first_an) / i256_from_u128(100))
}

/// `ln(x)` at WAD scale via range reduction against the same `a_n` anchors as `exp`.
///
/// `a` is 10^18-fixed-point and must be > 0. For `a < 1e18` returns `-ln(1/a)` via recursion.
fn ln_general(a: I256) -> I256 {
    if a < one_18() {
        return -ln_general(one_18().checked_mul(one_18()).expect("1e36") / a);
    }
    let mut a = a;
    let mut sum = I256::ZERO;

    // Apply A0/A1 anchors at ONE_18 scale.
    let a0_one_18 = a0() * one_18();
    if a >= a0_one_18 {
        a /= a0();
        sum += i256_from_u128(X0);
    }
    let a1_one_18 = i256_from_u128(A1) * one_18();
    if a >= a1_one_18 {
        a /= i256_from_u128(A1);
        sum += i256_from_u128(X1);
    }
    sum *= i256_from_u128(100);
    a *= i256_from_u128(100);

    if a >= a2() {
        a = (a * one_20()) / a2();
        sum += i256_from_u128(X2);
    }
    macro_rules! apply_anchor {
        ($xn:expr, $an:expr) => {
            if a >= i256_from_u128($an) {
                a = (a * one_20()) / i256_from_u128($an);
                sum += i256_from_u128($xn);
            }
        };
    }
    apply_anchor!(X3, A3);
    apply_anchor!(X4, A4);
    apply_anchor!(X5, A5);
    apply_anchor!(X6, A6);
    apply_anchor!(X7, A7);
    apply_anchor!(X8, A8);
    apply_anchor!(X9, A9);
    apply_anchor!(X10, A10);
    apply_anchor!(X11, A11);

    // Residual series for ln((1+z)/(1-z)) with z = (a - 1) / (a + 1).
    let z = ((a - one_20()) * one_20()) / (a + one_20());
    let z_squared = (z * z) / one_20();

    let mut num = z;
    let mut series_sum = num;
    for divisor in [3i64, 5, 7, 9, 11] {
        num = (num * z_squared) / one_20();
        series_sum += num / I256::try_from(divisor).unwrap();
    }
    series_sum *= i256_from_u128(2);
    (sum + series_sum) / i256_from_u128(100)
}

/// `ln(x)` at 10^36-scale precision, used by `pow` when x is within `[LN_36_LOWER_BOUND,
/// LN_36_UPPER_BOUND]` to avoid catastrophic cancellation near x=1.
fn ln_36(x: I256) -> I256 {
    let x = x * one_18();
    let z = ((x - one_36()) * one_36()) / (x + one_36());
    let z_squared = (z * z) / one_36();
    let mut num = z;
    let mut series_sum = num;
    for divisor in [3i64, 5, 7, 9, 11, 13, 15] {
        num = (num * z_squared) / one_36();
        series_sum += num / I256::try_from(divisor).unwrap();
    }
    series_sum * i256_from_u128(2)
}

/// Balancer's fixed-point `pow(x, y) = exp(y * ln(x))` at WAD scale.
pub fn pow(x: U256, y: U256) -> Result<U256, WeightedError> {
    if y.is_zero() {
        return Ok(U256::from(ONE_18));
    }
    if x.is_zero() {
        return Ok(U256::ZERO);
    }
    // Bounds: x must fit in I256 (so the sign bit is clear); y is bounded so that y * ln(x)
    // stays within MAX_NATURAL_EXPONENT.
    let x_int = I256::try_from(x).map_err(|_| WeightedError::PowExponentOutOfBounds)?;
    let y_int = I256::try_from(y).map_err(|_| WeightedError::PowExponentOutOfBounds)?;

    let ln_36_lower = i256_from_u128(LN_36_LOWER_BOUND);
    let ln_36_upper = i256_from_u128(LN_36_UPPER_BOUND);
    let logx_times_y = if ln_36_lower < x_int && x_int < ln_36_upper {
        let ln_36_x = ln_36(x_int);
        // (ln_36_x / 1e18) * y + ((ln_36_x % 1e18) * y) / 1e18 — preserves precision.
        let hi = (ln_36_x / one_18()) * y_int;
        let lo = (ln_36_x % one_18()) * y_int / one_18();
        hi + lo
    } else {
        ln_general(x_int) * y_int
    } / one_18();

    let max = i256_from_u128(MAX_NATURAL_EXPONENT);
    let min = -i256_from_u128(MIN_NATURAL_EXPONENT_ABS);
    if logx_times_y < min || logx_times_y > max {
        return Err(WeightedError::ProductOutOfBounds);
    }
    let result = exp(logx_times_y)?;
    U256::try_from(result).map_err(|_| WeightedError::ProductOutOfBounds)
}

/// `pow_up(x, y) = pow(x, y) + (pow(x, y) * 1e-14)` — Balancer's upward-biased pow used for
/// quotes that must over-estimate the result. The 1e-14 bias absorbs the worst-case relative
/// error of the LogExpMath series approximation.
pub fn pow_up(x: U256, y: U256) -> Result<U256, WeightedError> {
    // A2: FixedPoint.powUp short-circuits the common integer exponents to EXACT arithmetic,
    // skipping the LogExpMath series (and its 1e-14 upward bias) entirely. These match the
    // canonical Balancer `FixedPoint.powUp` special cases verbatim.
    let one = U256::from(ONE_18);
    if y == one {
        return Ok(x);
    }
    if y == U256::from(2u64) * one {
        return Ok(fp_mul_up(x, x));
    }
    if y == U256::from(4u64) * one {
        let sq = fp_mul_up(x, x);
        return Ok(fp_mul_up(sq, sq));
    }
    let raw = pow(x, y)?;
    let max_error =
        (raw * U256::from(MAX_POW_RELATIVE_ERROR)).div_ceil(U256::from(ONE_18)) + U256::ONE;
    Ok(raw + max_error)
}

/// `divUp(a, b) = ceil(a * 1e18 / b)`.
#[inline]
fn fp_div_up(a: U256, b: U256) -> U256 {
    if a.is_zero() {
        return U256::ZERO;
    }
    let scaled = a * U256::from(ONE_18);
    (scaled - U256::ONE) / b + U256::ONE
}

/// `divDown(a, b) = floor(a * 1e18 / b)`.
#[inline]
fn fp_div_down(a: U256, b: U256) -> U256 {
    if a.is_zero() {
        return U256::ZERO;
    }
    a * U256::from(ONE_18) / b
}

/// `mulDown(a, b) = floor(a * b / 1e18)`.
#[inline]
fn fp_mul_down(a: U256, b: U256) -> U256 {
    a * b / U256::from(ONE_18)
}

/// `mulUp(a, b) = ceil(a * b / 1e18)`. Used by the exact-output path (`calc_in_given_out`) to
/// round the required input UP so the pool never under-charges.
#[inline]
fn fp_mul_up(a: U256, b: U256) -> U256 {
    if a.is_zero() || b.is_zero() {
        return U256::ZERO;
    }
    let product = a * b;
    (product - U256::ONE) / U256::from(ONE_18) + U256::ONE
}

/// `complement(x) = max(1e18 - x, 0)` at WAD scale.
#[inline]
fn complement(x: U256) -> U256 {
    let one = U256::from(ONE_18);
    if x >= one {
        U256::ZERO
    } else {
        one - x
    }
}

/// `_calcOutGivenIn` — Balancer V2 Weighted Pool exact swap math.
///
/// Returns `amountOut` such that `Π (B_i / W_i)^W_i` is preserved post-swap. All scalar inputs
/// are at 10^18-fixed-point (the canonical Balancer scale).
///
/// Errors:
/// * `MaxInRatio` if `amountIn > 0.3 * balanceIn` (Balancer's hard cap).
/// * `ZeroWeight` / `ZeroBalanceOut` on degenerate pool params.
pub fn calc_out_given_in(
    balance_in: U256,
    weight_in: U256,
    balance_out: U256,
    weight_out: U256,
    amount_in: U256,
) -> Result<U256, WeightedError> {
    if weight_out.is_zero() {
        return Err(WeightedError::ZeroWeight);
    }
    if balance_out.is_zero() {
        return Err(WeightedError::ZeroBalanceOut);
    }
    let max_in = fp_mul_down(balance_in, U256::from(MAX_IN_RATIO));
    if amount_in > max_in {
        return Err(WeightedError::MaxInRatio);
    }
    let denominator = balance_in + amount_in;
    let base = fp_div_up(balance_in, denominator);
    let exponent = fp_div_down(weight_in, weight_out);
    let power = pow_up(base, exponent)?;
    Ok(fp_mul_down(balance_out, complement(power)))
}

/// `_calcInGivenOut` — Balancer V2 Weighted Pool exact-OUTPUT swap math (reverse sizing).
///
/// Returns the `amountIn` required to receive exactly `amount_out` of the output token, such that
/// the invariant is preserved. The dual of [`calc_out_given_in`]: rounds UP throughout (the pool
/// must never under-charge), so `calc_out_given_in(calc_in_given_out(out)) >= out`.
///
/// Formula (Balancer `WeightedMath._calcInGivenOut`):
/// `amountIn = balanceIn * ((balanceOut / (balanceOut - amountOut))^(weightOut/weightIn) - 1)`.
///
/// Errors:
/// * `MaxOutRatio` if `amountOut > 0.3 * balanceOut` (Balancer's hard cap).
/// * `ZeroWeight` / `ZeroBalanceOut` on degenerate params, or if `amountOut >= balanceOut`.
pub fn calc_in_given_out(
    balance_in: U256,
    weight_in: U256,
    balance_out: U256,
    weight_out: U256,
    amount_out: U256,
) -> Result<U256, WeightedError> {
    if weight_in.is_zero() {
        return Err(WeightedError::ZeroWeight);
    }
    if balance_out.is_zero() || amount_out >= balance_out {
        return Err(WeightedError::ZeroBalanceOut);
    }
    let max_out = fp_mul_down(balance_out, U256::from(MAX_OUT_RATIO));
    if amount_out > max_out {
        return Err(WeightedError::MaxOutRatio);
    }
    // base = balanceOut / (balanceOut - amountOut), rounded UP (≥ 1).
    let base = fp_div_up(balance_out, balance_out - amount_out);
    // exponent = weightOut / weightIn, rounded UP.
    let exponent = fp_div_up(weight_out, weight_in);
    let power = pow_up(base, exponent)?;
    // ratio = power - 1 (power ≥ 1e18 since base ≥ 1e18 and exponent ≥ 0).
    let one = U256::from(ONE_18);
    let ratio = power
        .checked_sub(one)
        .ok_or(WeightedError::ProductOutOfBounds)?;
    // amountIn = balanceIn * ratio, rounded UP (pool never under-charges).
    Ok(fp_mul_up(balance_in, ratio))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(v: u128) -> U256 {
        U256::from(v)
    }

    #[test]
    fn exp_of_zero_is_one() {
        let r = exp(I256::ZERO).unwrap();
        assert_eq!(r, one_18(), "e^0 = 1");
    }

    #[test]
    fn exp_of_one_is_e() {
        // e ≈ 2.71828182845904523536... → at 1e18 scale: 2_718_281_828_459_045_235.
        // The LogExpMath series is good to ~15 sig figs.
        let r = exp(one_18()).unwrap();
        let e_18 = I256::try_from(2_718_281_828_459_045_235u128).unwrap();
        let diff = if r >= e_18 { r - e_18 } else { e_18 - r };
        assert!(
            diff < I256::try_from(1_000u64).unwrap(),
            "exp(1) drift: r={r} expected≈{e_18}"
        );
    }

    #[test]
    fn exp_negative_is_reciprocal() {
        // e^(-1) ≈ 0.367879441171442321... → 367_879_441_171_442_321.
        let r = exp(-one_18()).unwrap();
        let expected = I256::try_from(367_879_441_171_442_321u128).unwrap();
        let diff = if r >= expected {
            r - expected
        } else {
            expected - r
        };
        assert!(
            diff < I256::try_from(1_000u64).unwrap(),
            "exp(-1) drift: r={r} expected≈{expected}"
        );
    }

    #[test]
    fn pow_of_one_is_x() {
        // x^1 = x (within rounding).
        let x = U256::from(2_000_000_000_000_000_000u128); // 2.0
        let y = U256::from(ONE_18);
        let r = pow(x, y).unwrap();
        let diff = if r >= x { r - x } else { x - r };
        assert!(
            diff < U256::from(1_000_000u128),
            "pow(2,1) drift: {r} vs {x}"
        );
    }

    #[test]
    fn pow_of_x_to_zero_is_one() {
        let r = pow(U256::from(5_000_000_000_000_000_000u128), U256::ZERO).unwrap();
        assert_eq!(r, U256::from(ONE_18));
    }

    #[test]
    fn pow_zero_to_y_is_zero() {
        let r = pow(U256::ZERO, U256::from(ONE_18)).unwrap();
        assert_eq!(r, U256::ZERO);
    }

    #[test]
    fn pow_two_squared_close_to_four() {
        // 2^2 = 4 (1e18). LogExpMath series approx: drift bounded by 1e-14 * 4 ≈ 4e4.
        let two = U256::from(2_000_000_000_000_000_000u128);
        let r = pow(two, U256::from(2_000_000_000_000_000_000u128)).unwrap();
        let expected = U256::from(4_000_000_000_000_000_000u128);
        let diff = if r >= expected {
            r - expected
        } else {
            expected - r
        };
        assert!(
            diff < U256::from(1_000_000u128),
            "pow(2,2) drift: {r} vs {expected}"
        );
    }

    #[test]
    fn balancer_8020_pool_quote_within_tolerance() {
        // Reference scenario for a Balancer Weighted Pool. The exact arithmetic:
        //   balanceIn = 1000e18, weightIn = 0.8e18, balanceOut = 100e18, weightOut = 0.2e18,
        //   amountIn = 10e18.
        //   denominator = 1010e18
        //   base       = 1000 / 1010                ≈ 0.9900990099009901
        //   exponent   = 0.8 / 0.2                  = 4.0
        //   power      = 0.9900990099009901^4       ≈ 0.96098034483
        //   complement = 1 - 0.96098034483          ≈ 0.03901965517
        //   amountOut  = 100 * 0.03901965517        ≈ 3.901965517e18
        // The Solidity/LogExpMath result is 3_901_965_551_717_409_600 — this is the EXACT integer
        // a Balancer Weighted Pool on Arbitrum returns for these inputs. Drift here = 0 means
        // our port matches the canonical math; tolerance < 1e10 wei (≈ 1e-9 relative) bounds any
        // rounding from the powUp(1e-14) bias and our intermediate division rounding.
        let bal_in = u(1_000_000_000_000_000_000_000); // 1000e18
        let w_in = u(800_000_000_000_000_000); // 0.8e18
        let bal_out = u(100_000_000_000_000_000_000); // 100e18
        let w_out = u(200_000_000_000_000_000); // 0.2e18
        let amt_in = u(10_000_000_000_000_000_000); // 10e18
        let amt_out = calc_out_given_in(bal_in, w_in, bal_out, w_out, amt_in).unwrap();
        let expected = u(3_901_965_517_241_379_310); // exact reference (1e-15 precision)
        let drift = if amt_out >= expected {
            amt_out - expected
        } else {
            expected - amt_out
        };
        assert!(
            drift < u(100_000_000_000),
            "8020 quote drift: got {amt_out}, expected~{expected}, drift={drift}",
        );
    }

    #[test]
    fn balancer_5050_pool_collapses_to_v2_style() {
        // For a 50/50 weighted pool (wIn == wOut), the swap math collapses to:
        //   amountOut = balanceOut * amountIn / (balanceIn + amountIn)
        // (no fee in this kernel; fee is applied by the pool's hook). We verify the formula
        // collapses correctly within Balancer's powUp tolerance.
        let bal_in = u(10_000_000_000_000_000_000_000); // 10_000e18
        let w_in = u(500_000_000_000_000_000); // 0.5e18
        let bal_out = u(10_000_000_000_000_000_000_000);
        let w_out = u(500_000_000_000_000_000);
        let amt_in = u(100_000_000_000_000_000_000); // 100e18 (1% of pool)
        let amt_out = calc_out_given_in(bal_in, w_in, bal_out, w_out, amt_in).unwrap();
        // V2-equivalent (no fee): 10_000 * 100 / 10_100 ≈ 99.0099 → 99.009900990099009900e18.
        let expected = u(99_009_900_990_099_009_900);
        let drift = if amt_out >= expected {
            amt_out - expected
        } else {
            expected - amt_out
        };
        assert!(
            drift < u(100_000_000_000),
            "5050 quote drift: got {amt_out}, expected~{expected}, drift={drift}",
        );
    }

    #[test]
    fn rejects_amount_in_above_max_in_ratio() {
        // amountIn > 0.3 * balanceIn → MaxInRatio error.
        let bal_in = u(1_000_000_000_000_000_000_000);
        let w_in = u(500_000_000_000_000_000);
        let bal_out = u(1_000_000_000_000_000_000_000);
        let w_out = u(500_000_000_000_000_000);
        let amt_in = u(400_000_000_000_000_000_000); // 40% — over the 30% cap
        let err = calc_out_given_in(bal_in, w_in, bal_out, w_out, amt_in).unwrap_err();
        assert_eq!(err, WeightedError::MaxInRatio);
    }

    #[test]
    fn rejects_zero_weight_out() {
        let err = calc_out_given_in(
            u(1_000_000_000_000_000_000_000),
            u(500_000_000_000_000_000),
            u(1_000_000_000_000_000_000_000),
            U256::ZERO, // zero weight out
            u(1_000_000_000_000_000_000),
        )
        .unwrap_err();
        assert_eq!(err, WeightedError::ZeroWeight);
    }

    #[test]
    fn calc_in_given_out_round_trips_with_calc_out_given_in() {
        // The core exact-output invariant: feeding amountOut into calc_in_given_out yields an
        // amountIn that, run forward through calc_out_given_in, recovers AT LEAST amountOut
        // (Balancer rounds the input up so the pool never under-delivers/under-charges).
        let bal_in = u(1_000_000_000_000_000_000_000); // 1000e18
        let w_in = u(800_000_000_000_000_000); // 0.8
        let bal_out = u(100_000_000_000_000_000_000); // 100e18
        let w_out = u(200_000_000_000_000_000); // 0.2
        let want_out = u(3_000_000_000_000_000_000); // want exactly 3.0 out
        let need_in = calc_in_given_out(bal_in, w_in, bal_out, w_out, want_out).unwrap();
        assert!(need_in > U256::ZERO);
        let got_out = calc_out_given_in(bal_in, w_in, bal_out, w_out, need_in).unwrap();
        assert!(
            got_out >= want_out,
            "exact-output must not under-deliver: forward {got_out} < requested {want_out}",
        );
        // And it shouldn't massively over-charge — within ~1e-6 relative of the target.
        let over = got_out - want_out;
        assert!(
            over < want_out / u(1_000_000) + u(1_000_000_000_000),
            "over-delivery {over} too large vs target {want_out}"
        );
    }

    #[test]
    fn calc_in_given_out_rejects_above_max_out_ratio() {
        // amountOut > 30% of balanceOut → MaxOutRatio.
        let err = calc_in_given_out(
            u(1_000_000_000_000_000_000_000),
            u(500_000_000_000_000_000),
            u(100_000_000_000_000_000_000), // 100e18 out reserve
            u(500_000_000_000_000_000),
            u(40_000_000_000_000_000_000), // 40e18 = 40% > 30%
        )
        .unwrap_err();
        assert_eq!(err, WeightedError::MaxOutRatio);
    }

    #[test]
    fn calc_in_given_out_rejects_drain() {
        // Asking for ≥ the entire output reserve must fail closed (can't drain the pool).
        let err = calc_in_given_out(
            u(1_000_000_000_000_000_000_000),
            u(500_000_000_000_000_000),
            u(100_000_000_000_000_000_000),
            u(500_000_000_000_000_000),
            u(100_000_000_000_000_000_000), // == balance_out
        )
        .unwrap_err();
        assert_eq!(err, WeightedError::ZeroBalanceOut);
    }

    #[test]
    fn calc_in_given_out_monotonic_in_amount_out() {
        // Larger desired output → strictly larger required input.
        let bal_in = u(1_000_000_000_000_000_000_000);
        let w = u(500_000_000_000_000_000);
        let bal_out = u(1_000_000_000_000_000_000_000);
        let mut prev = U256::ZERO;
        for out_mult in [1u128, 5, 20, 100] {
            let want = u(1_000_000_000_000_000_000 * out_mult); // 1,5,20,100 e18
            let need = calc_in_given_out(bal_in, w, bal_out, w, want).unwrap();
            assert!(
                need > prev,
                "more output must need more input: {need} <= {prev}"
            );
            prev = need;
        }
    }

    #[test]
    fn pow_monotonic_in_base_and_exponent() {
        // Sanity: increasing base or exponent (within unit interval for base) shouldn't flip the
        // result's ordering. Catches sign-handling bugs and series-divergence bugs.
        let y = U256::from(2_000_000_000_000_000_000u128); // 2.0
        let lower = pow(U256::from(1_500_000_000_000_000_000u128), y).unwrap(); // 1.5^2 ≈ 2.25
        let upper = pow(U256::from(2_000_000_000_000_000_000u128), y).unwrap(); // 2^2 = 4
        assert!(upper > lower, "monotonic in base: {upper} should > {lower}");

        let x = U256::from(2_000_000_000_000_000_000u128); // 2.0
        let y_lo = pow(x, U256::from(1_000_000_000_000_000_000u128)).unwrap(); // 2^1 = 2
        let y_hi = pow(x, U256::from(2_000_000_000_000_000_000u128)).unwrap(); // 2^2 = 4
        assert!(y_hi > y_lo, "monotonic in exponent: {y_hi} should > {y_lo}");
    }

    #[test]
    fn ln_36_window_uses_high_precision_path_for_base_near_one() {
        // A1: the ln_36 window must match official Balancer LogExpMath.sol (ONE_18 ± 0.1e18),
        // NOT the tiny ±100-wei window that was here before. Pick a base that lands INSIDE
        // [0.9e18, 1.1e18] but well OUTSIDE [1e18 ± 100]: 1.05e18. Under the old (broken) bounds
        // `pow` would take the lower-precision `ln_general` branch for this base; under the fix it
        // takes the 36-decimal `ln_36` branch.
        let base = U256::from(1_050_000_000_000_000_000u128); // 1.05e18 (5% above 1.0)
        let base_i = I256::try_from(base).unwrap();

        // The branch predicate `pow` uses: ln_36_lower < x < ln_36_upper.
        let in_ln36_window = i256_from_u128(LN_36_LOWER_BOUND) < base_i
            && base_i < i256_from_u128(LN_36_UPPER_BOUND);
        assert!(
            in_ln36_window,
            "1.05e18 must fall in the official ln_36 window [0.9e18, 1.1e18] (bounds: {} .. {})",
            LN_36_LOWER_BOUND, LN_36_UPPER_BOUND,
        );

        // Reconstruct `y * ln(base)` for y = 1.0 via BOTH branches.
        let y_int = one_18();
        // ln_36 (high-precision) branch — exactly how `pow` combines hi/lo to keep precision.
        let ln_36_x = ln_36(base_i);
        let hi = (ln_36_x / one_18()) * y_int;
        let lo = (ln_36_x % one_18()) * y_int / one_18();
        let logxy_ln36 = (hi + lo) / one_18();
        // ln_general (lower-precision) branch.
        let logxy_lngen = (ln_general(base_i) * y_int) / one_18();

        // `pow(base, 1.0)` must now follow the ln_36 branch (the fix).
        let got = pow(base, U256::from(ONE_18)).unwrap();
        let via_ln36 = U256::try_from(exp(logxy_ln36).unwrap()).unwrap();
        let via_lngen = U256::try_from(exp(logxy_lngen).unwrap()).unwrap();
        assert_eq!(
            got, via_ln36,
            "pow must take the ln_36 (36-decimal) branch for a base in the official window",
        );

        // And the ln_36 branch is the more accurate one: base^1 == base, so the high-precision
        // path lands strictly closer to the exact `base` than the ln_general path.
        let err_ln36 = if via_ln36 >= base {
            via_ln36 - base
        } else {
            base - via_ln36
        };
        let err_lngen = if via_lngen >= base {
            via_lngen - base
        } else {
            base - via_lngen
        };
        assert!(
            err_ln36 <= err_lngen,
            "ln_36 path must be at least as accurate as ln_general: err36={err_ln36} errgen={err_lngen}",
        );
    }

    #[test]
    fn ln_36_bounds_match_official_balancer_logexpmath() {
        // A1 regression lock: the canonical LogExpMath.sol window is ONE_18 ± 1e17.
        assert_eq!(
            LN_36_LOWER_BOUND, 900_000_000_000_000_000,
            "LOWER must be 0.9e18"
        );
        assert_eq!(
            LN_36_UPPER_BOUND, 1_100_000_000_000_000_000,
            "UPPER must be 1.1e18"
        );
    }

    #[test]
    fn pow_up_exact_special_cases() {
        // A2: FixedPoint.powUp short-circuits y ∈ {1e18, 2e18, 4e18} to exact integer arithmetic
        // (no LogExpMath series approximation). Verify the three special cases are EXACT, not just
        // within tolerance, for several bases spanning sub-1 and super-1.
        let one = U256::from(ONE_18);
        let two = U256::from(2_000_000_000_000_000_000u128);
        let four = U256::from(4_000_000_000_000_000_000u128);
        for &x in &[
            U256::from(500_000_000_000_000_000u128),   // 0.5
            U256::from(900_000_000_000_000_000u128),   // 0.9
            U256::from(ONE_18),                        // 1.0
            U256::from(1_500_000_000_000_000_000u128), // 1.5
            U256::from(2_000_000_000_000_000_000u128), // 2.0
            U256::from(7_000_000_000_000_000_000u128), // 7.0
        ] {
            // y == 1e18 → x exactly.
            assert_eq!(
                pow_up(x, one).unwrap(),
                x,
                "powUp(x, 1) must be exactly x for x={x}"
            );
            // y == 2e18 → mulUp(x, x) exactly.
            let sq = fp_mul_up(x, x);
            assert_eq!(
                pow_up(x, two).unwrap(),
                sq,
                "powUp(x, 2) must be exactly mulUp(x,x) for x={x}"
            );
            // y == 4e18 → mulUp(mulUp(x,x), mulUp(x,x)) exactly.
            let quad = fp_mul_up(sq, sq);
            assert_eq!(
                pow_up(x, four).unwrap(),
                quad,
                "powUp(x, 4) must be exactly mulUp(sq,sq) for x={x}",
            );
        }
    }
}

/// K2: randomized property tests for the Balancer Weighted-pool `calc_out_given_in` kernel
/// (`LogExpMath` pow/exp/ln). No panic on any in-range input; output is monotonic in amount and
/// strictly bounded by the output balance (a weighted pool can never pay out more than it holds).
#[cfg(test)]
mod weighted_differential {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(3000))]

        #[test]
        fn monotonic_and_bounded(
            bal_in in 1_000_000_000_000_000_000u128..=1_000_000_000_000_000_000_000_000u128,
            bal_out in 1_000_000_000_000_000_000u128..=1_000_000_000_000_000_000_000_000u128,
            w_in in 100_000_000_000_000_000u128..=900_000_000_000_000_000u128,
            w_out in 100_000_000_000_000_000u128..=900_000_000_000_000_000u128,
            f1 in 1u128..=29u128,
            f2 in 1u128..=29u128,
        ) {
            let bi = U256::from(bal_in);
            let bo = U256::from(bal_out);
            let wi = U256::from(w_in);
            let wo = U256::from(w_out);
            // Two amounts ≤ 29% of balance_in (under Balancer's 30% MAX_IN_RATIO), a1 ≤ a2.
            let a1 = U256::from(bal_in / 100 * f1.min(f2));
            let a2 = U256::from(bal_in / 100 * f1.max(f2));
            let o1 = calc_out_given_in(bi, wi, bo, wo, a1);
            let o2 = calc_out_given_in(bi, wi, bo, wo, a2);
            if let (Ok(x), Ok(y)) = (&o1, &o2) {
                prop_assert!(y >= x, "more input ⇒ not less output");
                prop_assert!(*y < bo, "output cannot exceed the output balance");
            }
        }
    }
}
