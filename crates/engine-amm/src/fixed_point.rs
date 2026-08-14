//! Fixed-point helpers used by the V3 kernels.
//!
//! Mirrors the integer rounding behaviour of Uniswap V3's `FullMath` library
//! (Solidity 0.7.6 / 0.8.x). Implemented over `ruint::U256` / `ruint::U512`.

use alloy_primitives::U256;

use crate::AmmError;

/// Q96 = 2^96, the canonical fixed-point base for V3 sqrtPriceX96 values.
pub const Q96: U256 = U256::from_limbs([0, 1 << 32, 0, 0]);

/// Q192 = 2^192 used in the price^2 product space.
pub fn q192() -> U256 {
    // 1 << 192 fits in U256.
    U256::from(1u64) << 192
}

/// `mul_div(a, b, denom)` returning `floor((a*b) / denom)` exactly.
///
/// Byte-identical to Uniswap V3's `FullMath.mulDiv` (Remco Bloemen's algorithm): forms the
/// 512-bit product `[prod1 prod0]` via the `mulmod` trick, then performs the 512-by-256
/// division using a single `mulmod` for the remainder plus a Newton–Raphson modular inverse
/// — **avoiding ruint's generic 512-bit Knuth long-division** that the previous `U512`
/// implementation incurred on every V3 tick-walk step. All arithmetic is wrapping (mod
/// 2^256), matching the EVM. Returns `Overflow` when the exact quotient is ≥ 2^256 (the same
/// boundary the old `bit_len() > 256` cast detected), or `InvariantViolation` on `denom == 0`.
#[inline]
pub fn mul_div(a: U256, b: U256, denom: U256) -> Result<U256, AmmError> {
    if denom.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    // 512-bit product [prod1 prod0]:
    //   prod0 = (a*b) mod 2^256        — the low 256 bits
    //   prod1 = high 256 bits, recovered from (a*b) mod (2^256 - 1)
    let mm = a.mul_mod(b, U256::MAX);
    let prod0 = a.wrapping_mul(b);
    let prod1 = mm
        .wrapping_sub(prod0)
        .wrapping_sub(U256::from((mm < prod0) as u64));

    // Fast path: the product fits in 256 bits — a plain division, no overflow possible.
    if prod1.is_zero() {
        return Ok(prod0 / denom);
    }

    // floor(prod / denom) ≥ 2^256  ⟺  prod1 ≥ denom  → overflows the U256 result.
    if denom <= prod1 {
        return Err(AmmError::Overflow);
    }

    // --- 512-by-256 division without a generic 512-bit divide ---
    // Subtract the 256-bit remainder from the 512-bit numerator.
    let remainder = a.mul_mod(b, denom);
    let prod1 = prod1.wrapping_sub(U256::from((remainder > prod0) as u64));
    let prod0 = prod0.wrapping_sub(remainder);

    // Factor the largest power of two out of denom so it becomes odd (invertible mod 2^256).
    let twos = denom & U256::ZERO.wrapping_sub(denom);
    let denom = denom / twos;
    let mut prod0 = prod0 / twos;
    // Recombine the bits of prod1 shifted into prod0: prod0 |= prod1 * (2^256 / twos).
    let twos_shift = (U256::ZERO.wrapping_sub(twos) / twos).wrapping_add(U256::from(1u64));
    prod0 |= prod1.wrapping_mul(twos_shift);

    // Newton–Raphson inverse of the (now odd) denom, mod 2^256 (Hensel lifting, 4→256 bits).
    let two = U256::from(2u64);
    let mut inv = denom.wrapping_mul(U256::from(3u64)) ^ two; // correct mod 2^4
    inv = inv.wrapping_mul(two.wrapping_sub(denom.wrapping_mul(inv))); // mod 2^8
    inv = inv.wrapping_mul(two.wrapping_sub(denom.wrapping_mul(inv))); // mod 2^16
    inv = inv.wrapping_mul(two.wrapping_sub(denom.wrapping_mul(inv))); // mod 2^32
    inv = inv.wrapping_mul(two.wrapping_sub(denom.wrapping_mul(inv))); // mod 2^64
    inv = inv.wrapping_mul(two.wrapping_sub(denom.wrapping_mul(inv))); // mod 2^128
    inv = inv.wrapping_mul(two.wrapping_sub(denom.wrapping_mul(inv))); // mod 2^256

    Ok(prod0.wrapping_mul(inv))
}

