//! Fluid DEX v2 math.
//!
//! This module intentionally models only the math state needed for quoting.
//! Fluid v2 pool discovery, resolver calls, callback execution, and settlement
//! belong to the host system. Feed this module:
//!
//! - `currentSqrtPriceX96`;
//! - `activeLiquidity`;
//! - the effective `lpFee` for this quote;
//! - pre-sorted tick/segment boundaries with liquidity after crossing.
//!
//! D3 and D4 share the same range-order swap kernel. Their difference is how
//! liquidity is sourced and settled at the Fluid Liquidity Layer, not how a
//! local quote traverses sqrt-price segments.

use alloy_primitives::U256;

use super::error::{AmmError, Result};
use super::fixed::{
    amount0_delta, amount1_delta, apply_fee_floor, checked_add, checked_sub,
    fee_amount_for_exact_input_if_target_reached, gross_up_fee_ceil, mul_div_floor,
    next_sqrt_price_from_amount0_rounding_up, next_sqrt_price_from_amount1_rounding_down,
};
use super::types::{SwapDirection, FEE_DENOMINATOR_1E6, Q96_U128};

/// Solidity-compatible Fluid v2 dynamic-fee sentinel in `DexKey.fee`.
pub const V2_DYNAMIC_FEE_SENTINEL: u32 = 0x00FF_FFFF;

/// Minimum and maximum ticks for sqrt-ratio helpers. These are the standard
/// 1.0001^tick bounds used by Q64.96 concentrated-liquidity math.
pub const V2_MIN_TICK: i32 = -887_272;
pub const V2_MAX_TICK: i32 = 887_272;

#[inline(always)]
pub const fn v2_min_sqrt_ratio_x96() -> U256 {
    U256::from_limbs([4_295_128_739, 0, 0, 0])
}

#[inline(always)]
pub const fn v2_max_sqrt_ratio_x96() -> U256 {
    U256::from_limbs([
        6_743_328_256_752_651_558,
        17_280_870_778_742_802_505,
        4_294_805_859,
        0,
    ])
}

#[inline(always)]
const fn max_u256() -> U256 {
    U256::from_limbs([u64::MAX, u64::MAX, u64::MAX, u64::MAX])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2DexType {
    /// DEX Type 3: Smart Collateral Range Orders.
    D3SmartCollateralRange,
    /// DEX Type 4: Smart Debt Range Orders.
    D4SmartDebtRange,
}

impl V2DexType {
    #[inline(always)]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::D3SmartCollateralRange => 3,
            Self::D4SmartDebtRange => 4,
        }
    }
}

/// Minimal math state for one Fluid v2 pool snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2PoolState {
    pub dex_type: V2DexType,
    pub current_tick: i32,
    pub current_sqrt_price_x96: U256,
    pub active_liquidity: U256,
    /// Effective LP fee on 1e6 precision. For dynamic-fee pools, the host must
    /// fetch/compute the current effective fee and pass it here.
    pub lp_fee_1e6: u32,
    pub dynamic_fee: bool,
    pub swaps_paused: bool,
}

/// Boundary reached while moving price across initialized liquidity.
///
/// For `ZeroToOne`, provide boundaries strictly below the current price in
/// descending order. For `OneToZero`, provide boundaries strictly above the
/// current price in ascending order. `liquidity_after_cross` is the active
/// liquidity immediately after crossing this boundary in the quote direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Boundary {
    pub tick: i32,
    pub sqrt_price_x96: U256,
    pub liquidity_after_cross: U256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2QuoteOptions {
    /// Optional hard price limit. If absent, the kernel uses the global min/max
    /// sqrt-ratio bound for the direction.
    pub sqrt_price_limit_x96: Option<U256>,
    /// If true, return `InsufficientLiquidity` when the requested amount cannot
    /// be fully consumed/satisfied before the price limit.
    pub fail_if_partial: bool,
}

