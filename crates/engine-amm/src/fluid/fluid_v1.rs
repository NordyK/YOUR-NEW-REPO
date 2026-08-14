//! Fluid DEX v1 / T1 math.
//!
//! Fluid v1/T1 exposes two internal AMM sides for a pair:
//! - smart collateral side;
//! - smart debt side.
//!
//! Both sides use constant-product math over *imaginary reserves* returned by
//! the DexReservesResolver adjusted methods. When both sides are enabled, Fluid
//! splits the trade so the final prices of both sides are equal. The formulas
//! below mirror the public resolver's `_swapRoutingIn` and `_swapRoutingOut`
//! structure, then apply the same v1 fee precision and price-move guard.

use alloy_primitives::{U256, U512};

use super::error::{AmmError, Result};
use super::fixed::{
    abs_diff, apply_fee_floor, checked_add, checked_sub, gross_up_fee_ceil, gross_up_fee_floor,
    mul_div_ceil, mul_div_floor, one_e12, one_e18, one_e27, sqrt_u512, u512_to_u256,
};
use super::types::{
    SwapDirection, TokenScale, FLUID_V1_MIN_RESERVES_RATIO_1E12, FLUID_V1_PRICE_MOVE_LIMIT_1E18,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct V1SideReservesAdjusted {
    pub token0_real: U256,
    pub token1_real: U256,
    pub token0_imaginary: U256,
    pub token1_imaginary: U256,
}

impl V1SideReservesAdjusted {
    #[inline(always)]
    pub const fn imaginary_pair(self) -> V1ImaginaryPair {
        V1ImaginaryPair {
            token0: self.token0_imaginary,
            token1: self.token1_imaginary,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct V1ImaginaryPair {
    pub token0: U256,
    pub token1: U256,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct V1ExpandableLimit {
    pub available_native: U256,
    pub expands_to_native: U256,
    /// Remaining seconds from the snapshot timestamp until `expands_to_native`.
    pub expand_duration_left_secs: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct V1LimitsNative {
    pub withdrawable_token0: V1ExpandableLimit,
    pub withdrawable_token1: V1ExpandableLimit,
    pub borrowable_token0: V1ExpandableLimit,
    pub borrowable_token1: V1ExpandableLimit,
    pub snapshot_timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1PoolState {
    pub fee_1e6: u32,
    pub token0_scale: TokenScale,
    pub token1_scale: TokenScale,
    pub collateral: V1SideReservesAdjusted,
    pub debt: V1SideReservesAdjusted,
    pub limits: V1LimitsNative,
    pub smart_collateral_enabled: bool,
    pub smart_debt_enabled: bool,
    pub swaps_paused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum V1ExactOutRounding {
    /// Matches the public resolver estimator path, which uses floor division for
    /// `_getAmountIn()` and floor gross-up by fee.
    ResolverParity,
    /// Conservative quote for transaction building. Uses ceil amount-in and
    /// ceil fee gross-up so `amountInMax` is not under-estimated by local math.
    #[default]
    ExecutionSafe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1SplitQuote {
    pub collateral_in_adjusted_gross: U256,
    pub debt_in_adjusted_gross: U256,
    pub collateral_out_adjusted: U256,
    pub debt_out_adjusted: U256,
    pub collateral_fee_adjusted: U256,
    pub debt_fee_adjusted: U256,
}

impl V1SplitQuote {
    #[inline(always)]
    pub fn total_in_adjusted(self) -> Result<U256> {
        checked_add(
            self.collateral_in_adjusted_gross,
            self.debt_in_adjusted_gross,
        )
    }

    #[inline(always)]
    pub fn total_out_adjusted(self) -> Result<U256> {
        checked_add(self.collateral_out_adjusted, self.debt_out_adjusted)
    }

    #[inline(always)]
    pub fn total_fee_adjusted(self) -> Result<U256> {
        checked_add(self.collateral_fee_adjusted, self.debt_fee_adjusted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1ExactInQuote {
    pub amount_in_native: U256,
    pub amount_out_native: U256,
    pub amount_in_adjusted: U256,
    pub amount_out_adjusted: U256,
    pub split: V1SplitQuote,
    pub dominant_old_price_1e27: U256,
    pub dominant_new_price_1e27: U256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1ExactOutQuote {
    pub amount_in_native: U256,
    pub amount_out_native: U256,
    pub amount_in_adjusted: U256,
    pub amount_out_adjusted: U256,
    pub split: V1SplitQuote,
    pub dominant_old_price_1e27: U256,
    pub dominant_new_price_1e27: U256,
}

impl V1PoolState {
    #[inline(always)]
    pub fn quote_exact_in(
        &self,
        direction: SwapDirection,
        amount_in_native: U256,
        now: u64,
    ) -> Result<V1ExactInQuote> {
        self.pre_quote_checks()?;
        let input_scale = self.input_scale(direction);
        let output_scale = self.output_scale(direction);
        let amount_in_adjusted = input_scale.native_to_adjusted_floor(amount_in_native)?;
        if amount_in_adjusted.is_zero() {
            return Err(AmmError::InsufficientLiquidity);
        }

        let split = route_v1_exact_in(
            amount_in_adjusted,
            self.collateral.imaginary_pair(),
            self.debt.imaginary_pair(),
            direction,
            self.fee_1e6,
            self.smart_collateral_enabled,
            self.smart_debt_enabled,
        )?;

        self.check_output_limits(
            direction,
            now,
            split.collateral_out_adjusted,
            split.debt_out_adjusted,
        )?;
        self.check_reserve_ratios()?;
        let (old_price, new_price) = dominant_v1_price_exact_in(
            self.collateral.imaginary_pair(),
            self.debt.imaginary_pair(),
            direction,
            split,
        )?;
        enforce_v1_price_move(old_price, new_price)?;

        let amount_out_adjusted = split.total_out_adjusted()?;
        let amount_out_native = output_scale.adjusted_to_native_floor(amount_out_adjusted)?;

        Ok(V1ExactInQuote {
            amount_in_native,
            amount_out_native,
            amount_in_adjusted,
            amount_out_adjusted,
            split,
            dominant_old_price_1e27: old_price,
            dominant_new_price_1e27: new_price,
        })
    }

    #[inline(always)]
    pub fn quote_exact_out(
        &self,
        direction: SwapDirection,
        amount_out_native: U256,
        now: u64,
        rounding: V1ExactOutRounding,
    ) -> Result<V1ExactOutQuote> {
        self.pre_quote_checks()?;
        let input_scale = self.input_scale(direction);
        let output_scale = self.output_scale(direction);
        let amount_out_adjusted = output_scale.native_to_adjusted_floor(amount_out_native)?;
        if amount_out_adjusted.is_zero() {
            return Err(AmmError::InsufficientLiquidity);
        }

        let split = route_v1_exact_out(
            amount_out_adjusted,
            self.collateral.imaginary_pair(),
            self.debt.imaginary_pair(),
            direction,
            self.fee_1e6,
            self.smart_collateral_enabled,
            self.smart_debt_enabled,
            rounding,
        )?;

        self.check_output_limits(
            direction,
            now,
            split.collateral_out_adjusted,
            split.debt_out_adjusted,
        )?;
        self.check_reserve_ratios()?;
        let (old_price, new_price) = dominant_v1_price_exact_in(
            self.collateral.imaginary_pair(),
            self.debt.imaginary_pair(),
            direction,
            split,
        )?;
        enforce_v1_price_move(old_price, new_price)?;

        let amount_in_adjusted = split.total_in_adjusted()?;
        let amount_in_native = input_scale.adjusted_to_native_ceil(amount_in_adjusted)?;

        Ok(V1ExactOutQuote {
            amount_in_native,
            amount_out_native,
            amount_in_adjusted,
            amount_out_adjusted,
            split,
            dominant_old_price_1e27: old_price,
            dominant_new_price_1e27: new_price,
        })
    }

    /// Forward-apply an exact-in swap: returns `(amount_out_native, new_state)`. Each routed side's
    /// PRICE-DETERMINING imaginary reserves move by the constant-product transition the quote
    /// implies — input-token reserve `+= amount_in_after_fee`, output-token reserve `-= amount_out`,
    /// per the routed `split`. The quote reads ONLY `imaginary_pair()`, so this keeps the predicted
    /// state self-consistent (a re-quote off it prices correctly). `limits` are time/config-driven
    /// (a swap does not mutate them) and the `*_real` reserves don't affect quoting (resync refreshes
    /// them), so both are carried as-is. Callers pass `now = u64::MAX` (limits fully expanded)
    /// because the triggering swap was already sequencer-accepted, so its limit gate must not reject.
    pub fn apply_exact_in(
        &self,
        direction: SwapDirection,
        amount_in_native: U256,
        now: u64,
    ) -> Result<(U256, V1PoolState)> {
        let q = self.quote_exact_in(direction, amount_in_native, now)?;
        let mut new = *self;
        let col_net_in = checked_sub(
            q.split.collateral_in_adjusted_gross,
            q.split.collateral_fee_adjusted,
        )?;
        let debt_net_in = checked_sub(q.split.debt_in_adjusted_gross, q.split.debt_fee_adjusted)?;
        apply_v1_side_imaginary(
            &mut new.collateral,
            direction,
            col_net_in,
            q.split.collateral_out_adjusted,
        )?;
        apply_v1_side_imaginary(
            &mut new.debt,
            direction,
            debt_net_in,
            q.split.debt_out_adjusted,
        )?;
        Ok((q.amount_out_native, new))
    }

    #[inline(always)]
    fn pre_quote_checks(&self) -> Result<()> {
        if self.swaps_paused {
            return Err(AmmError::PoolPaused);
        }
        if !self.smart_collateral_enabled && !self.smart_debt_enabled {
            return Err(AmmError::BothV1SidesDisabled);
        }
        Ok(())
    }

    #[inline(always)]
    fn input_scale(&self, direction: SwapDirection) -> TokenScale {
        match direction {
            SwapDirection::ZeroToOne => self.token0_scale,
            SwapDirection::OneToZero => self.token1_scale,
        }
    }

    #[inline(always)]
    fn output_scale(&self, direction: SwapDirection) -> TokenScale {
        match direction {
            SwapDirection::ZeroToOne => self.token1_scale,
            SwapDirection::OneToZero => self.token0_scale,
        }
    }

    fn check_output_limits(
        &self,
        direction: SwapDirection,
        now: u64,
        collateral_out_adjusted: U256,
        debt_out_adjusted: U256,
    ) -> Result<()> {
        let (withdrawable_native, borrowable_native, out_scale) = match direction {
            SwapDirection::ZeroToOne => (
                expand_v1_limit(
                    self.limits.withdrawable_token1,
                    self.limits.snapshot_timestamp,
                    now,
                ),
                expand_v1_limit(
                    self.limits.borrowable_token1,
                    self.limits.snapshot_timestamp,
                    now,
                ),
                self.token1_scale,
            ),
            SwapDirection::OneToZero => (
                expand_v1_limit(
                    self.limits.withdrawable_token0,
                    self.limits.snapshot_timestamp,
                    now,
                ),
                expand_v1_limit(
                    self.limits.borrowable_token0,
                    self.limits.snapshot_timestamp,
                    now,
                ),
                self.token0_scale,
            ),
        };

        let withdrawable_adjusted = out_scale.native_to_adjusted_floor(withdrawable_native)?;
        let borrowable_adjusted = out_scale.native_to_adjusted_floor(borrowable_native)?;

        if collateral_out_adjusted > withdrawable_adjusted
            || debt_out_adjusted > borrowable_adjusted
        {
            return Err(AmmError::LimitExceeded);
        }
        Ok(())
    }

    fn check_reserve_ratios(&self) -> Result<()> {
        if self.smart_collateral_enabled {
            check_v1_reserves_ratio(self.collateral.imaginary_pair())?;
        }
        if self.smart_debt_enabled {
            check_v1_reserves_ratio(self.debt.imaginary_pair())?;
        }
        Ok(())
    }
}

#[inline(always)]
pub fn expand_v1_limit(limit: V1ExpandableLimit, snapshot_timestamp: u64, now: u64) -> U256 {
    if now <= snapshot_timestamp {
        return limit.available_native;
    }
    if limit.expand_duration_left_secs == 0 || limit.expands_to_native <= limit.available_native {
        return limit.available_native;
    }
    let elapsed = now.saturating_sub(snapshot_timestamp);
    if elapsed >= limit.expand_duration_left_secs {
        return limit.expands_to_native;
    }
    let delta = limit.expands_to_native - limit.available_native;
    match mul_div_floor(
        delta,
        U256::from(elapsed),
        U256::from(limit.expand_duration_left_secs),
    ) {
        Ok(grown) => limit.available_native + grown,
        Err(_) => limit.available_native,
    }
}

#[inline(always)]
pub fn v1_amount_out(
    amount_in_after_fee: U256,
    reserve_in: U256,
    reserve_out: U256,
) -> Result<U256> {
    if amount_in_after_fee.is_zero() {
        return Ok(U256::ZERO);
    }
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(AmmError::ZeroReserves);
    }
    let denominator = checked_add(reserve_in, amount_in_after_fee)?;
    mul_div_floor(amount_in_after_fee, reserve_out, denominator)
}

/// Constant-product transition on one side's imaginary reserves for a forward-apply: the input
/// token's reserve rises by the net (after-fee) input, the output token's reserve falls by the
/// output. A side that wasn't routed (both deltas zero) is left untouched. `saturating_sub` guards
/// the (kernel-prevented) case `out >= reserve` so a malformed split can never underflow.
#[inline(always)]
fn apply_v1_side_imaginary(
    side: &mut V1SideReservesAdjusted,
    direction: SwapDirection,
    in_adjusted_net: U256,
    out_adjusted: U256,
) -> Result<()> {
    if in_adjusted_net.is_zero() && out_adjusted.is_zero() {
        return Ok(());
    }
    match direction {
        SwapDirection::ZeroToOne => {
            side.token0_imaginary = checked_add(side.token0_imaginary, in_adjusted_net)?;
            side.token1_imaginary = side.token1_imaginary.saturating_sub(out_adjusted);
        }
        SwapDirection::OneToZero => {
            side.token1_imaginary = checked_add(side.token1_imaginary, in_adjusted_net)?;
            side.token0_imaginary = side.token0_imaginary.saturating_sub(out_adjusted);
        }
    }
    Ok(())
}

#[inline(always)]
pub fn v1_amount_in_floor(amount_out: U256, reserve_in: U256, reserve_out: U256) -> Result<U256> {
    if amount_out.is_zero() {
        return Ok(U256::ZERO);
    }
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(AmmError::ZeroReserves);
    }
    if amount_out >= reserve_out {
        return Err(AmmError::InsufficientLiquidity);
    }
    let denominator = reserve_out - amount_out;
    mul_div_floor(amount_out, reserve_in, denominator)
}

#[inline(always)]
pub fn v1_amount_in_ceil(amount_out: U256, reserve_in: U256, reserve_out: U256) -> Result<U256> {
    if amount_out.is_zero() {
        return Ok(U256::ZERO);
    }
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(AmmError::ZeroReserves);
    }
    if amount_out >= reserve_out {
        return Err(AmmError::InsufficientLiquidity);
    }
    mul_div_ceil(amount_out, reserve_in, reserve_out - amount_out)
}

/// Fluid public resolver `_swapRoutingIn` split, clamped to [0, total_in].
pub fn v1_swap_routing_in(
    total_in_adjusted: U256,
    col_out_reserve: U256,
    col_in_reserve: U256,
    debt_out_reserve: U256,
    debt_in_reserve: U256,
) -> Result<U256> {
    if total_in_adjusted.is_zero() {
        return Ok(U256::ZERO);
    }

    let precision = U512::from(1_000_000_000_000_000_000u128);
    let col_root = sqrt_u512(U512::from(col_out_reserve) * U512::from(col_in_reserve) * precision);
    let debt_root =
        sqrt_u512(U512::from(debt_out_reserve) * U512::from(debt_in_reserve) * precision);
    let denom = col_root + debt_root;
    if denom.is_zero() {
        return Err(AmmError::ZeroReserves);
    }

    let lhs = U512::from(debt_in_reserve) * col_root + U512::from(total_in_adjusted) * col_root;
    let rhs = U512::from(col_in_reserve) * debt_root;
    if lhs <= rhs {
        return Ok(U256::ZERO);
    }

    let route = u512_to_u256((lhs - rhs) / denom)?;
    Ok(if route > total_in_adjusted {
        total_in_adjusted
    } else {
        route
    })
}

/// Fluid public resolver `_swapRoutingOut` split, clamped to [0, total_out].
pub fn v1_swap_routing_out(
    total_out_adjusted: U256,
    col_in_reserve: U256,
    col_out_reserve: U256,
    debt_in_reserve: U256,
    debt_out_reserve: U256,
) -> Result<U256> {
    if total_out_adjusted.is_zero() {
        return Ok(U256::ZERO);
    }

    let precision = U512::from(1_000_000_000_000_000_000u128);
    let col_root = sqrt_u512(U512::from(col_in_reserve) * U512::from(col_out_reserve) * precision);
    let debt_root =
        sqrt_u512(U512::from(debt_in_reserve) * U512::from(debt_out_reserve) * precision);
    let denom = col_root + debt_root;
    if denom.is_zero() {
        return Err(AmmError::ZeroReserves);
    }

    let lhs = U512::from(total_out_adjusted) * col_root + U512::from(col_out_reserve) * debt_root;
    let rhs = U512::from(debt_out_reserve) * col_root;
    if lhs <= rhs {
        return Ok(U256::ZERO);
    }

    let route = u512_to_u256((lhs - rhs) / denom)?;
    Ok(if route > total_out_adjusted {
        total_out_adjusted
    } else {
        route
    })
}

pub fn route_v1_exact_in(
    amount_in_adjusted: U256,
    collateral: V1ImaginaryPair,
    debt: V1ImaginaryPair,
    direction: SwapDirection,
    fee_1e6: u32,
    collateral_enabled: bool,
    debt_enabled: bool,
) -> Result<V1SplitQuote> {
    let (col_in_res, col_out_res, debt_in_res, debt_out_res) =
        directional_v1_reserves(collateral, debt, direction);

    if !collateral_enabled && !debt_enabled {
        return Err(AmmError::BothV1SidesDisabled);
    }

    let (col_in_gross, debt_in_gross) = if collateral_enabled && debt_enabled {
        let col_in = v1_swap_routing_in(
            amount_in_adjusted,
            col_out_res,
            col_in_res,
            debt_out_res,
            debt_in_res,
        )?;
        (col_in, amount_in_adjusted - col_in)
    } else if collateral_enabled {
        (amount_in_adjusted, U256::ZERO)
    } else {
        (U256::ZERO, amount_in_adjusted)
    };

    let col_net = apply_fee_floor(col_in_gross, fee_1e6)?;
    let debt_net = apply_fee_floor(debt_in_gross, fee_1e6)?;
    let col_fee = col_in_gross - col_net;
    let debt_fee = debt_in_gross - debt_net;

    let col_out = if col_in_gross.is_zero() {
        U256::ZERO
    } else {
        v1_amount_out(col_net, col_in_res, col_out_res)?
    };
    let debt_out = if debt_in_gross.is_zero() {
        U256::ZERO
    } else {
        v1_amount_out(debt_net, debt_in_res, debt_out_res)?
    };

    Ok(V1SplitQuote {
        collateral_in_adjusted_gross: col_in_gross,
        debt_in_adjusted_gross: debt_in_gross,
        collateral_out_adjusted: col_out,
        debt_out_adjusted: debt_out,
        collateral_fee_adjusted: col_fee,
        debt_fee_adjusted: debt_fee,
    })
}

pub fn route_v1_exact_out(
    amount_out_adjusted: U256,
    collateral: V1ImaginaryPair,
    debt: V1ImaginaryPair,
    direction: SwapDirection,
    fee_1e6: u32,
    collateral_enabled: bool,
    debt_enabled: bool,
    rounding: V1ExactOutRounding,
) -> Result<V1SplitQuote> {
    let (col_in_res, col_out_res, debt_in_res, debt_out_res) =
        directional_v1_reserves(collateral, debt, direction);

    if !collateral_enabled && !debt_enabled {
        return Err(AmmError::BothV1SidesDisabled);
    }

    let (col_out, debt_out) = if collateral_enabled && debt_enabled {
        let col_out = v1_swap_routing_out(
            amount_out_adjusted,
            col_in_res,
            col_out_res,
            debt_in_res,
            debt_out_res,
        )?;
        (col_out, amount_out_adjusted - col_out)
    } else if collateral_enabled {
        (amount_out_adjusted, U256::ZERO)
    } else {
        (U256::ZERO, amount_out_adjusted)
    };

    let (col_after_fee, debt_after_fee) = match rounding {
        V1ExactOutRounding::ResolverParity => (
            if col_out.is_zero() {
                U256::ZERO
            } else {
                v1_amount_in_floor(col_out, col_in_res, col_out_res)?
            },
            if debt_out.is_zero() {
                U256::ZERO
            } else {
                v1_amount_in_floor(debt_out, debt_in_res, debt_out_res)?
            },
        ),
        V1ExactOutRounding::ExecutionSafe => (
            if col_out.is_zero() {
                U256::ZERO
            } else {
                v1_amount_in_ceil(col_out, col_in_res, col_out_res)?
            },
            if debt_out.is_zero() {
                U256::ZERO
            } else {
                v1_amount_in_ceil(debt_out, debt_in_res, debt_out_res)?
            },
        ),
    };

    let col_gross = match rounding {
        V1ExactOutRounding::ResolverParity => gross_up_fee_floor(col_after_fee, fee_1e6)?,
        V1ExactOutRounding::ExecutionSafe => gross_up_fee_ceil(col_after_fee, fee_1e6)?,
    };
    let debt_gross = match rounding {
        V1ExactOutRounding::ResolverParity => gross_up_fee_floor(debt_after_fee, fee_1e6)?,
        V1ExactOutRounding::ExecutionSafe => gross_up_fee_ceil(debt_after_fee, fee_1e6)?,
    };

    Ok(V1SplitQuote {
        collateral_in_adjusted_gross: col_gross,
        debt_in_adjusted_gross: debt_gross,
        collateral_out_adjusted: col_out,
        debt_out_adjusted: debt_out,
        collateral_fee_adjusted: if col_gross >= col_after_fee {
            col_gross - col_after_fee
        } else {
            U256::ZERO
        },
        debt_fee_adjusted: if debt_gross >= debt_after_fee {
            debt_gross - debt_after_fee
        } else {
            U256::ZERO
        },
    })
}

#[inline(always)]
fn directional_v1_reserves(
    collateral: V1ImaginaryPair,
    debt: V1ImaginaryPair,
    direction: SwapDirection,
) -> (U256, U256, U256, U256) {
    match direction {
        SwapDirection::ZeroToOne => (
            collateral.token0,
            collateral.token1,
            debt.token0,
            debt.token1,
        ),
        SwapDirection::OneToZero => (
            collateral.token1,
            collateral.token0,
            debt.token1,
            debt.token0,
        ),
    }
}

pub fn dominant_v1_price_exact_in(
    collateral: V1ImaginaryPair,
    debt: V1ImaginaryPair,
    direction: SwapDirection,
    split: V1SplitQuote,
) -> Result<(U256, U256)> {
    let use_collateral = split.collateral_in_adjusted_gross > split.debt_in_adjusted_gross;
    let (reserve_in, reserve_out, amount_in, amount_out) = match (direction, use_collateral) {
        (SwapDirection::ZeroToOne, true) => (
            collateral.token0,
            collateral.token1,
            split.collateral_in_adjusted_gross,
            split.collateral_out_adjusted,
        ),
        (SwapDirection::ZeroToOne, false) => (
            debt.token0,
            debt.token1,
            split.debt_in_adjusted_gross,
            split.debt_out_adjusted,
        ),
        (SwapDirection::OneToZero, true) => (
            collateral.token1,
            collateral.token0,
            split.collateral_in_adjusted_gross,
            split.collateral_out_adjusted,
        ),
        (SwapDirection::OneToZero, false) => (
            debt.token1,
            debt.token0,
            split.debt_in_adjusted_gross,
            split.debt_out_adjusted,
        ),
    };

    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(AmmError::ZeroReserves);
    }
    if amount_out >= reserve_out {
        return Err(AmmError::InsufficientLiquidity);
    }

    match direction {
        SwapDirection::ZeroToOne => {
            let old_price = mul_div_floor(reserve_out, one_e27(), reserve_in)?;
            let new_reserve_in = checked_add(reserve_in, amount_in)?;
            let new_price = mul_div_floor(reserve_out - amount_out, one_e27(), new_reserve_in)?;
            Ok((old_price, new_price))
        }
        SwapDirection::OneToZero => {
            let old_price = mul_div_floor(reserve_in, one_e27(), reserve_out)?;
            let new_reserve_in = checked_add(reserve_in, amount_in)?;
            let new_price = mul_div_floor(new_reserve_in, one_e27(), reserve_out - amount_out)?;
            Ok((old_price, new_price))
        }
    }
}

#[inline(always)]
pub fn enforce_v1_price_move(old_price_1e27: U256, new_price_1e27: U256) -> Result<()> {
    let diff = abs_diff(old_price_1e27, new_price_1e27);
    let allowed = mul_div_floor(
        old_price_1e27,
        U256::from(FLUID_V1_PRICE_MOVE_LIMIT_1E18),
        one_e18(),
    )?;
    if diff > allowed {
        return Err(AmmError::PriceMoveTooHigh);
    }
    Ok(())
}

#[inline(always)]
pub fn check_v1_reserves_ratio(reserves: V1ImaginaryPair) -> Result<()> {
    if reserves.token0.is_zero() || reserves.token1.is_zero() {
        return Ok(());
    }
    let ratio = mul_div_floor(reserves.token0, one_e12(), reserves.token1)?;
    let inverse = mul_div_floor(reserves.token1, one_e12(), reserves.token0)?;
    let min_ratio = U256::from(FLUID_V1_MIN_RESERVES_RATIO_1E12);
    if ratio < min_ratio || inverse < min_ratio {
        return Err(AmmError::ReservesRatioTooExtreme);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale() -> TokenScale {
        TokenScale {
            numerator: U256::from(1u8),
            denominator: U256::from(1u8),
        }
    }

    #[test]
    fn symmetric_v1_exact_in_splits_evenly() {
        // Fixture: 1e9 reserves so 100k input is a 0.01 % price move,
        // well under the 5 % price-move guard in `enforce_v1_price_move`.
        // (Upstream fixture used 1e6 / 100k = 10 % move → tripped the guard.)
        let side = V1SideReservesAdjusted {
            token0_imaginary: U256::from(1_000_000_000u64),
            token1_imaginary: U256::from(1_000_000_000u64),
            ..Default::default()
        };
        let pool = V1PoolState {
            fee_1e6: 0,
            token0_scale: scale(),
            token1_scale: scale(),
            collateral: side,
            debt: side,
            limits: V1LimitsNative {
                withdrawable_token1: V1ExpandableLimit {
                    available_native: U256::from(1_000_000_000u64),
                    expands_to_native: U256::from(1_000_000_000u64),
                    expand_duration_left_secs: 0,
                },
                borrowable_token1: V1ExpandableLimit {
                    available_native: U256::from(1_000_000_000u64),
                    expands_to_native: U256::from(1_000_000_000u64),
                    expand_duration_left_secs: 0,
                },
                ..Default::default()
            },
            smart_collateral_enabled: true,
            smart_debt_enabled: true,
            swaps_paused: false,
        };
        let q = pool
            .quote_exact_in(SwapDirection::ZeroToOne, U256::from(100_000u64), 0)
            .unwrap();
        assert_eq!(q.split.collateral_in_adjusted_gross, U256::from(50_000u64));
        assert_eq!(q.split.debt_in_adjusted_gross, U256::from(50_000u64));
    }

    // AUD-021: the audit claimed `route_v1_exact_in` lacks a "50% input cap", letting the kernel
    // phantom-over-quote a swap that consumes >50% of imaginary reserves. But the actual entry point
    // `quote_exact_in` applies `enforce_v1_price_move` (FLUID_V1_PRICE_MOVE_LIMIT_1E18 = 5e16 = 5%),
    // which fires FAR earlier than any 50% cap: a >50%-of-reserve swap moves the price ~50% >> 5%, so
    // the quote fail-closes with `PriceMoveTooHigh` — no over-quote can escape. This locks that.
    #[test]
    fn oversized_v1_swap_fails_closed_via_price_move_guard() {
        let pool = symmetric_pool(); // 1e9 imaginary reserves per side, fee=0
                                     // 1e9 input = 100% of one side's reserve (≈25% of total imaginary) → ~50% price move.
        let err = pool
            .quote_exact_in(SwapDirection::ZeroToOne, U256::from(1_000_000_000u64), 0)
            .expect_err("oversized Fluid swap must NOT return an over-quote");
        assert!(
            matches!(err, AmmError::PriceMoveTooHigh),
            "oversized Fluid swap must fail-closed via the 5% price-move guard, got {err:?}"
        );
        // And a swap small enough to stay under the 5% guard still quotes normally (sanity).
        let ok = pool
            .quote_exact_in(SwapDirection::ZeroToOne, U256::from(1_000_000u64), 0)
            .expect("a 0.1%-impact swap must quote");
        assert!(ok.amount_out_native > U256::ZERO);
    }

    fn symmetric_pool() -> V1PoolState {
        let side = V1SideReservesAdjusted {
            token0_imaginary: U256::from(1_000_000_000u64),
            token1_imaginary: U256::from(1_000_000_000u64),
            ..Default::default()
        };
        V1PoolState {
            fee_1e6: 0,
            token0_scale: scale(),
            token1_scale: scale(),
            collateral: side,
            debt: side,
            limits: {
                let big = V1ExpandableLimit {
                    available_native: U256::from(1_000_000_000u64),
                    expands_to_native: U256::from(1_000_000_000u64),
                    expand_duration_left_secs: 0,
                };
                V1LimitsNative {
                    withdrawable_token0: big,
                    withdrawable_token1: big,
                    borrowable_token0: big,
                    borrowable_token1: big,
                    snapshot_timestamp: 0,
                }
            },
            smart_collateral_enabled: true,
            smart_debt_enabled: true,
            swaps_paused: false,
        }
    }

    #[test]
    fn apply_exact_in_moves_imaginary_reserves_and_stays_consistent() {
        let pool = symmetric_pool();
        let (out, new) = pool
            .apply_exact_in(SwapDirection::ZeroToOne, U256::from(100_000u64), 0)
            .unwrap();
        assert!(out > U256::ZERO);
        // Even split, fee=0: 50k net input added to EACH side's token0 imaginary reserve.
        assert_eq!(
            new.collateral.token0_imaginary,
            U256::from(1_000_050_000u64)
        );
        assert_eq!(new.debt.token0_imaginary, U256::from(1_000_050_000u64));
        // Output left each side → token1 imaginary fell.
        assert!(new.collateral.token1_imaginary < U256::from(1_000_000_000u64));
        assert!(new.debt.token1_imaginary < U256::from(1_000_000_000u64));
        // A swap doesn't mutate limits, and real reserves don't drive the quote → both carried as-is.
        assert_eq!(new.limits, pool.limits);
        assert_eq!(new.collateral.token0_real, pool.collateral.token0_real);
        // Self-consistency: re-quoting the SAME swap on the predicted state yields strictly less
        // output (price moved against the next buyer) — proves the imaginary reserves stay coherent.
        let q2 = new
            .quote_exact_in(SwapDirection::ZeroToOne, U256::from(100_000u64), 0)
            .unwrap();
        assert!(
            q2.amount_out_native < out,
            "price moved against the next identical buy"
        );
    }

    #[test]
    fn apply_exact_in_one_to_zero_moves_token1_in_token0_out() {
        let pool = symmetric_pool();
        let (out, new) = pool
            .apply_exact_in(SwapDirection::OneToZero, U256::from(100_000u64), 0)
            .unwrap();
        assert!(out > U256::ZERO);
        // Reversed direction: token1 imaginary rises, token0 imaginary falls.
        assert_eq!(
            new.collateral.token1_imaginary,
            U256::from(1_000_050_000u64)
        );
        assert!(new.collateral.token0_imaginary < U256::from(1_000_000_000u64));
    }
}
