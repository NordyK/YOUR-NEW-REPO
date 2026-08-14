//! Structured error types for the scythe AMM kernels.
//!
//! Two enums: `V2MathError` (constant-product) and `V3MathError` (concentrated liquidity
//! + tick math). Both impl `std::error::Error` via `thiserror::Error`. The variants carry
//! enough context (operation name, offending value) to make a failed swap diagnosable
//! from a log line.

use alloy_primitives::U256;
use thiserror::Error;

// ============================================================================
// V2
// ============================================================================

/// Errors that can occur during Uniswap V2 mathematical operations.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum V2MathError {
    /// Invalid fee value (must be ≤ 10000 basis points).
    #[error("invalid fee: {fee_bps} bps (must be ≤ 10 000)")]
    InvalidFee {
        /// Fee in basis points that was supplied.
        fee_bps: u16,
    },

    /// Zero liquidity in pool, cannot execute swap.
    #[error("zero liquidity in pool, cannot execute swap")]
    ZeroLiquidity,

    /// Arithmetic overflow in V2 calculation.
    #[error("arithmetic overflow in V2 calculation")]
    ArithmeticOverflow,
}

/// Result type for V2 math operations.
pub type V2MathResult<T> = Result<T, V2MathError>;

// ============================================================================
// V3
// ============================================================================

/// Errors that can occur during Uniswap V3 mathematical operations.
#[derive(Error, Debug, Clone)]
pub enum V3MathError {
    /// Tick value is outside the valid range `[-887272, 887272]`.
    #[error("tick {tick} out of bounds [{min}, {max}]",
            min = super::uniswap_v3::MIN_TICK,
            max = super::uniswap_v3::MAX_TICK)]
    TickOutOfBounds {
        /// Offending tick.
        tick: i32,
    },

    /// Liquidity is zero, making swap calculations impossible.
    #[error("zero liquidity in pool, cannot execute swap")]
    ZeroLiquidity,

    /// Arithmetic overflow occurred during calculation.
    #[error("arithmetic overflow in {operation}: {context}")]
    ArithmeticOverflow {
        /// Operation name (where the overflow happened).
        operation: &'static str,
        /// Free-form context (operand values).
        context: String,
    },

    /// Division by zero attempted.
    #[error("division by zero in {operation}")]
    DivisionByZero {
        /// Operation name.
        operation: &'static str,
    },

    /// Invalid sqrt price value.
    #[error("invalid sqrt price: {value} (must be > 0 and within bounds)")]
    InvalidSqrtPrice {
        /// Offending sqrt-price value.
        value: U256,
    },

    /// Amount is too small to process.
    #[error("amount too small: {amount} (minimum required: {minimum})")]
    AmountTooSmall {
        /// Offending amount.
        amount: U256,
        /// Minimum required.
        minimum: U256,
    },

    /// Internal consistency error (should never happen).
    #[error("internal consistency error: {message}")]
    InternalError {
        /// Diagnostic message.
        message: String,
    },

    /// Invalid fee tier (must be ≤ 1 000 000 ppm).
    #[error("invalid fee tier: {fee_ppm} ppm (must be ≤ 1 000 000)")]
    InvalidFeeTier {
        /// Offending fee.
        fee_ppm: u32,
    },

    /// Liquidity amount exceeds `i128::MAX`.
    #[error(
        "liquidity overflow in {operation}: amount={amount} exceeds i128::MAX (pool likely malicious)"
    )]
    LiquidityOverflow {
        /// Operation name.
        operation: &'static str,
        /// Offending amount.
        amount: u128,
    },

    /// Liquidity underflowed below zero (subtraction would go negative).
    #[error(
        "liquidity underflow in {operation}: current={current}, delta={delta} (pool state corrupted)"
    )]
    LiquidityUnderflow {
        /// Operation name.
        operation: &'static str,
        /// Current liquidity.
        current: u128,
        /// Subtraction delta that would have gone below zero.
        delta: u128,
    },
}

/// Result type for V3 math operations.
pub type V3MathResult<T> = Result<T, V3MathError>;

impl V3MathError {
    /// Create an overflow error with context.
    pub fn overflow(operation: &'static str, context: impl Into<String>) -> Self {
        Self::ArithmeticOverflow {
            operation,
            context: context.into(),
        }
    }

    /// Create a division-by-zero error.
    pub fn div_by_zero(operation: &'static str) -> Self {
        Self::DivisionByZero { operation }
    }

    /// Create an internal error (for invariant violations).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::InternalError {
            message: message.into(),
        }
    }
}
