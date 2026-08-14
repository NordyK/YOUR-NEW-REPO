use alloy_primitives::U256;

use super::fixed::{mul_div_ceil, mul_div_floor};
use super::Result;

/// Generic swap direction used by both Fluid v1 and Fluid v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapDirection {
    /// token0 -> token1
    ZeroToOne,
    /// token1 -> token0
    OneToZero,
}

impl SwapDirection {
    #[inline(always)]
    pub const fn is_zero_to_one(self) -> bool {
        matches!(self, Self::ZeroToOne)
    }

    #[inline(always)]
    pub const fn sign(self) -> i8 {
        match self {
            Self::ZeroToOne => -1,
            Self::OneToZero => 1,
        }
    }
}

/// Fluid v1 adjusted-reserve precision. Resolver "Adjusted" reserve methods use
/// 1e12 scale regardless of token native decimals.
pub const FLUID_V1_ADJUSTED_PRECISION: u64 = 1_000_000_000_000;

/// Fluid fee precision. Both v1 poolFee and v2 fee tiers use 1e6 precision:
/// 500 = 0.05%, 3000 = 0.3%, 1_000_000 = 100%.
pub const FEE_DENOMINATOR_1E6: u64 = 1_000_000;

/// Fluid v1 price-move guard used by the public resolver path: 5% on 1e18 scale.
pub const FLUID_V1_PRICE_MOVE_LIMIT_1E18: u64 = 50_000_000_000_000_000;

/// Fluid v1 minimum reserve-ratio guard expressed on 1e12 scale.
pub const FLUID_V1_MIN_RESERVES_RATIO_1E12: u64 = 1_000_000;

/// Q96 fixed-point scale used by Fluid v2 state (`currentSqrtPriceX96`).
pub const Q96_U128: u128 = 79_228_162_514_264_337_593_543_950_336u128;

/// Per-token native <-> Fluid v1 adjusted scaling.
///
/// The v1 resolver exposes `token{0,1}NumeratorPrecision` and
/// `token{0,1}DenominatorPrecision` through `constantsView2()`. The exact
/// conversion is:
///
/// - adjusted = native * numerator / denominator
/// - native   = adjusted * denominator / numerator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenScale {
    pub numerator: U256,
    pub denominator: U256,
}

impl TokenScale {
    #[inline(always)]
    pub const fn new(numerator: U256, denominator: U256) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    #[inline(always)]
    pub fn native_to_adjusted_floor(self, amount_native: U256) -> Result<U256> {
        mul_div_floor(amount_native, self.numerator, self.denominator)
    }

    #[inline(always)]
    pub fn native_to_adjusted_ceil(self, amount_native: U256) -> Result<U256> {
        mul_div_ceil(amount_native, self.numerator, self.denominator)
    }

    #[inline(always)]
    pub fn adjusted_to_native_floor(self, amount_adjusted: U256) -> Result<U256> {
        mul_div_floor(amount_adjusted, self.denominator, self.numerator)
    }

    #[inline(always)]
    pub fn adjusted_to_native_ceil(self, amount_adjusted: U256) -> Result<U256> {
        mul_div_ceil(amount_adjusted, self.denominator, self.numerator)
    }
}

/// Quote request in native token units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactInRequest {
    pub direction: SwapDirection,
    pub amount_in: U256,
    /// Unix timestamp used only for Fluid v1 local limit expansion. For Fluid v2
    /// this is ignored by the core math and may be used by the host dynamic-fee layer.
    pub now: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactOutRequest {
    pub direction: SwapDirection,
    pub amount_out: U256,
    pub now: u64,
}

/// Minimal normalized exact-input quote returned by the generic kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelExactInQuote {
    pub amount_in: U256,
    pub amount_out: U256,
    pub effective_fee_paid: U256,
}

/// Minimal normalized exact-output quote returned by the generic kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelExactOutQuote {
    pub amount_in: U256,
    pub amount_out: U256,
    pub effective_fee_paid: U256,
}
