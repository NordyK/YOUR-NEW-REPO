//! Exact-integer AMM math kernels.
//!
//! All swap math is integer-exact and bit-identical to the deployed-source contracts:
//! * Uniswap V2 / Sushi V2 (canonical 997/1000 constant product) and Camelot V2 dynamic fees.
//! * Uniswap V3 / Sushi V3 / PancakeSwap V3 / Algebra-family concentrated liquidity with full
//!   `SqrtPriceMath` and `TickMath` parity.
//!
//! The hot path never allocates; every entry point is a `fn` with `Copy` inputs.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// clippy: numeric-kernel false-positives — range indices deliberately span parallel arrays
// (xp/bal/x_old), byte-exact math fns legitimately take many params; the rest are cosmetic
// doc/style lints. Kernels are byte-exact-certified, so suppress rather than churn certified
// hot-path code. Every allow below is a zero-behavior-change attribute.
#![allow(
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::collapsible_else_if,
    clippy::single_match,
    clippy::doc_lazy_continuation,
    // false-positive on `live_reference_vectors_ready()`: a deliberate fail-closed gate whose
    // const operand is currently 0, so clippy sees "always false" — that IS the intent.
    clippy::absurd_extreme_comparisons
)]

pub mod balancer_stable;
pub mod balancer_weighted;
pub mod curve;
pub mod curve_cryptoswap;
pub mod curve_twocrypto;
pub mod dodo_v2;
pub mod error;
pub mod fixed_point;
pub mod fluid;
pub mod kyber_elastic;
pub mod maverick_v2;
pub mod scythe;
pub mod solidly_stable;
pub mod trader_joe_lb;
pub mod uniswap_v2;
pub mod uniswap_v3;

pub use curve::{
    apply_exchange as curve_apply_exchange, get_dy as curve_get_dy,
    StableSwapState as CurveStableSwapState, A_PRECISION as CURVE_A_PRECISION,
    FEE_DENOMINATOR as CURVE_FEE_DENOMINATOR, MAX_COINS as CURVE_MAX_COINS,
};
pub use error::{AmmError, AmmResult};
pub use uniswap_v2::{
    get_amount_in as v2_get_amount_in, get_amount_out as v2_get_amount_out, Reserves as V2Reserves,
    V2_FEE_BPS_DEFAULT,
};
pub use uniswap_v3::{
    get_amount_out as v3_get_amount_out,
    get_amount_out_with_final_tick as v3_get_amount_out_with_final_tick,
    next_sqrt_price_from_amount0_in, next_sqrt_price_from_amount1_in, sqrt_price_at_tick,
    tick_at_sqrt_price, Slot0 as V3Slot0, TickInfo, MAX_TICK, MIN_TICK,
};
