use core::fmt;

/// Kernel error type. The host can map these to "pool unavailable", zero quote,
/// resolver fallback, or hard rejection depending on its routing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmmError {
    DivisionByZero,
    MathOverflow,
    InvalidFee,
    InvalidDirection,
    PoolPaused,
    NoLiquidity,
    BothV1SidesDisabled,
    ZeroReserves,
    InsufficientLiquidity,
    LimitExceeded,
    PriceMoveTooHigh,
    ReservesRatioTooExtreme,
    SegmentOrder,
    TickOutOfRange,
    SqrtPriceOutOfRange,
    OutputWouldExhaustPriceRange,
    MissingV2Segments,
}

pub type Result<T> = core::result::Result<T, AmmError>;

impl fmt::Display for AmmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            AmmError::DivisionByZero => "division by zero",
            AmmError::MathOverflow => "math overflow",
            AmmError::InvalidFee => "invalid fee",
            AmmError::InvalidDirection => "invalid swap direction",
            AmmError::PoolPaused => "pool is paused",
            AmmError::NoLiquidity => "no active liquidity",
            AmmError::BothV1SidesDisabled => "both Fluid v1 collateral and debt sides are disabled",
            AmmError::ZeroReserves => "zero reserves",
            AmmError::InsufficientLiquidity => "insufficient liquidity",
            AmmError::LimitExceeded => "liquidity-layer limit exceeded",
            AmmError::PriceMoveTooHigh => "price move exceeds Fluid guard",
            AmmError::ReservesRatioTooExtreme => "reserves ratio is outside accepted range",
            AmmError::SegmentOrder => "v2 segment order does not match swap direction",
            AmmError::TickOutOfRange => "tick is outside supported range",
            AmmError::SqrtPriceOutOfRange => "sqrt price is outside supported range",
            AmmError::OutputWouldExhaustPriceRange => {
                "exact-output request exhausts available v2 price range"
            }
            AmmError::MissingV2Segments => {
                "v2 quote requires at least current segment liquidity or boundary data"
            }
        };
        f.write_str(msg)
    }
}

impl std::error::Error for AmmError {}
