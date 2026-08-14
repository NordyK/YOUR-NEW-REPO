//! Errors returned by exact AMM kernels.

use thiserror::Error;

/// Errors emitted by AMM kernels. All variants are pure data; the kernels never allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AmmError {
    /// Provided reserves or liquidity is zero where a positive value is required.
    #[error("zero liquidity")]
    ZeroLiquidity,
    /// Caller provided an input amount of zero.
    #[error("zero input amount")]
    ZeroInput,
    /// Computation overflowed a 256-bit field.
    #[error("overflow")]
    Overflow,
    /// Computation underflowed (negative balance).
    #[error("underflow")]
    Underflow,
    /// The requested swap would cross outside the warmed tick window.
    #[error("warmed tick window exhausted")]
    TickWindowExhausted,
    /// Division by zero or invariant violated.
    #[error("invariant violated")]
    InvariantViolation,
    /// V3 sqrt price out of canonical bounds.
    #[error("sqrt price out of bounds")]
    SqrtPriceOutOfBounds,
    /// V3 tick out of canonical bounds.
    #[error("tick out of bounds")]
    TickOutOfBounds,
    /// Fee parameter is malformed.
    #[error("invalid fee")]
    InvalidFee,
}

/// Convenience alias.
pub type AmmResult<T> = Result<T, AmmError>;
