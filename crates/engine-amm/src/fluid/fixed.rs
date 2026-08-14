//! Shared integer arithmetic utilities.
//!
//! These helpers use U512 intermediates whenever a Fluid/Uniswap-style formula
//! can overflow 256-bit multiplication. They intentionally expose floor/ceil
//! semantics explicitly because AMM parity depends on rounding direction.

use alloy_primitives::{U256, U512};

use super::error::{AmmError, Result};
use super::types::{FEE_DENOMINATOR_1E6, Q96_U128};

#[inline(always)]
pub const fn q96() -> U256 {
    U256::from_limbs([0, 0x1_0000_0000, 0, 0])
}

#[inline(always)]
pub const fn one_e6() -> U256 {
    U256::from_limbs([FEE_DENOMINATOR_1E6, 0, 0, 0])
}

#[inline(always)]
pub const fn one_e12() -> U256 {
    U256::from_limbs([1_000_000_000_000, 0, 0, 0])
}

#[inline(always)]
pub const fn one_e18() -> U256 {
    U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0])
}

#[inline(always)]
pub const fn one_e27() -> U256 {
    U256::from_limbs([0x9fd0803ce8000000, 0x33b2e3c, 0, 0])
}

#[inline(always)]
pub fn u512_to_u256(x: U512) -> Result<U256> {
    let bytes = x.to_be_bytes::<64>();
    if bytes[..32].iter().any(|b| *b != 0) {
        return Err(AmmError::MathOverflow);
    }
    Ok(U256::from_be_slice(&bytes[32..]))
}

#[inline(always)]
pub fn mul_div_floor(a: U256, b: U256, denominator: U256) -> Result<U256> {
    if denominator.is_zero() {
        return Err(AmmError::DivisionByZero);
    }
    let q = (U512::from(a) * U512::from(b)) / U512::from(denominator);
    u512_to_u256(q)
}

#[inline(always)]
pub fn mul_div_ceil(a: U256, b: U256, denominator: U256) -> Result<U256> {
    if denominator.is_zero() {
        return Err(AmmError::DivisionByZero);
    }
    let prod = U512::from(a) * U512::from(b);
    if prod.is_zero() {
        return Ok(U256::ZERO);
    }
    let d = U512::from(denominator);
    let q = (prod - U512::from(1u8)) / d + U512::from(1u8);
    u512_to_u256(q)
}

#[inline(always)]
pub fn div_floor(a: U256, denominator: U256) -> Result<U256> {
    if denominator.is_zero() {
        return Err(AmmError::DivisionByZero);
    }
    Ok(a / denominator)
}

#[inline(always)]
pub fn div_ceil(a: U256, denominator: U256) -> Result<U256> {
    if denominator.is_zero() {
        return Err(AmmError::DivisionByZero);
    }
    if a.is_zero() {
        return Ok(U256::ZERO);
    }
    Ok((a - U256::from(1u8)) / denominator + U256::from(1u8))
}

#[inline(always)]
pub fn checked_add(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or(AmmError::MathOverflow)
}

#[inline(always)]
pub fn checked_sub(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b).ok_or(AmmError::InsufficientLiquidity)
}

#[inline(always)]
pub fn checked_mul(a: U256, b: U256) -> Result<U256> {
    a.checked_mul(b).ok_or(AmmError::MathOverflow)
}

#[inline(always)]
pub fn apply_fee_floor(amount: U256, fee_1e6: u32) -> Result<U256> {
    if fee_1e6 as u64 >= FEE_DENOMINATOR_1E6 {
        return Err(AmmError::InvalidFee);
    }
    mul_div_floor(
        amount,
        U256::from(FEE_DENOMINATOR_1E6 - fee_1e6 as u64),
        U256::from(FEE_DENOMINATOR_1E6),
    )
}

#[inline(always)]
pub fn fee_amount_for_exact_input_if_target_reached(
    amount_in_after_fee: U256,
    fee_1e6: u32,
) -> Result<U256> {
    if fee_1e6 as u64 >= FEE_DENOMINATOR_1E6 {
        return Err(AmmError::InvalidFee);
    }
    // ceil(amount_in_after_fee * fee / (1e6 - fee))
    mul_div_ceil(
        amount_in_after_fee,
        U256::from(fee_1e6 as u64),
        U256::from(FEE_DENOMINATOR_1E6 - fee_1e6 as u64),
    )
}

#[inline(always)]
pub fn gross_up_fee_floor(amount_after_fee: U256, fee_1e6: u32) -> Result<U256> {
    if fee_1e6 as u64 >= FEE_DENOMINATOR_1E6 {
        return Err(AmmError::InvalidFee);
    }
    mul_div_floor(
        amount_after_fee,
        U256::from(FEE_DENOMINATOR_1E6),
        U256::from(FEE_DENOMINATOR_1E6 - fee_1e6 as u64),
    )
}

#[inline(always)]
pub fn gross_up_fee_ceil(amount_after_fee: U256, fee_1e6: u32) -> Result<U256> {
    if fee_1e6 as u64 >= FEE_DENOMINATOR_1E6 {
        return Err(AmmError::InvalidFee);
    }
    mul_div_ceil(
        amount_after_fee,
        U256::from(FEE_DENOMINATOR_1E6),
        U256::from(FEE_DENOMINATOR_1E6 - fee_1e6 as u64),
    )
}