impl Default for V2QuoteOptions {
    fn default() -> Self {
        Self {
            sqrt_price_limit_x96: None,
            fail_if_partial: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2StepQuote {
    pub sqrt_price_start_x96: U256,
    pub sqrt_price_next_x96: U256,
    /// Input amount that actually moves price, after fee.
    pub amount_in_after_fee: U256,
    pub amount_out: U256,
    pub fee_amount: U256,
    pub reached_target: bool,
}

impl V2StepQuote {
    #[inline(always)]
    pub fn amount_in_gross(self) -> Result<U256> {
        checked_add(self.amount_in_after_fee, self.fee_amount)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2ExactInQuote {
    pub amount_in: U256,
    pub amount_in_consumed: U256,
    pub amount_out: U256,
    pub fee_paid: U256,
    pub sqrt_price_start_x96: U256,
    pub sqrt_price_next_x96: U256,
    pub active_liquidity_next: U256,
    pub consumed_all_input: bool,
    pub boundaries_crossed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2ExactOutQuote {
    pub amount_in: U256,
    pub amount_out: U256,
    pub amount_out_satisfied: U256,
    pub fee_paid: U256,
    pub sqrt_price_start_x96: U256,
    pub sqrt_price_next_x96: U256,
    pub active_liquidity_next: U256,
    pub satisfied_full_output: bool,
    pub boundaries_crossed: usize,
}

impl V2PoolState {
    #[inline(always)]
    fn pre_quote_checks(&self) -> Result<()> {
        if self.swaps_paused {
            return Err(AmmError::PoolPaused);
        }
        if self.active_liquidity.is_zero() {
            return Err(AmmError::NoLiquidity);
        }
        if self.lp_fee_1e6 as u64 >= FEE_DENOMINATOR_1E6 {
            return Err(AmmError::InvalidFee);
        }
        validate_v2_sqrt_price(self.current_sqrt_price_x96)?;
        Ok(())
    }

    pub fn quote_exact_in(
        &self,
        direction: SwapDirection,
        amount_in: U256,
        boundaries: &[V2Boundary],
        options: V2QuoteOptions,
    ) -> Result<V2ExactInQuote> {
        self.pre_quote_checks()?;
        if amount_in.is_zero() {
            return Err(AmmError::InsufficientLiquidity);
        }

        let start_sqrt = self.current_sqrt_price_x96;
        let limit = normalize_v2_price_limit(direction, start_sqrt, options.sqrt_price_limit_x96)?;
        let mut sqrt_price = start_sqrt;
        let mut liquidity = self.active_liquidity;
        let mut amount_remaining = amount_in;
        let mut amount_in_consumed = U256::ZERO;
        let mut amount_out_total = U256::ZERO;
        let mut fee_total = U256::ZERO;
        let mut boundaries_crossed = 0usize;
        let mut boundary_idx = 0usize;

        while amount_remaining > U256::ZERO {
            if liquidity.is_zero() {
                if options.fail_if_partial {
                    return Err(AmmError::InsufficientLiquidity);
                }
                break;
            }

            let (target, crosses_boundary) =
                next_v2_target(direction, sqrt_price, limit, boundaries, boundary_idx)?;
            if target == sqrt_price {
                if crosses_boundary {
                    liquidity = boundaries[boundary_idx].liquidity_after_cross;
                    boundary_idx += 1;
                    boundaries_crossed += 1;
                    continue;
                }
                break;
            }

            let step = compute_v2_swap_step_exact_in(
                direction,
                sqrt_price,
                target,
                liquidity,
                amount_remaining,
                self.lp_fee_1e6,
            )?;
            let gross = step.amount_in_gross()?;
            amount_remaining = checked_sub(amount_remaining, gross)?;
            amount_in_consumed = checked_add(amount_in_consumed, gross)?;
            amount_out_total = checked_add(amount_out_total, step.amount_out)?;
            fee_total = checked_add(fee_total, step.fee_amount)?;
            sqrt_price = step.sqrt_price_next_x96;

            if step.reached_target && crosses_boundary {
                liquidity = boundaries[boundary_idx].liquidity_after_cross;
                boundary_idx += 1;
                boundaries_crossed += 1;
                continue;
            }

            if !step.reached_target || sqrt_price == limit {
                break;
            }
        }

        let consumed_all_input = amount_remaining.is_zero();
        if options.fail_if_partial && !consumed_all_input {
            return Err(AmmError::InsufficientLiquidity);
        }

        Ok(V2ExactInQuote {
            amount_in,
            amount_in_consumed,
            amount_out: amount_out_total,
            fee_paid: fee_total,
            sqrt_price_start_x96: start_sqrt,
            sqrt_price_next_x96: sqrt_price,
            active_liquidity_next: liquidity,
            consumed_all_input,
            boundaries_crossed,
        })
    }

    pub fn quote_exact_out(
        &self,
        direction: SwapDirection,
        amount_out: U256,
        boundaries: &[V2Boundary],
        options: V2QuoteOptions,
    ) -> Result<V2ExactOutQuote> {
        self.pre_quote_checks()?;
        if amount_out.is_zero() {
            return Err(AmmError::InsufficientLiquidity);
        }

        let start_sqrt = self.current_sqrt_price_x96;
        let limit = normalize_v2_price_limit(direction, start_sqrt, options.sqrt_price_limit_x96)?;
        let mut sqrt_price = start_sqrt;
        let mut liquidity = self.active_liquidity;
        let mut amount_remaining_out = amount_out;
        let mut amount_in_total = U256::ZERO;
        let mut amount_out_satisfied = U256::ZERO;
        let mut fee_total = U256::ZERO;
        let mut boundaries_crossed = 0usize;
        let mut boundary_idx = 0usize;

        while amount_remaining_out > U256::ZERO {
            if liquidity.is_zero() {
                if options.fail_if_partial {
                    return Err(AmmError::InsufficientLiquidity);
                }
                break;
            }

            let (target, crosses_boundary) =
                next_v2_target(direction, sqrt_price, limit, boundaries, boundary_idx)?;
            if target == sqrt_price {
                if crosses_boundary {
                    liquidity = boundaries[boundary_idx].liquidity_after_cross;
                    boundary_idx += 1;
                    boundaries_crossed += 1;
                    continue;
                }
                break;
            }

            let step = compute_v2_swap_step_exact_out(
                direction,
                sqrt_price,
                target,
                liquidity,
                amount_remaining_out,
                self.lp_fee_1e6,
            )?;
            let gross = step.amount_in_gross()?;
            amount_remaining_out = checked_sub(amount_remaining_out, step.amount_out)?;
            amount_out_satisfied = checked_add(amount_out_satisfied, step.amount_out)?;
            amount_in_total = checked_add(amount_in_total, gross)?;
            fee_total = checked_add(fee_total, step.fee_amount)?;
            sqrt_price = step.sqrt_price_next_x96;

            if step.reached_target && crosses_boundary {
                liquidity = boundaries[boundary_idx].liquidity_after_cross;
                boundary_idx += 1;
                boundaries_crossed += 1;
                continue;
            }

            if !step.reached_target || sqrt_price == limit {
                break;
            }
        }

        let satisfied_full_output = amount_remaining_out.is_zero();
        if options.fail_if_partial && !satisfied_full_output {
            return Err(AmmError::InsufficientLiquidity);
        }

        Ok(V2ExactOutQuote {
            amount_in: amount_in_total,
            amount_out,
            amount_out_satisfied,
            fee_paid: fee_total,
            sqrt_price_start_x96: start_sqrt,
            sqrt_price_next_x96: sqrt_price,
            active_liquidity_next: liquidity,
            satisfied_full_output,
            boundaries_crossed,
        })
    }
}

#[inline(always)]
pub fn validate_v2_sqrt_price(sqrt_price_x96: U256) -> Result<()> {
    if sqrt_price_x96 < v2_min_sqrt_ratio_x96() || sqrt_price_x96 > v2_max_sqrt_ratio_x96() {
        return Err(AmmError::SqrtPriceOutOfRange);
    }
    Ok(())
}

#[inline(always)]
pub fn normalize_v2_price_limit(
    direction: SwapDirection,
    current_sqrt_price_x96: U256,
    price_limit_x96: Option<U256>,
) -> Result<U256> {
    let limit = match (direction, price_limit_x96) {
        (SwapDirection::ZeroToOne, None) => v2_min_sqrt_ratio_x96(),
        (SwapDirection::OneToZero, None) => v2_max_sqrt_ratio_x96(),
        (_, Some(v)) => v,
    };
    validate_v2_sqrt_price(limit)?;
    match direction {
        SwapDirection::ZeroToOne if limit >= current_sqrt_price_x96 => {
            Err(AmmError::SqrtPriceOutOfRange)
        }
        SwapDirection::OneToZero if limit <= current_sqrt_price_x96 => {
            Err(AmmError::SqrtPriceOutOfRange)
        }
        _ => Ok(limit),
    }
}

#[inline(always)]
fn next_v2_target(
    direction: SwapDirection,
    current: U256,
    limit: U256,
    boundaries: &[V2Boundary],
    idx: usize,
) -> Result<(U256, bool)> {
    if idx >= boundaries.len() {
        return Ok((limit, false));
    }

    let b = boundaries[idx].sqrt_price_x96;
    validate_v2_sqrt_price(b)?;

    match direction {
        SwapDirection::ZeroToOne => {
            if b >= current {
                return Err(AmmError::SegmentOrder);
            }
            if b <= limit {
                Ok((limit, false))
            } else {
                Ok((b, true))
            }
        }
        SwapDirection::OneToZero => {
            if b <= current {
                return Err(AmmError::SegmentOrder);
            }
            if b >= limit {
                Ok((limit, false))
            } else {
                Ok((b, true))
            }
        }
    }
}

/// Compute one v2 concentrated-liquidity exact-input step.
///
/// `amount_remaining_gross` includes fee. The returned `amount_in_after_fee`
/// is the token amount that actually moves price.
pub fn compute_v2_swap_step_exact_in(
    direction: SwapDirection,
    sqrt_current_x96: U256,
    sqrt_target_x96: U256,
    liquidity: U256,
    amount_remaining_gross: U256,
    fee_1e6: u32,
) -> Result<V2StepQuote> {
    if liquidity.is_zero() {
        return Err(AmmError::NoLiquidity);
    }
    if fee_1e6 as u64 >= FEE_DENOMINATOR_1E6 {
        return Err(AmmError::InvalidFee);
    }

    let amount_remaining_less_fee = apply_fee_floor(amount_remaining_gross, fee_1e6)?;

    let (amount_in_max, sqrt_next_x96) = match direction {
        SwapDirection::ZeroToOne => {
            if sqrt_target_x96 >= sqrt_current_x96 {
                return Err(AmmError::SegmentOrder);
            }
            let amount0_max = amount0_delta(liquidity, sqrt_target_x96, sqrt_current_x96, true)?;
            let next = if amount_remaining_less_fee >= amount0_max {
                sqrt_target_x96
            } else {
                next_sqrt_price_from_amount0_rounding_up(
                    sqrt_current_x96,
                    liquidity,
                    amount_remaining_less_fee,
                    true,
                )?
            };
            (amount0_max, next)
        }
        SwapDirection::OneToZero => {
            if sqrt_target_x96 <= sqrt_current_x96 {
                return Err(AmmError::SegmentOrder);
            }
            let amount1_max = amount1_delta(liquidity, sqrt_current_x96, sqrt_target_x96, true)?;
            let next = if amount_remaining_less_fee >= amount1_max {
                sqrt_target_x96
            } else {
                next_sqrt_price_from_amount1_rounding_down(
                    sqrt_current_x96,
                    liquidity,
                    amount_remaining_less_fee,
                    true,
                )?
            };
            (amount1_max, next)
        }
    };

    let reached_target = sqrt_next_x96 == sqrt_target_x96;
    let amount_in_after_fee = match direction {
        SwapDirection::ZeroToOne => {
            amount0_delta(liquidity, sqrt_next_x96, sqrt_current_x96, true)?
        }
        SwapDirection::OneToZero => {
            amount1_delta(liquidity, sqrt_current_x96, sqrt_next_x96, true)?
        }
    };

    let amount_out = match direction {
        SwapDirection::ZeroToOne => {
            amount1_delta(liquidity, sqrt_next_x96, sqrt_current_x96, false)?
        }
        SwapDirection::OneToZero => {
            amount0_delta(liquidity, sqrt_current_x96, sqrt_next_x96, false)?
        }
    };

    let fee_amount = if reached_target {
        fee_amount_for_exact_input_if_target_reached(amount_in_after_fee, fee_1e6)?
    } else {
        checked_sub(amount_remaining_gross, amount_in_after_fee)?
    };

    // Defensive guard for rare rounding cases where a helper returns more than
    // the precomputed amount needed to hit the target.
    if reached_target && amount_in_after_fee > amount_in_max {
        return Err(AmmError::MathOverflow);
    }

    Ok(V2StepQuote {
        sqrt_price_start_x96: sqrt_current_x96,
        sqrt_price_next_x96: sqrt_next_x96,
        amount_in_after_fee,
        amount_out,
        fee_amount,
        reached_target,
    })
}

/// Compute one v2 concentrated-liquidity exact-output step.
pub fn compute_v2_swap_step_exact_out(
    direction: SwapDirection,
    sqrt_current_x96: U256,
    sqrt_target_x96: U256,
    liquidity: U256,
    amount_remaining_out: U256,
    fee_1e6: u32,
) -> Result<V2StepQuote> {
    if liquidity.is_zero() {
        return Err(AmmError::NoLiquidity);
    }
    if fee_1e6 as u64 >= FEE_DENOMINATOR_1E6 {
        return Err(AmmError::InvalidFee);
    }

    let (amount_out_max, sqrt_next_x96) = match direction {
        SwapDirection::ZeroToOne => {
            if sqrt_target_x96 >= sqrt_current_x96 {
                return Err(AmmError::SegmentOrder);
            }
            let max = amount1_delta(liquidity, sqrt_target_x96, sqrt_current_x96, false)?;
            let next = if amount_remaining_out >= max {
                sqrt_target_x96
            } else {
                // Receiving token1 moves sqrt price down. This is the inverse of
                // token1 exact-in and rounds down to avoid over-output.
                next_sqrt_price_from_amount1_rounding_down(
                    sqrt_current_x96,
                    liquidity,
                    amount_remaining_out,
                    false,
                )?
            };
            (max, next)
        }
        SwapDirection::OneToZero => {
            if sqrt_target_x96 <= sqrt_current_x96 {
                return Err(AmmError::SegmentOrder);
            }
            let max = amount0_delta(liquidity, sqrt_current_x96, sqrt_target_x96, false)?;
            let next = if amount_remaining_out >= max {
                sqrt_target_x96
            } else {
                // Receiving token0 moves sqrt price up. This is the inverse of
                // token0 exact-in and rounds up to avoid over-output.
                next_sqrt_price_from_amount0_rounding_up(
                    sqrt_current_x96,
                    liquidity,
                    amount_remaining_out,
                    false,
                )?
            };
            (max, next)
        }
    };

    let reached_target = sqrt_next_x96 == sqrt_target_x96;
    let amount_out = if reached_target {
        amount_out_max
    } else {
        amount_remaining_out
    };

    let amount_in_after_fee = match direction {
        SwapDirection::ZeroToOne => {
            amount0_delta(liquidity, sqrt_next_x96, sqrt_current_x96, true)?
        }
        SwapDirection::OneToZero => {
            amount1_delta(liquidity, sqrt_current_x96, sqrt_next_x96, true)?
        }
    };
    let fee_amount = gross_up_fee_ceil(amount_in_after_fee, fee_1e6)? - amount_in_after_fee;

    Ok(V2StepQuote {
        sqrt_price_start_x96: sqrt_current_x96,
        sqrt_price_next_x96: sqrt_next_x96,
        amount_in_after_fee,
        amount_out,
        fee_amount,
        reached_target,
    })
}

/// Convert a tick to sqrt(price) Q64.96. This helper is included so an indexer
/// can build boundary arrays without pulling in a separate CL math crate.
///
/// It follows the canonical 1.0001^tick fixed-point exponentiation constants.
pub fn v2_sqrt_ratio_at_tick(tick: i32) -> Result<U256> {
    if !(V2_MIN_TICK..=V2_MAX_TICK).contains(&tick) {
        return Err(AmmError::TickOutOfRange);
    }

    let abs_tick = if tick < 0 {
        (-tick) as u32
    } else {
        tick as u32
    };
    let mut ratio = if (abs_tick & 0x1) != 0 {
        U256::from_limbs([12262481743371124737, 18445821805675392311, 0, 0])
    } else {
        U256::from_limbs([0, 0, 1, 0])
    };

    macro_rules! mul_shift {
        ($mask:expr, $lo:expr, $hi:expr) => {
            if (abs_tick & $mask) != 0 {
                ratio = mul_div_floor(
                    ratio,
                    U256::from_limbs([$lo, $hi, 0, 0]),
                    U256::from_limbs([0, 0, 1, 0]),
                )?;
            }
        };
    }

    mul_shift!(0x2, 6459403834229662010u64, 18444899583751176498u64);
    mul_shift!(0x4, 17226890335427755468u64, 18443055278223354162u64);
    mul_shift!(0x8, 2032852871939366096u64, 18439367220385604838u64);
    mul_shift!(0x10, 14545316742740207172u64, 18431993317065449817u64);
    mul_shift!(0x20, 5129152022828963008u64, 18417254355718160513u64);
    mul_shift!(0x40, 4894419605888772193u64, 18387811781193591352u64);
    mul_shift!(0x80, 1280255884321894483u64, 18329067761203520168u64);
    mul_shift!(0x100, 15924666964335305636u64, 18212142134806087854u64);
    mul_shift!(0x200, 8010504389359918676u64, 17980523815641551639u64);
    mul_shift!(0x400, 10668036004952895731u64, 17526086738831147013u64);
    mul_shift!(0x800, 4878133418470705625u64, 16651378430235024244u64);
    mul_shift!(0x1000, 9537173718739605541u64, 15030750278693429944u64);
    mul_shift!(0x2000, 9972618978014552549u64, 12247334978882834399u64);
    mul_shift!(0x4000, 10428997489610666743u64, 8131365268884726200u64);
    mul_shift!(0x8000, 9305304367709015974u64, 3584323654723342297u64);
    mul_shift!(0x10000, 14301143598189091785u64, 696457651847595233u64);
    mul_shift!(0x20000, 7393154844743099908u64, 26294789957452057u64);
    mul_shift!(0x40000, 2209338891292245656u64, 37481735321082u64);
    mul_shift!(0x80000, 10518117631919034274u64, 76158723u64);

    if tick > 0 {
        ratio = max_u256() / ratio;
    }

    let low_32_mask = U256::from(0xffff_ffffu64);
    let rounded = if (ratio & low_32_mask) > U256::ZERO {
        (ratio >> 32) + U256::from(1u8)
    } else {
        ratio >> 32
    };
    validate_v2_sqrt_price(rounded)?;
    Ok(rounded)
}

#[inline(always)]
pub fn v2_price_x96_from_sqrt_x96(sqrt_price_x96: U256) -> Result<U256> {
    // price(token1/token0) on Q96 scale: sqrt^2 / Q96. Returning the raw
    // Q192 numerator would overflow U256 near the upper tick range, so this
    // helper keeps the value inside 256 bits while preserving ordering.
    mul_div_floor(sqrt_price_x96, sqrt_price_x96, U256::from(Q96_U128))
}

#[inline(always)]
pub fn v2_one_to_one_sqrt_price_x96() -> U256 {
    U256::from(Q96_U128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_to_one_tick_zero() {
        assert_eq!(v2_sqrt_ratio_at_tick(0).unwrap(), U256::from(Q96_U128));
    }

    #[test]
    fn exact_in_zero_to_one_moves_price_down() {
        let pool = V2PoolState {
            dex_type: V2DexType::D3SmartCollateralRange,
            current_tick: 0,
            current_sqrt_price_x96: U256::from(Q96_U128),
            active_liquidity: U256::from(1_000_000_000_000u64),
            lp_fee_1e6: 3000,
            dynamic_fee: false,
            swaps_paused: false,
        };
        let q = pool
            .quote_exact_in(
                SwapDirection::ZeroToOne,
                U256::from(1_000_000u64),
                &[],
                V2QuoteOptions::default(),
            )
            .unwrap();
        assert!(q.sqrt_price_next_x96 < U256::from(Q96_U128));
        assert!(q.amount_out > U256::ZERO);
    }
}
