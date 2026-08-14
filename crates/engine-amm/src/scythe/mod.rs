//! Scythe-engine AMM kernels (integrated from the operator-uploaded research code).
//!
//! These are alloy-native AMM kernels with three properties our existing
//! `engine_amm::{uniswap_v2, uniswap_v3, curve}` modules do not have:
//!
//! 1. **u128 fast path on V2 `get_amount_out`** — falls back to U256 only on overflow.
//!    Significantly faster on the common case (deep pools whose reserves and amount fit
//!    in u128). Hot-path grid scan benefits the most.
//!
//! 2. **Analytical V2 optimal-input solver** — closed-form solution for the V2/V2 single
//!    hop sizing problem (`solve_optimal_v2_single_hop`). Plan §291 explicitly names this
//!    symbol. Today our `pipeline::evaluate_two_hop` grid-scans; the analytical seed is
//!    the bounded-search starting point the plan describes ("amount-grid + bounded
//!    search").
//!
//! 3. **Thread-local direct-mapped tick → sqrtPrice cache** — avoids recomputing
//!    `sqrt_ratio_at_tick` for the same tick across the binary search inside the V3
//!    grid scan. Each cache hit saves 200–400 ns.
//!
//! The kernels speak `alloy_primitives::U256` / `alloy_primitives::Address`. The existing
//! ruint-based kernels remain in place; cross-check parity tests live in `tests::parity`
//! and assert bit-exact agreement so callers can pick either with confidence.

#![allow(missing_docs)] // research module — docs follow on each public item

pub mod errors;
pub mod fixed_point;
pub mod uniswap_v2;
pub mod uniswap_v2_solver;
pub mod uniswap_v3;

pub use errors::{V2MathError, V2MathResult, V3MathError, V3MathResult};
pub use fixed_point::isqrt_u256;
pub use uniswap_v2::{
    get_amount_out as v2_get_amount_out_scythe, get_fee_factor_v2, FeeBps, UniswapV2Reserves,
    V2Book, V2Pair,
};
pub use uniswap_v2_solver::{solve_optimal_v2_single_hop, solve_optimal_v2_two_hop};
pub use uniswap_v3::{
    fee_from_path_bytes, get_amount_out as v3_get_amount_out_scythe, get_amount_out_with_ticks,
    mul_div, sqrt_ratio_at_tick, tick_at_sqrt_ratio, UniswapV3Slot0, V3PoolState, MAX_SQRT_RATIO,
    MAX_TICK as V3_MAX_TICK, MIN_SQRT_RATIO, MIN_TICK as V3_MIN_TICK, Q96,
};