/// Babylonian square root on U512. Returns floor(sqrt(x)).
pub fn sqrt_u512(x: U512) -> U512 {
    if x <= U512::from(1u8) {
        return x;
    }

    // Start with x/2+1. It is not the fastest possible initial guess, but this
    // branch is used only by Fluid v1 route splitting, not the v2 hot path.
    let mut z = x;
    let mut y = (x >> 1) + U512::from(1u8);
    while y < z {
        z = y;
        y = ((x / y) + y) >> 1;
    }
    z
}

/// Absolute difference for U256 values.
#[inline(always)]
pub fn abs_diff(a: U256, b: U256) -> U256 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

/// Liquidity delta helper for v2 amount0 math:
/// ceil/floor(liquidity * (sqrt_b - sqrt_a) * Q96 / sqrt_b / sqrt_a).
pub fn amount0_delta(
    liquidity: U256,
    sqrt_a_x96: U256,
    sqrt_b_x96: U256,
    round_up: bool,
) -> Result<U256> {
    if liquidity.is_zero() {
        return Ok(U256::ZERO);
    }
    if sqrt_a_x96.is_zero() || sqrt_b_x96.is_zero() {
        return Err(AmmError::SqrtPriceOutOfRange);
    }

    let (lo, hi) = if sqrt_a_x96 < sqrt_b_x96 {
        (sqrt_a_x96, sqrt_b_x96)
    } else {
        (sqrt_b_x96, sqrt_a_x96)
    };
    let delta = hi - lo;
    let numerator1 = checked_mul(liquidity, U256::from(Q96_U128))?;

    if round_up {
        let tmp = mul_div_ceil(numerator1, delta, hi)?;
        div_ceil(tmp, lo)
    } else {
        let tmp = mul_div_floor(numerator1, delta, hi)?;
        div_floor(tmp, lo)
    }
}

/// Liquidity delta helper for v2 amount1 math:
/// ceil/floor(liquidity * (sqrt_b - sqrt_a) / Q96).
pub fn amount1_delta(
    liquidity: U256,
    sqrt_a_x96: U256,
    sqrt_b_x96: U256,
    round_up: bool,
) -> Result<U256> {
    if liquidity.is_zero() {
        return Ok(U256::ZERO);
    }
    let (lo, hi) = if sqrt_a_x96 < sqrt_b_x96 {
        (sqrt_a_x96, sqrt_b_x96)
    } else {
        (sqrt_b_x96, sqrt_a_x96)
    };
    let delta = hi - lo;
    if round_up {
        mul_div_ceil(liquidity, delta, U256::from(Q96_U128))
    } else {
        mul_div_floor(liquidity, delta, U256::from(Q96_U128))
    }
}

/// Next sqrt price when token0 is added to or removed from the pool. Rounds up.
pub fn next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: U256,
    liquidity: U256,
    amount: U256,
    add: bool,
) -> Result<U256> {
    if amount.is_zero() {
        return Ok(sqrt_price_x96);
    }
    if liquidity.is_zero() || sqrt_price_x96.is_zero() {
        return Err(AmmError::NoLiquidity);
    }

    let numerator1 = checked_mul(liquidity, U256::from(Q96_U128))?;
    let product = U512::from(amount) * U512::from(sqrt_price_x96);
    let numerator = U512::from(numerator1) * U512::from(sqrt_price_x96);

    let denominator = if add {
        U512::from(numerator1) + product
    } else {
        if product >= U512::from(numerator1) {
            return Err(AmmError::OutputWouldExhaustPriceRange);
        }
        U512::from(numerator1) - product
    };

    if denominator.is_zero() {
        return Err(AmmError::DivisionByZero);
    }
    let q = if numerator.is_zero() {
        U512::ZERO
    } else {
        (numerator - U512::from(1u8)) / denominator + U512::from(1u8)
    };
    u512_to_u256(q)
}

/// Next sqrt price when token1 is added to or removed from the pool. Rounds down.
pub fn next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: U256,
    liquidity: U256,
    amount: U256,
    add: bool,
) -> Result<U256> {
    if amount.is_zero() {
        return Ok(sqrt_price_x96);
    }
    if liquidity.is_zero() {
        return Err(AmmError::NoLiquidity);
    }

    if add {
        let quotient = mul_div_floor(amount, U256::from(Q96_U128), liquidity)?;
        checked_add(sqrt_price_x96, quotient)
    } else {
        let quotient = mul_div_ceil(amount, U256::from(Q96_U128), liquidity)?;
        if sqrt_price_x96 <= quotient {
            return Err(AmmError::OutputWouldExhaustPriceRange);
        }
        Ok(sqrt_price_x96 - quotient)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_div_ceil_rounds() {
        assert_eq!(
            mul_div_floor(U256::from(10u8), U256::from(10u8), U256::from(6u8)).unwrap(),
            U256::from(16u8)
        );
        assert_eq!(
            mul_div_ceil(U256::from(10u8), U256::from(10u8), U256::from(6u8)).unwrap(),
            U256::from(17u8)
        );
    }

    #[test]
    fn q96_constant_matches() {
        assert_eq!(q96(), U256::from(Q96_U128));
    }
}