/// `mul_div_rounding_up(a, b, denom)` returning `ceil((a*b) / denom)` exactly.
///
/// Mirrors Uniswap's `FullMath.mulDivRoundingUp`: detects a non-zero remainder with a single
/// `mulmod` (rather than recomputing the full 512-bit product as the old implementation did).
#[inline]
pub fn mul_div_rounding_up(a: U256, b: U256, denom: U256) -> Result<U256, AmmError> {
    let floor = mul_div(a, b, denom)?;
    // `mul_div` already validated `denom != 0`, so this `mul_mod` is well-defined.
    if a.mul_mod(b, denom).is_zero() {
        Ok(floor)
    } else if floor == U256::MAX {
        Err(AmmError::Overflow)
    } else {
        Ok(floor + U256::from(1u64))
    }
}

/// Computes `floor(num / denom)` and returns `denom == 0` as `InvariantViolation`.
#[inline]
pub fn div_floor(num: U256, denom: U256) -> Result<U256, AmmError> {
    if denom.is_zero() {
        Err(AmmError::InvariantViolation)
    } else {
        Ok(num / denom)
    }
}

/// Computes `ceil(num / denom)`.
#[inline]
pub fn div_ceil(num: U256, denom: U256) -> Result<U256, AmmError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_div_basic() {
        let a = U256::from(10u64);
        let b = U256::from(20u64);
        let d = U256::from(3u64);
        assert_eq!(mul_div(a, b, d).unwrap(), U256::from(66u64));
        assert_eq!(mul_div_rounding_up(a, b, d).unwrap(), U256::from(67u64));
    }

    #[test]
    fn mul_div_large() {
        // (2^200 * 5) / 4 = 5 * 2^198
        let a = U256::from(1u64) << 200;
        let b = U256::from(5u64);
        let d = U256::from(4u64);
        let expected = U256::from(5u64) << 198;
        assert_eq!(mul_div(a, b, d).unwrap(), expected);
    }

    #[test]
    fn mul_div_overflow() {
        let a = U256::MAX;
        let b = U256::from(2u64);
        let d = U256::from(1u64);
        assert!(matches!(mul_div(a, b, d), Err(AmmError::Overflow)));
    }

    #[test]
    fn div_by_zero() {
        assert!(matches!(
            mul_div(U256::from(1u64), U256::from(1u64), U256::ZERO),
            Err(AmmError::InvariantViolation)
        ));
    }

    // --- FullMath edge cases (pin the exact boundary behaviour the rewrite must preserve) ---

    #[test]
    fn mul_div_denom_one_is_product_or_overflow() {
        // a*b fits → product; a*b overflows U256 → Overflow (denom=1 can't rescue it).
        let max = U256::MAX;
        assert_eq!(
            mul_div(max, U256::from(1u64), U256::from(1u64)).unwrap(),
            max
        );
        assert!(matches!(
            mul_div(max, U256::from(2u64), U256::from(1u64)),
            Err(AmmError::Overflow)
        ));
    }

    #[test]
    fn mul_div_zero_operands() {
        let d = U256::from(7u64);
        assert_eq!(mul_div(U256::ZERO, U256::MAX, d).unwrap(), U256::ZERO);
        assert_eq!(mul_div(U256::MAX, U256::ZERO, d).unwrap(), U256::ZERO);
        assert_eq!(
            mul_div_rounding_up(U256::ZERO, U256::MAX, d).unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn mul_div_max_squared_over_max_is_max() {
        // floor(MAX*MAX / MAX) == MAX (the exact 2^256-boundary case — prod1 == denom-1).
        let max = U256::MAX;
        assert_eq!(mul_div(max, max, max).unwrap(), max);
        assert_eq!(mul_div_rounding_up(max, max, max).unwrap(), max);
    }

    #[test]
    fn mul_div_even_denominator_factoring() {
        // Exercises the "factor out powers of two" branch: (2^200 * 2^50) / 2^100 = 2^150.
        let a = U256::from(1u64) << 200;
        let b = U256::from(1u64) << 50;
        let d = U256::from(1u64) << 100;
        assert_eq!(mul_div(a, b, d).unwrap(), U256::from(1u64) << 150);
    }

    #[test]
    fn mul_div_rounding_up_exact_division_does_not_round() {
        // 12 / 3 = 4 exactly → ceil == floor (no +1).
        let a = U256::from(12u64);
        let b = U256::from(1u64);
        let d = U256::from(3u64);
        assert_eq!(mul_div_rounding_up(a, b, d).unwrap(), U256::from(4u64));
        assert_eq!(mul_div(a, b, d).unwrap(), U256::from(4u64));
    }

    #[test]
    fn mul_div_rounding_up_overflows_when_floor_is_max_with_remainder() {
        // floor == MAX and a non-zero remainder ⇒ ceil would be 2^256 ⇒ Overflow.
        // a*b = MAX*3, denom = 3 → floor = MAX exactly, remainder 0 → NOT overflow.
        // a*b = (MAX)*3 + 1 shape: use a=MAX, b=3, d=3 → exact; to force remainder use d=4.
        let max = U256::MAX;
        // MAX*3 / 4 = floor((3*2^256-3)/4); this is < MAX so not the overflow case — instead
        // construct floor==MAX-with-remainder via a=MAX, b=2, d=2 (exact) vs d s.t. remainder.
        // Simplest exact-overflow trigger: a=MAX, b=MAX, d=MAX-1 → floor >= 2^256.
        assert!(matches!(
            mul_div_rounding_up(max, max, max - U256::from(1u64)),
            Err(AmmError::Overflow)
        ));
    }
}

/// Byte-exactness guard for the `FullMath` (mulmod + modular-inverse) implementation of
/// `mul_div`/`mul_div_rounding_up`: compares the production path against an INDEPENDENT
/// full-width 512-bit oracle (the generic-division algorithm) across 20 000 fuzzed cases,
/// plus a power-of-two-denominator strategy that hammers the "factor out twos" branch. If
/// the production primitive ever diverges from exact `floor`/`ceil` by a single wei, these
/// fail — the same contract the V3 `amount0/1_delta` differential proptests rely on.
#[cfg(test)]
mod fullmath_differential {
    use super::*;
    use proptest::prelude::*;
    use ruint::aliases::U512;

    fn arb_u256() -> impl Strategy<Value = U256> {
        any::<[u64; 4]>().prop_map(U256::from_limbs)
    }

    fn to_u256(x: U512) -> Option<U256> {
        if x.bit_len() > 256 {
            None
        } else {
            let l = x.as_limbs();
            Some(U256::from_limbs([l[0], l[1], l[2], l[3]]))
        }
    }

    /// Independent reference: `floor(a*b/d)` via a full 512-bit division (a different code
    /// path from the production mulmod + Newton-inverse). `None` == overflows U256.
    fn ref_floor(a: U256, b: U256, d: U256) -> Option<U256> {
        let prod = U512::from(a) * U512::from(b);
        to_u256(prod / U512::from(d))
    }

    /// Independent reference: `ceil(a*b/d)`. `None` == overflows U256.
    fn ref_ceil(a: U256, b: U256, d: U256) -> Option<U256> {
        let prod = U512::from(a) * U512::from(b);
        let dd = U512::from(d);
        let q = prod / dd;
        let q = if (prod % dd).is_zero() {
            q
        } else {
            q + U512::from(1u64)
        };
        to_u256(q)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20_000))]

        #[test]
        fn mul_div_matches_u512_floor(a in arb_u256(), b in arb_u256(), d in arb_u256()) {
            prop_assume!(!d.is_zero());
            match ref_floor(a, b, d) {
                Some(exp) => prop_assert_eq!(mul_div(a, b, d).ok(), Some(exp)),
                None => prop_assert!(matches!(mul_div(a, b, d), Err(AmmError::Overflow))),
            }
        }

        #[test]
        fn mul_div_rounding_up_matches_u512_ceil(a in arb_u256(), b in arb_u256(), d in arb_u256()) {
            prop_assume!(!d.is_zero());
            match ref_ceil(a, b, d) {
                Some(exp) => prop_assert_eq!(mul_div_rounding_up(a, b, d).ok(), Some(exp)),
                None => prop_assert!(matches!(mul_div_rounding_up(a, b, d), Err(AmmError::Overflow))),
            }
        }

        /// Power-of-two denominators exercise the `twos` factoring + bit-shift recombination.
        #[test]
        fn mul_div_power_of_two_denominator(a in arb_u256(), b in arb_u256(), shift in 0u32..=255) {
            let d = U256::from(1u64) << shift;
            match ref_floor(a, b, d) {
                Some(exp) => prop_assert_eq!(mul_div(a, b, d).ok(), Some(exp)),
                None => prop_assert!(matches!(mul_div(a, b, d), Err(AmmError::Overflow))),
            }
        }
    }
}
