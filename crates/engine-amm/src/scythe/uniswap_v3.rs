//! WARNING (AUD-016): NON-BYTE-EXACT / TEST-ONLY V3 KERNEL — DO NOT WIRE TO PRODUCTION / FAST-LANE.
//!
//! This module is a CONSERVATIVE (under-quoting) V3 approximation kept only for cross-checks and
//! micro-benchmarks. It is NOT byte-exact vs the deployed Uniswap V3 contracts (it floors
//! `next_sqrt_price` instead of `mulDivRoundingUp` and collapses the two-step amount0 rounding).
//! The AUTHORITATIVE, byte-exact V3 kernel wired into sizing/pipeline is
//! `engine_amm::uniswap_v3::v3_get_amount_out` (proven 0.00 bps vs the on-chain V3 Quoter). Unlike
//! the V2 path, there is intentionally NO scythe-vs-canonical bit-exact V3 test because the two
//! deliberately differ. Promoting THIS kernel to the no-REVM fast lane believing it equals the
//! contract would ship a wrong quote to capital. It is unwired (zero production callers, no PoolKind).
//!
//! Uniswap V3 pricing + log application using alloy.rs primitives.
//! - Exact tick math (sqrtRatioAtTick) and deltas
//! - Accurate fee handling
//! - Multi-tick crossing quote is the default path (uses initialized ticks BTreeMap)
//! - Single-tick micro quote available as fallback when tick data is unavailable
//! - All decoding from relay logs with alloy types (Address/B256)

use alloy_primitives::{Address, U256};
use once_cell::sync::Lazy;
use ruint::Uint;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
};

// ============================================================================
// TICK → SQRT_PRICE CACHE (M4 optimization)
// ============================================================================
// Thread-local direct-mapped cache to avoid repeated expensive sqrt_ratio_at_tick()
// computations during binary search. Each call saves ~200-400ns.
//
// Implementation: Direct-mapped cache (hash(tick) % SIZE), no locking needed.
// Size configurable via TICK_CACHE_SIZE env var (default 4096, clamped to [256, 65536]).
// Memory: ~40 bytes × entries per thread.

const DEFAULT_TICK_CACHE_SIZE: usize = 4096;
const MIN_TICK_CACHE_SIZE: usize = 256;
const MAX_TICK_CACHE_SIZE: usize = 65536;

/// Returns the tick cache size, parsed from TICK_CACHE_SIZE env var on first call.
/// Clamped to [256, 65536] and rounded up to the next power of two (required for bitmask).
fn tick_cache_size() -> usize {
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        let raw: usize = std::env::var("TICK_CACHE_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_TICK_CACHE_SIZE);
        let clamped = raw.clamp(MIN_TICK_CACHE_SIZE, MAX_TICK_CACHE_SIZE);
        clamped.next_power_of_two().min(MAX_TICK_CACHE_SIZE)
    })
}

thread_local! {
    /// Direct-mapped cache: index = hash(tick) % cache_size
    /// Entry format: (tick, sqrt_price_x96), where tick=i32::MIN means empty
    static TICK_SQRT_CACHE: RefCell<Vec<(i32, U256)>> = {
        RefCell::new(vec![(i32::MIN, U256::ZERO); tick_cache_size()])
    };
}

/// Hash function for tick cache index (golden ratio multiplication)
#[inline(always)]
fn tick_cache_index(tick: i32) -> usize {
    ((tick as u32).wrapping_mul(0x9e3779b9) as usize) & (tick_cache_size() - 1)
}

// U512 type for overflow-safe tick ratio calculations
type U512 = Uint<512, 8>;

pub const MIN_TICK: i32 = -887_272;
pub const MAX_TICK: i32 = 887_272;

/// Minimum sqrt price ratio: sqrt_ratio_at_tick(MIN_TICK) = 4295128739
/// From Uniswap V3 TickMath.sol
pub static MIN_SQRT_RATIO: Lazy<U256> = Lazy::new(|| {
    "4295128739"
        .parse::<U256>()
        .expect("MIN_SQRT_RATIO constant")
});

/// Maximum sqrt price ratio: sqrt_ratio_at_tick(MAX_TICK)
/// From Uniswap V3 TickMath.sol
pub static MAX_SQRT_RATIO: Lazy<U256> = Lazy::new(|| {
    "1461446703485210103287273052203988822378723970342"
        .parse::<U256>()
        .expect("MAX_SQRT_RATIO constant")
});

/// Q96 = 2^96, the fixed-point scaling constant for Uniswap V3 sqrt price math.
/// Limbs: 2^96 = 2^64 × 2^32; in little-endian u64 limbs: [0, 1<<32, 0, 0].
pub const Q96: U256 = U256::from_limbs([0, 1u64 << 32, 0, 0]);

/// Returns Q96 constant. Kept as function for backward compatibility.
/// COR-023: Made public for use in optimal_sizing V3 math
#[inline(always)]
pub const fn q96() -> U256 {
    Q96
}

// Pre-parse tick math constants using Lazy to avoid unwrap() in hot path
// These constants are from Uniswap V3 TickMath.sol - values of 1.0001^(2^i) scaled by 2^128
// TICK_RATIO_BASE_ODD = 0xfffcb933bd6fad37aa2d162d1a594001 = sqrt(1/1.0001) * 2^128
static TICK_RATIO_BASE_ODD: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("fffcb933bd6fad37aa2d162d1a594001", 16)
        .expect("Uniswap V3 tick math constant (odd) should be valid")
});
static TICK_RATIO_BASE_EVEN: Lazy<U256> = Lazy::new(|| {
    "340282366920938463463374607431768211456"
        .parse::<U256>()
        .expect("Uniswap V3 tick math constant (even) should be valid")
});
static TICK_RATIO_0X2: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("fff97272373d413259a46990580e213a", 16)
        .expect("Uniswap V3 tick constant 0x2 should be valid")
});
static TICK_RATIO_0X4: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("fff2e50f5f656932ef12357cf3c7fdcc", 16)
        .expect("Uniswap V3 tick constant 0x4 should be valid")
});
static TICK_RATIO_0X8: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("ffe5caca7e10e4e61c3624eaa0941cd0", 16)
        .expect("Uniswap V3 tick constant 0x8 should be valid")
});
static TICK_RATIO_0X10: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("ffcb9843d60f6159c9db58835c926644", 16)
        .expect("Uniswap V3 tick constant 0x10 should be valid")
});
static TICK_RATIO_0X20: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("ff973b41fa98c081472e6896dfb254c0", 16)
        .expect("Uniswap V3 tick constant 0x20 should be valid")
});
static TICK_RATIO_0X40: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("ff2ea16466c96a3843ec78b326b52861", 16)
        .expect("Uniswap V3 tick constant 0x40 should be valid")
});
static TICK_RATIO_0X80: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("fe5dee046a99a2a811c461f1969c3053", 16)
        .expect("Uniswap V3 tick constant 0x80 should be valid")
});
static TICK_RATIO_0X100: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("fcbe86c7900a88aedcffc83b479aa3a4", 16)
        .expect("Uniswap V3 tick constant 0x100 should be valid")
});
static TICK_RATIO_0X200: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("f987a7253ac413176f2b074cf7815e54", 16)
        .expect("Uniswap V3 tick constant 0x200 should be valid")
});
static TICK_RATIO_0X400: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("f3392b0822b70005940c7a398e4b70f3", 16)
        .expect("Uniswap V3 tick constant 0x400 should be valid")
});
static TICK_RATIO_0X800: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("e7159475a2c29b7443b29c7fa6e889d9", 16)
        .expect("Uniswap V3 tick constant 0x800 should be valid")
});
static TICK_RATIO_0X1000: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("d097f3bdfd2022b8845ad8f792aa5825", 16)
        .expect("Uniswap V3 tick constant 0x1000 should be valid")
});
static TICK_RATIO_0X2000: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("a9f746462d870fdf8a65dc1f90e061e5", 16)
        .expect("Uniswap V3 tick constant 0x2000 should be valid")
});
static TICK_RATIO_0X4000: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("70d869a156d2a1b890bb3df62baf32f7", 16)
        .expect("Uniswap V3 tick constant 0x4000 should be valid")
});
static TICK_RATIO_0X8000: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("31be135f97d08fd981231505542fcfa6", 16)
        .expect("Uniswap V3 tick constant 0x8000 should be valid")
});
static TICK_RATIO_0X10000: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("9aa508b5b7a84e1c677de54f3e99bc9", 16)
        .expect("Uniswap V3 tick constant 0x10000 should be valid")
});
static TICK_RATIO_0X20000: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("5d6af8dedb81196699c329225ee604", 16)
        .expect("Uniswap V3 tick constant 0x20000 should be valid")
});
static TICK_RATIO_0X40000: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("2216e584f5fa1ea926041bedfe98", 16)
        .expect("Uniswap V3 tick constant 0x40000 should be valid")
});
static TICK_RATIO_0X80000: Lazy<U256> = Lazy::new(|| {
    U256::from_str_radix("48a170391f7dc42444e8fa2", 16)
        .expect("Uniswap V3 tick constant 0x80000 should be valid")
});

#[derive(Debug)]
pub struct V3PoolState {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee_ppm: u32,      // 500/3000/10000
    pub tick_spacing: i32, // 10/60/200
    pub sqrt_price_x96: U256,
    pub liquidity: U256,
    pub tick: i32,
    /// Liquidity net at initialized ticks (+L at lower, -L at upper)
    pub liquidity_net: BTreeMap<i32, i128>,
    /// Fast next/prev search of initialized ticks
    pub initialized_ticks: BTreeSet<i32>,
    pub last_update_ns: AtomicU64,
}

impl Clone for V3PoolState {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool,
            token0: self.token0,
            token1: self.token1,
            fee_ppm: self.fee_ppm,
            tick_spacing: self.tick_spacing,
            sqrt_price_x96: self.sqrt_price_x96,
            liquidity: self.liquidity,
            tick: self.tick,
            liquidity_net: self.liquidity_net.clone(),
            initialized_ticks: self.initialized_ticks.clone(),
            last_update_ns: AtomicU64::new(self.last_update_ns.load(Ordering::Relaxed)),
        }
    }
}

impl V3PoolState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: Address,
        token0: Address,
        token1: Address,
        fee_ppm: u32,
        tick_spacing: i32,
        sqrt_price_x96: U256,
        liquidity: U256,
        tick: i32,
    ) -> Self {
        Self {
            pool,
            token0,
            token1,
            fee_ppm,
            tick_spacing,
            sqrt_price_x96,
            liquidity,
            tick,
            liquidity_net: BTreeMap::new(),
            initialized_ticks: BTreeSet::new(),
            last_update_ns: AtomicU64::new(0),
        }
    }

    #[inline(always)]
    fn fee_num(&self) -> crate::scythe::errors::V3MathResult<U256> {
        let remaining = 1_000_000u32.checked_sub(self.fee_ppm).ok_or(
            crate::scythe::errors::V3MathError::InvalidFeeTier {
                fee_ppm: self.fee_ppm,
            },
        )?;
        Ok(U256::from(remaining))
    }
    #[inline(always)]
    fn fee_den(&self) -> U256 {
        U256::from(1_000_000u32)
    }

    // ---------- QUOTING ----------

    /// Single-tick micro quote (fallback when no initialized tick data is available).
    /// Assumes the trade stays within the current tick range — use `quote_exact_in_crossing()`
    /// for production accuracy when tick data is present.
    /// SEC-007: Updated to propagate mul_div overflow errors instead of saturating
    ///
    /// A6 (verified-correct, locked): this scythe single-tick path is TEST-ONLY / scaffolding. The
    /// WIRED authoritative V3 quoter is `engine_amm::v3_get_amount_out` (the top-level
    /// `uniswap_v3.rs::get_amount_out`, re-exported in `lib.rs`), which walks initialized ticks and
    /// is the one proven byte-exact against the on-chain V3 Quoter. This path is intentionally
    /// CONSERVATIVE: it caps the result at `max_output` (the most the current-tick liquidity can pay
    /// before the price would cross the range) and returns `0` when `sqrt_price`/`liquidity` are
    /// zero — i.e. it can only ever UNDER-quote, never over-quote, so it is capital-safe even where
    /// it is imprecise. Behavior is deliberately unchanged.
    #[inline(always)]
    pub fn quote_exact_in_single_tick(
        &self,
        amount_in: U256,
        zero_for_one: bool,
    ) -> crate::scythe::errors::V3MathResult<(U256, U256)> {
        if amount_in.is_zero() || self.liquidity.is_zero() {
            return Ok((U256::ZERO, self.sqrt_price_x96));
        }
        let fee_n = self.fee_num()?;
        let fee_d = self.fee_den();
        let a = mul_div(amount_in, fee_n, fee_d).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "quote_exact_in_single_tick",
                "fee calculation overflow".to_string(),
            )
        })?;
        if zero_for_one {
            // amount0 in → price DOWN
            let lq_q96: U256 = self.liquidity << 96;
            let product = mul_div(a, self.sqrt_price_x96, U256::from(1)).ok_or_else(|| {
                crate::scythe::errors::V3MathError::overflow(
                    "quote_exact_in_single_tick",
                    "product overflow".to_string(),
                )
            })?;

            let den = match lq_q96.checked_add(product) {
                Some(v) => v,
                None => return Ok((U256::ZERO, self.sqrt_price_x96)),
            };
            if den.is_zero() {
                return Ok((U256::ZERO, self.sqrt_price_x96));
            }

            let next_sp = mul_div(lq_q96, self.sqrt_price_x96, den).ok_or_else(|| {
                crate::scythe::errors::V3MathError::overflow(
                    "quote_exact_in_single_tick",
                    "next_sp calculation overflow".to_string(),
                )
            })?;
            let out =
                amount1_delta(next_sp, self.sqrt_price_x96, self.liquidity).ok_or_else(|| {
                    crate::scythe::errors::V3MathError::overflow(
                        "quote_exact_in_single_tick",
                        "amount1_delta overflow".to_string(),
                    )
                })?;

            // Cap output at worst-case price
            let inner = shl96_div(a, next_sp).ok_or_else(|| {
                crate::scythe::errors::V3MathError::overflow(
                    "quote_exact_in_single_tick",
                    "max_output inner overflow".to_string(),
                )
            })?;
            let max_output = shl96_div(inner, next_sp).ok_or_else(|| {
                crate::scythe::errors::V3MathError::overflow(
                    "quote_exact_in_single_tick",
                    "max_output overflow".to_string(),
                )
            })?;
            let capped_out = if out > max_output { max_output } else { out };

            Ok((capped_out, next_sp))
        } else {
            // amount1 in → price UP
            let d_sp = shl96_div(a, self.liquidity).ok_or_else(|| {
                crate::scythe::errors::V3MathError::overflow(
                    "quote_exact_in_single_tick",
                    "d_sp overflow".to_string(),
                )
            })?;

            let next_sp = match self.sqrt_price_x96.checked_add(d_sp) {
                Some(v) => v,
                None => {
                    return Err(crate::scythe::errors::V3MathError::overflow(
                        "quote_exact_in_single_tick",
                        format!(
                            "sqrt_price {} + delta {} exceeds U256::MAX",
                            self.sqrt_price_x96, d_sp
                        ),
                    ));
                }
            };
            let out =
                amount0_delta(self.sqrt_price_x96, next_sp, self.liquidity).ok_or_else(|| {
                    crate::scythe::errors::V3MathError::overflow(
                        "quote_exact_in_single_tick",
                        "amount0_delta overflow".to_string(),
                    )
                })?;

            // Cap output at worst-case price
            let inner = mul_shr96(a, next_sp).ok_or_else(|| {
                crate::scythe::errors::V3MathError::overflow(
                    "quote_exact_in_single_tick",
                    "max_output inner overflow".to_string(),
                )
            })?;
            let max_output = mul_shr96(inner, next_sp).ok_or_else(|| {
                crate::scythe::errors::V3MathError::overflow(
                    "quote_exact_in_single_tick",
                    "max_output overflow".to_string(),
                )
            })?;
            let capped_out = if out > max_output { max_output } else { out };

            Ok((capped_out, next_sp))
        }
    }

    /// Full quote that can cross initialized ticks using `liquidity_net`.
    /// Returns (amount_out, final_sqrtP).
    /// COR-012: Fixed to propagate errors instead of failing open with partial results.
    /// SEC-007: Updated to propagate mul_div overflow errors instead of saturating
    pub fn quote_exact_in_crossing(
        &self,
        mut amount_in: U256,
        zero_for_one: bool,
    ) -> crate::scythe::errors::V3MathResult<(U256, U256)> {
        use crate::scythe::errors::V3MathError;

        if amount_in.is_zero() || self.liquidity.is_zero() {
            return Ok((U256::ZERO, self.sqrt_price_x96));
        }
        let fee_n = self.fee_num()?;
        let fee_d = self.fee_den();

        let mut sqrt_p = self.sqrt_price_x96;
        let mut liq = self.liquidity;
        let mut tick = self.tick;
        let mut out = U256::ZERO;

        loop {
            // Next initialized tick in direction
            let next_tick_opt = if zero_for_one {
                // go to previous initialized tick (< current)
                self.initialized_ticks.range(..tick).next_back().copied()
            } else {
                // go to next initialized tick (> current)
                self.initialized_ticks.range((tick + 1)..).next().copied()
            };

            // If no more initialized ticks, we do single-tick to exhaustion
            let target_sqrt = if let Some(nt) = next_tick_opt {
                // COR-012: Propagate tick errors instead of silently returning partial results
                sqrt_ratio_at_tick(nt)?
            } else {
                // no boundary; just eat remaining within this pseudo-range
                if amount_in.is_zero() {
                    break;
                }
                let a = mul_div(amount_in, fee_n, fee_d).ok_or_else(|| {
                    V3MathError::overflow("quote_exact_in_crossing", "fee mul_div overflow")
                })?;
                let (part_out, new_sp) = if zero_for_one {
                    // amount0 in → price down
                    let lq_q96: U256 = liq << 96;
                    let product = mul_div(a, sqrt_p, U256::from(1)).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "product overflow")
                    })?;
                    let den = lq_q96.checked_add(product).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "denominator overflow")
                    })?;
                    let next_sp = mul_div(lq_q96, sqrt_p, den).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "next_sp overflow")
                    })?;
                    let delta = amount1_delta(next_sp, sqrt_p, liq).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "amount1_delta overflow")
                    })?;
                    (delta, next_sp)
                } else {
                    // amount1 in → price up
                    let d_sp = shl96_div(a, liq).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "d_sp overflow")
                    })?;
                    let next_sp = sqrt_p.checked_add(d_sp).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "next_sp add overflow")
                    })?;
                    let delta = amount0_delta(sqrt_p, next_sp, liq).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "amount0_delta overflow")
                    })?;
                    (delta, next_sp)
                };
                out = out.checked_add(part_out).ok_or_else(|| {
                    V3MathError::overflow("quote_exact_in_crossing", "output accumulation overflow")
                })?;
                sqrt_p = new_sp;
                break;
            };

            // Gross amount needed to reach boundary
            let need = if zero_for_one {
                // token0 in → price down
                let a_no_fee =
                    amount0_delta_round_up(target_sqrt, sqrt_p, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "quote_exact_in_crossing",
                            "amount0_delta_round_up overflow",
                        )
                    })?;
                gross_up(a_no_fee, fee_n, fee_d).ok_or_else(|| {
                    V3MathError::overflow("quote_exact_in_crossing", "gross_up overflow")
                })?
            } else {
                // token1 in → price up
                let a_no_fee =
                    amount1_delta_round_up(sqrt_p, target_sqrt, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "quote_exact_in_crossing",
                            "amount1_delta_round_up overflow",
                        )
                    })?;
                gross_up(a_no_fee, fee_n, fee_d).ok_or_else(|| {
                    V3MathError::overflow("quote_exact_in_crossing", "gross_up overflow")
                })?
            };

            if amount_in < need {
                // Partial swap (not enough to reach boundary)
                let a = mul_div(amount_in, fee_n, fee_d).ok_or_else(|| {
                    V3MathError::overflow("quote_exact_in_crossing", "partial fee mul_div overflow")
                })?;
                let (part_out, new_sp) = if zero_for_one {
                    let lq_q96: U256 = liq << 96;
                    let product = mul_div(a, sqrt_p, U256::from(1)).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "partial product overflow")
                    })?;
                    let den = lq_q96.checked_add(product).ok_or_else(|| {
                        V3MathError::overflow(
                            "quote_exact_in_crossing",
                            "partial denominator overflow",
                        )
                    })?;
                    let next_sp = mul_div(lq_q96, sqrt_p, den).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "partial next_sp overflow")
                    })?;
                    let delta = amount1_delta(next_sp, sqrt_p, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "quote_exact_in_crossing",
                            "partial amount1_delta overflow",
                        )
                    })?;
                    (delta, next_sp)
                } else {
                    let d_sp = shl96_div(a, liq).ok_or_else(|| {
                        V3MathError::overflow("quote_exact_in_crossing", "partial d_sp overflow")
                    })?;
                    let next_sp = sqrt_p.checked_add(d_sp).ok_or_else(|| {
                        V3MathError::overflow(
                            "quote_exact_in_crossing",
                            "partial next_sp add overflow",
                        )
                    })?;
                    let delta = amount0_delta(sqrt_p, next_sp, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "quote_exact_in_crossing",
                            "partial amount0_delta overflow",
                        )
                    })?;
                    (delta, next_sp)
                };
                out = out.checked_add(part_out).ok_or_else(|| {
                    V3MathError::overflow(
                        "quote_exact_in_crossing",
                        "partial output accumulation overflow",
                    )
                })?;
                sqrt_p = new_sp;
                break;
            }

            // Full swap to boundary, then cross tick
            amount_in -= need;
            let part_out = if zero_for_one {
                amount1_delta(target_sqrt, sqrt_p, liq).ok_or_else(|| {
                    V3MathError::overflow(
                        "quote_exact_in_crossing",
                        "boundary amount1_delta overflow",
                    )
                })?
            } else {
                amount0_delta(sqrt_p, target_sqrt, liq).ok_or_else(|| {
                    V3MathError::overflow(
                        "quote_exact_in_crossing",
                        "boundary amount0_delta overflow",
                    )
                })?
            };
            out = out.checked_add(part_out).ok_or_else(|| {
                V3MathError::overflow(
                    "quote_exact_in_crossing",
                    "boundary output accumulation overflow",
                )
            })?;
            sqrt_p = target_sqrt;

            // Cross tick: update liquidity according to direction
            if let Some(nt) = next_tick_opt {
                let ln = *self.liquidity_net.get(&nt).unwrap_or(&0);
                if zero_for_one {
                    // moving left: subtract liquidityNet
                    liq = adj_liquidity(liq, -ln, "quote_exact_in_crossing")?;
                } else {
                    // moving right: add liquidityNet
                    liq = adj_liquidity(liq, ln, "quote_exact_in_crossing")?;
                }
                // update current tick to boundary tick (as in core)
                tick = nt;
                if liq.is_zero() {
                    break;
                }
            } else {
                break;
            }
        }

        Ok((out, sqrt_p))
    }
}

// ------------------- Math helpers -------------------
/// 512-bit precision floor(a×b÷d). Returns None if d is zero or result overflows U256.
///
/// SEC-007: Changed from saturating to Option to prevent silent incorrect outputs.
/// Previously returned U256::MAX on overflow which produced wildly incorrect swap amounts.
/// COR-023: Made public for use in optimal_sizing V3 math
#[inline(always)]
pub fn mul_div(a: U256, b: U256, d: U256) -> Option<U256> {
    if d.is_zero() {
        return None;
    }

    // Use 512-bit precision for intermediate multiplication
    // This matches Uniswap V3's FullMath.sol which uses assembly for 512-bit math
    type U512 = ruint::Uint<512, 8>;

    let a_wide = U512::from(a);
    let b_wide = U512::from(b);
    let d_wide = U512::from(d);

    let product = a_wide * b_wide;
    let result = product / d_wide;

    // Check if result exceeds U256::MAX - return None instead of saturating
    if result > U512::from(U256::MAX) {
        None
    } else {
        // Extract lower 256 bits
        Some(U256::from_limbs(
            result.as_limbs()[..4].try_into().expect("U512 has 8 limbs"),
        ))
    }
}

/// 512-bit precision ceil(a×b÷d). Returns None if d is zero or result overflows U256.
///
/// SEC-007: Changed from saturating to Option to prevent silent incorrect outputs.
#[inline(always)]
fn mul_div_up(a: U256, b: U256, d: U256) -> Option<U256> {
    if d.is_zero() {
        return None;
    }

    // Use 512-bit precision for intermediate multiplication
    type U512 = ruint::Uint<512, 8>;

    let a_wide = U512::from(a);
    let b_wide = U512::from(b);
    let d_wide = U512::from(d);

    let product = a_wide * b_wide;
    let quotient = product / d_wide;
    let remainder = product - (quotient * d_wide);

    // Round up if there's a remainder
    let result = if remainder > U512::ZERO {
        quotient + U512::from(1u64)
    } else {
        quotient
    };

    // Check if result exceeds U256::MAX - return None instead of saturating
    if result > U512::from(U256::MAX) {
        None
    } else {
        // Extract lower 256 bits
        Some(U256::from_limbs(
            result.as_limbs()[..4].try_into().expect("U512 has 8 limbs"),
        ))
    }
}
/// SEC-007: Returns Option to propagate mul_div_up overflow errors
#[inline(always)]
fn gross_up(amount_no_fee: U256, fee_n: U256, fee_d: U256) -> Option<U256> {
    // amount_gross = ceil(amount_no_fee * fee_d / fee_n)
    mul_div_up(amount_no_fee, fee_d, fee_n)
}

/// Adjusts liquidity by a signed delta, with proper error propagation.
/// COR-013: Changed from saturating arithmetic to checked + error propagation.
#[inline(always)]
pub fn adj_liquidity(
    current: U256,
    delta_net: i128,
    operation: &'static str,
) -> crate::scythe::errors::V3MathResult<U256> {
    use crate::scythe::errors::V3MathError;

    if delta_net == 0 {
        return Ok(current);
    }

    if delta_net > 0 {
        let delta = U256::from(delta_net as u128);
        current
            .checked_add(delta)
            .ok_or(V3MathError::LiquidityOverflow {
                operation,
                amount: delta_net as u128,
            })
    } else {
        let delta = U256::from((-delta_net) as u128);
        current.checked_sub(delta).ok_or_else(|| {
            let current_display = current.saturating_to::<u128>();
            V3MathError::LiquidityUnderflow {
                operation,
                current: current_display,
                delta: (-delta_net) as u128,
            }
        })
    }
}

// Amount deltas (rounding down)
/// SEC-007: Returns Option to propagate mul_div overflow errors
#[inline(always)]
pub fn amount0_delta(sa: U256, sb: U256, liq: U256) -> Option<U256> {
    let (sa, sb) = if sa > sb { (sb, sa) } else { (sa, sb) };
    if sa.is_zero() || sb.is_zero() {
        return Some(U256::ZERO);
    }

    let numerator1 = checked_shl96(liq)?;
    let numerator2 = sb - sa;
    let intermediate = mul_div(numerator1, numerator2, sb)?;
    Some(intermediate / sa)
}

/// SEC-007: Returns Option to propagate mul_div overflow errors
#[inline(always)]
pub fn amount1_delta(sa: U256, sb: U256, liq: U256) -> Option<U256> {
    let (sa, sb) = if sa > sb { (sb, sa) } else { (sa, sb) };
    mul_shr96(liq, sb - sa)
}
// Rounding-UP variants for exact-in boundary calculations
/// SEC-007: Returns Option to propagate mul_div overflow errors
#[inline(always)]
fn amount0_delta_round_up(sa: U256, sb: U256, liq: U256) -> Option<U256> {
    let (sa, sb) = if sa > sb { (sb, sa) } else { (sa, sb) };
    let delta = sb - sa;
    let step1 = mul_div(liq, delta, U256::from(1))?;
    let numerator = checked_shl96(step1)?;
    let denominator = mul_div(sb, sa, U256::from(1))?;
    mul_div_up(numerator, U256::from(1), denominator)
}
/// SEC-007: Returns Option to propagate mul_div overflow errors
#[inline(always)]
fn amount1_delta_round_up(sa: U256, sb: U256, liq: U256) -> Option<U256> {
    let (sa, sb) = if sa > sb { (sb, sa) } else { (sa, sb) };
    mul_shr96_up(liq, sb - sa)
}

// ============================================================================
// Q96-specialized math helpers (shift-based, no U512 division)
// ============================================================================

/// floor(a × b / 2^96) via right-shift. Eliminates U512 division.
#[inline(always)]
fn mul_shr96(a: U256, b: U256) -> Option<U256> {
    let result: U512 = (U512::from(a) * U512::from(b)) >> 96;
    if result > U512::from(U256::MAX) {
        None
    } else {
        Some(U256::from_limbs(
            result.as_limbs()[..4].try_into().expect("U512 has 8 limbs"),
        ))
    }
}

/// ceil(a × b / 2^96) via right-shift with rounding. Eliminates U512 division.
#[inline(always)]
fn mul_shr96_up(a: U256, b: U256) -> Option<U256> {
    let product: U512 = U512::from(a) * U512::from(b);
    let mask_96: U512 = (U512::from(1u64) << 96) - U512::from(1u64);
    let has_remainder = (product & mask_96) > U512::ZERO;
    let mut result: U512 = product >> 96;
    if has_remainder {
        result += U512::from(1u64);
    }
    if result > U512::from(U256::MAX) {
        None
    } else {
        Some(U256::from_limbs(
            result.as_limbs()[..4].try_into().expect("U512 has 8 limbs"),
        ))
    }
}

/// floor((a << 96) / b). Replaces mul_div(a, q96(), b) — shift instead of U512 multiply.
#[inline(always)]
fn shl96_div(a: U256, b: U256) -> Option<U256> {
    if b.is_zero() {
        return None;
    }
    let shifted: U512 = U512::from(a) << 96;
    let result: U512 = shifted / U512::from(b);
    if result > U512::from(U256::MAX) {
        None
    } else {
        Some(U256::from_limbs(
            result.as_limbs()[..4].try_into().expect("U512 has 8 limbs"),
        ))
    }
}

/// a × 2^96 with overflow check. Returns None if result exceeds U256.
#[inline(always)]
fn checked_shl96(a: U256) -> Option<U256> {
    if (a >> 160) != U256::ZERO {
        return None;
    }
    Some(a << 96)
}

/// Safely converts U256 to u128, detecting malicious pools.
#[inline]
fn u256_to_u128_checked(
    value: U256,
    operation: &'static str,
) -> crate::scythe::errors::V3MathResult<u128> {
    let lo = value.as_limbs()[0];
    let hi = value.as_limbs()[1];
    let h2 = value.as_limbs()[2];
    let h3 = value.as_limbs()[3];
    if h2 != 0 || h3 != 0 {
        return Err(crate::scythe::errors::V3MathError::overflow(
            operation,
            format!("Amount {} exceeds u128::MAX (pool likely malicious)", value),
        ));
    }
    Ok(((hi as u128) << 64) | (lo as u128))
}

// Uniswap V3 TickMath.getSqrtRatioAtTick implementation

#[allow(clippy::unreadable_literal)]
pub fn sqrt_ratio_at_tick(tick: i32) -> crate::scythe::errors::V3MathResult<U256> {
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return Err(crate::scythe::errors::V3MathError::TickOutOfBounds { tick });
    }

    // M4 OPTIMIZATION: Check thread-local cache first (avoids ~200-400ns of U512 math)
    let cache_idx = tick_cache_index(tick);
    let cached = TICK_SQRT_CACHE.with(|cache| {
        let cache = cache.borrow();
        let (cached_tick, cached_sqrt) = cache[cache_idx];
        if cached_tick == tick {
            Some(cached_sqrt)
        } else {
            None
        }
    });

    if let Some(sqrt_price) = cached {
        return Ok(sqrt_price);
    }

    // Cache miss - compute sqrt_price_x96
    let abs_tick = if tick < 0 {
        (-tick) as u32
    } else {
        tick as u32
    };

    // U512 intermediate calculations to prevent overflow
    let ratio_base = if (abs_tick & 0x1) != 0 {
        *TICK_RATIO_BASE_ODD
    } else {
        *TICK_RATIO_BASE_EVEN
    };

    let mut ratio_wide = U512::from(ratio_base);

    // Apply bit-specific multipliers using U512 to prevent overflow during sequential multiplications
    if (abs_tick & 0x2) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X2)) >> 128;
    }
    if (abs_tick & 0x4) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X4)) >> 128;
    }
    if (abs_tick & 0x8) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X8)) >> 128;
    }
    if (abs_tick & 0x10) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X10)) >> 128;
    }
    if (abs_tick & 0x20) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X20)) >> 128;
    }
    if (abs_tick & 0x40) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X40)) >> 128;
    }
    if (abs_tick & 0x80) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X80)) >> 128;
    }
    if (abs_tick & 0x100) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X100)) >> 128;
    }
    if (abs_tick & 0x200) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X200)) >> 128;
    }
    if (abs_tick & 0x400) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X400)) >> 128;
    }
    if (abs_tick & 0x800) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X800)) >> 128;
    }
    if (abs_tick & 0x1000) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X1000)) >> 128;
    }
    if (abs_tick & 0x2000) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X2000)) >> 128;
    }
    if (abs_tick & 0x4000) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X4000)) >> 128;
    }
    if (abs_tick & 0x8000) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X8000)) >> 128;
    }
    if (abs_tick & 0x10000) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X10000)) >> 128;
    }
    if (abs_tick & 0x20000) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X20000)) >> 128;
    }
    if (abs_tick & 0x40000) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X40000)) >> 128;
    }
    if (abs_tick & 0x80000) != 0 {
        ratio_wide = (ratio_wide * U512::from(*TICK_RATIO_0X80000)) >> 128;
    }

    // Convert back to U256 with overflow check (should never overflow given valid tick range)
    // Note: alloy_primitives U512->U256 doesn't have TryFrom, so we convert manually via limbs
    let limbs = ratio_wide.into_limbs();
    // Check that high 4 limbs are zero (otherwise overflow)
    if limbs[4..].iter().any(|&x| x != 0) {
        return Err(crate::scythe::errors::V3MathError::overflow(
            "sqrt_ratio_at_tick",
            format!("Tick ratio calculation overflowed U256 for tick={}", tick),
        ));
    }
    let mut ratio = U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]);

    if tick > 0 {
        // Add division by zero check for defense-in-depth
        if ratio.is_zero() {
            return Err(crate::scythe::errors::V3MathError::div_by_zero(
                "sqrt_ratio_at_tick inversion",
            ));
        }
        // invert ratio (matches Uniswap v3-core TickMath: type(uint256).max / ratio)
        ratio = U256::MAX / ratio;
    }

    // Downshift 32 to get Q96 from Q128, then round up if any remainder exists
    // (matches Uniswap v3 TickMath.getSqrtRatioAtTick behavior).
    let mut sqrt_price_x96 = ratio >> 32;
    let round_mask = (U256::from(1u64) << 32) - U256::from(1u8);
    if (ratio & round_mask) != U256::ZERO {
        sqrt_price_x96 += U256::from(1u8);
    }

    // Store result in cache for future lookups
    TICK_SQRT_CACHE.with(|cache| {
        cache.borrow_mut()[cache_idx] = (tick, sqrt_price_x96);
    });

    Ok(sqrt_price_x96)
}

/// Integer-exact inverse of `sqrt_ratio_at_tick()`.
///
/// Returns the largest tick `t` such that `sqrt_ratio_at_tick(t) <= sqrt_price_x96`.
/// Matches Uniswap V3 TickMath.getTickAtSqrtRatio floor semantics.
///
/// Uses binary search over the monotonically increasing `sqrt_ratio_at_tick()`.
/// Cost: ~21 iterations (worst case ~6us with cache).
///
/// # Errors
/// Returns `InvalidSqrtPrice` if input is outside `[MIN_SQRT_RATIO, MAX_SQRT_RATIO]`.
pub fn tick_at_sqrt_ratio(sqrt_price_x96: U256) -> crate::scythe::errors::V3MathResult<i32> {
    if sqrt_price_x96 < *MIN_SQRT_RATIO || sqrt_price_x96 > *MAX_SQRT_RATIO {
        return Err(crate::scythe::errors::V3MathError::InvalidSqrtPrice {
            value: sqrt_price_x96,
        });
    }

    // Binary search: find largest tick where sqrt_ratio_at_tick(tick) <= sqrt_price_x96
    let mut lo = MIN_TICK;
    let mut hi = MAX_TICK;

    while lo < hi {
        // Ceiling midpoint for "rightmost satisfying" binary search pattern
        let mid = lo + (hi - lo + 1) / 2;
        let sqrt_at_mid = sqrt_ratio_at_tick(mid)?;

        if sqrt_at_mid <= sqrt_price_x96 {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }

    Ok(lo)
}

/// Single-tick V3 swap calculation (fallback when initialized tick data is unavailable).
/// Returns (new_sqrt_price, amount_out).
/// For production accuracy, prefer `get_amount_out_with_ticks()` which crosses initialized ticks.
/// SEC-007: Updated to propagate mul_div overflow errors instead of saturating
pub fn get_amount_out(
    amount_in: u128,
    sqrt_price_x96: &U256,
    liquidity: &U256,
    fee_ppm: u32,
    zero_for_one: bool,
) -> crate::scythe::errors::V3MathResult<(U256, u128)> {
    if amount_in == 0 || liquidity.is_zero() {
        return Ok((*sqrt_price_x96, 0));
    }

    // V3 fee in ppm (e.g., 500 = 0.05%, 3000 = 0.3%)
    let remaining = 1_000_000u32
        .checked_sub(fee_ppm)
        .ok_or(crate::scythe::errors::V3MathError::InvalidFeeTier { fee_ppm })?;
    let fee_num = U256::from(remaining);
    let fee_den = U256::from(1_000_000u32);

    let amount_in_u256 = U256::from(amount_in);
    let amount_after_fee = mul_div(amount_in_u256, fee_num, fee_den).ok_or_else(|| {
        crate::scythe::errors::V3MathError::overflow(
            "get_amount_out",
            "fee calculation overflow".to_string(),
        )
    })?;

    if zero_for_one {
        // amount0 in → price down
        let lq_q96: U256 = *liquidity << 96;
        let product =
            mul_div(amount_after_fee, *sqrt_price_x96, U256::from(1)).ok_or_else(|| {
                crate::scythe::errors::V3MathError::overflow(
                    "get_amount_out",
                    "product overflow".to_string(),
                )
            })?;
        let den = match lq_q96.checked_add(product) {
            Some(v) => v,
            None => return Ok((*sqrt_price_x96, 0)),
        };
        let next_sp = mul_div(lq_q96, *sqrt_price_x96, den).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "get_amount_out",
                "next_sp overflow".to_string(),
            )
        })?;
        let out = amount1_delta(next_sp, *sqrt_price_x96, *liquidity).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "get_amount_out",
                "amount1_delta overflow".to_string(),
            )
        })?;

        // Cap output at worst-case price limit
        let inner = shl96_div(amount_after_fee, next_sp).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "get_amount_out",
                "max_output inner overflow".to_string(),
            )
        })?;
        let max_output = shl96_div(inner, next_sp).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "get_amount_out",
                "max_output overflow".to_string(),
            )
        })?;
        let capped_out = if out > max_output { max_output } else { out };

        let amount_out = u256_to_u128_checked(capped_out, "get_amount_out")?;
        Ok((next_sp, amount_out))
    } else {
        // amount1 in → price up
        let d_sp = shl96_div(amount_after_fee, *liquidity).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "get_amount_out",
                "d_sp overflow".to_string(),
            )
        })?;
        let next_sp = match sqrt_price_x96.checked_add(d_sp) {
            Some(v) => v,
            None => {
                return Err(crate::scythe::errors::V3MathError::overflow(
                    "get_amount_out",
                    format!(
                        "sqrt_price {} + delta {} exceeds U256::MAX",
                        sqrt_price_x96, d_sp
                    ),
                ));
            }
        };
        let out = amount0_delta(*sqrt_price_x96, next_sp, *liquidity).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "get_amount_out",
                "amount0_delta overflow".to_string(),
            )
        })?;

        // Cap output at worst-case price limit
        let inner = mul_shr96(amount_after_fee, next_sp).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "get_amount_out",
                "max_output inner overflow".to_string(),
            )
        })?;
        let max_output = mul_shr96(inner, next_sp).ok_or_else(|| {
            crate::scythe::errors::V3MathError::overflow(
                "get_amount_out",
                "max_output overflow".to_string(),
            )
        })?;
        let capped_out = if out > max_output { max_output } else { out };

        let amount_out = u256_to_u128_checked(capped_out, "get_amount_out")?;
        Ok((next_sp, amount_out))
    }
}

/// Multi-tick swap simulation using cached tick data.
/// Returns (amount_out, final_sqrt_price).
/// COR-012: Fixed to propagate errors instead of failing open with partial results.
/// SEC-007: Updated to propagate mul_div overflow errors instead of saturating
#[allow(clippy::too_many_arguments)]
pub fn get_amount_out_with_ticks(
    amount_in: u128,
    sqrt_price_x96: U256,
    liquidity: U256,
    current_tick: i32,
    fee_ppm: u32,
    zero_for_one: bool,
    initialized_ticks: &[i32],
    liquidity_net: &ahash::AHashMap<i32, i128>,
) -> crate::scythe::errors::V3MathResult<(u128, U256)> {
    use crate::scythe::errors::V3MathError;

    if amount_in == 0 || liquidity.is_zero() {
        return Ok((0, sqrt_price_x96));
    }

    // V3 fee in ppm (e.g., 500 = 0.05%, 3000 = 0.3%)
    let remaining = 1_000_000u32
        .checked_sub(fee_ppm)
        .ok_or(crate::scythe::errors::V3MathError::InvalidFeeTier { fee_ppm })?;
    let fee_num = U256::from(remaining);
    let fee_den = U256::from(1_000_000u32);

    let mut amount_remaining = U256::from(amount_in);
    let mut sqrt_p = sqrt_price_x96;
    let mut liq = liquidity;
    let tick = current_tick;
    let mut amount_out = U256::ZERO;

    // PERF-005: Binary search + cursor optimization for O(log n) initial + O(1) per crossing
    // Instead of O(n) linear scan on every tick crossing, we:
    // 1. Use partition_point() for O(log n) binary search to find initial position
    // 2. Track cursor index and move O(1) per tick crossing
    let tick_len = initialized_ticks.len();

    // Initialize cursor using binary search
    // For zero_for_one: we want ticks < current, so cursor points to first tick >= current
    // For !zero_for_one: we want ticks > current, so cursor points to first tick > current
    let mut cursor_idx: usize = if zero_for_one {
        // partition_point: finds first index where predicate is false
        // Predicate: t < tick → finds first tick >= tick
        initialized_ticks.partition_point(|&t| t < tick)
    } else {
        // Predicate: t <= tick → finds first tick > tick
        initialized_ticks.partition_point(|&t| t <= tick)
    };

    loop {
        if amount_remaining.is_zero() {
            break;
        }

        // PERF-005: O(1) cursor-based tick lookup (replaces O(n) linear scan)
        let next_tick_opt: Option<i32> = if zero_for_one {
            // Moving left: need tick at cursor_idx - 1 (largest tick < current)
            if cursor_idx > 0 {
                Some(initialized_ticks[cursor_idx - 1])
            } else {
                None
            }
        } else {
            // Moving right: need tick at cursor_idx (smallest tick > current)
            if cursor_idx < tick_len {
                Some(initialized_ticks[cursor_idx])
            } else {
                None
            }
        };

        if let Some(next_tick) = next_tick_opt {
            // COR-012: Propagate tick errors instead of silently returning partial results
            let target_sqrt = sqrt_ratio_at_tick(next_tick)?;

            // Amount (with fee) needed to reach boundary
            let amount_needed = if zero_for_one {
                let a_no_fee =
                    amount0_delta_round_up(target_sqrt, sqrt_p, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "amount0_delta_round_up overflow",
                        )
                    })?;
                gross_up(a_no_fee, fee_num, fee_den).ok_or_else(|| {
                    V3MathError::overflow("get_amount_out_with_ticks", "gross_up overflow")
                })?
            } else {
                let a_no_fee =
                    amount1_delta_round_up(sqrt_p, target_sqrt, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "amount1_delta_round_up overflow",
                        )
                    })?;
                gross_up(a_no_fee, fee_num, fee_den).ok_or_else(|| {
                    V3MathError::overflow("get_amount_out_with_ticks", "gross_up overflow")
                })?
            };

            if amount_remaining >= amount_needed {
                // We have enough to reach the boundary
                amount_remaining -= amount_needed;

                let part_out = if zero_for_one {
                    amount1_delta(target_sqrt, sqrt_p, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "boundary amount1_delta overflow",
                        )
                    })?
                } else {
                    amount0_delta(sqrt_p, target_sqrt, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "boundary amount0_delta overflow",
                        )
                    })?
                };
                amount_out = amount_out.checked_add(part_out).ok_or_else(|| {
                    V3MathError::overflow(
                        "get_amount_out_with_ticks",
                        "boundary output accumulation overflow",
                    )
                })?;
                sqrt_p = target_sqrt;

                // Cross tick: update liquidity
                if let Some(&liq_net) = liquidity_net.get(&next_tick) {
                    if zero_for_one {
                        // Moving left: subtract liquidityNet
                        liq = adj_liquidity(liq, -liq_net, "get_amount_out_with_ticks")?;
                    } else {
                        // Moving right: add liquidityNet
                        liq = adj_liquidity(liq, liq_net, "get_amount_out_with_ticks")?;
                    }
                }

                // tick = next_tick; // Dead store: tick is not read again in this loop

                // PERF-005: Move cursor after crossing tick
                if zero_for_one {
                    // Crossed left, decrement cursor
                    cursor_idx -= 1;
                } else {
                    // Crossed right, increment cursor
                    cursor_idx += 1;
                }

                if liq.is_zero() {
                    break; // No more liquidity
                }
            } else {
                // Partial swap within current range
                let a = mul_div(amount_remaining, fee_num, fee_den).ok_or_else(|| {
                    V3MathError::overflow(
                        "get_amount_out_with_ticks",
                        "partial fee mul_div overflow",
                    )
                })?;
                let (part_out, new_sqrt) = if zero_for_one {
                    let lq_q96: U256 = liq << 96;
                    let product = mul_div(a, sqrt_p, U256::from(1)).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "partial product overflow",
                        )
                    })?;
                    let den = lq_q96.checked_add(product).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "partial denominator overflow",
                        )
                    })?;
                    let next_sp = mul_div(lq_q96, sqrt_p, den).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "partial next_sp overflow",
                        )
                    })?;
                    let delta = amount1_delta(next_sp, sqrt_p, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "partial amount1_delta overflow",
                        )
                    })?;
                    (delta, next_sp)
                } else {
                    let d_sp = shl96_div(a, liq).ok_or_else(|| {
                        V3MathError::overflow("get_amount_out_with_ticks", "partial d_sp overflow")
                    })?;
                    let next_sp = sqrt_p.checked_add(d_sp).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "partial next_sp add overflow",
                        )
                    })?;
                    let delta = amount0_delta(sqrt_p, next_sp, liq).ok_or_else(|| {
                        V3MathError::overflow(
                            "get_amount_out_with_ticks",
                            "partial amount0_delta overflow",
                        )
                    })?;
                    (delta, next_sp)
                };

                amount_out = amount_out.checked_add(part_out).ok_or_else(|| {
                    V3MathError::overflow(
                        "get_amount_out_with_ticks",
                        "partial output accumulation overflow",
                    )
                })?;
                sqrt_p = new_sqrt;
                break;
            }
        } else {
            // No more initialized ticks, consume remaining amount
            let a = mul_div(amount_remaining, fee_num, fee_den).ok_or_else(|| {
                V3MathError::overflow("get_amount_out_with_ticks", "no-ticks fee mul_div overflow")
            })?;
            let (part_out, new_sqrt) = if zero_for_one {
                let lq_q96: U256 = liq << 96;
                let product = mul_div(a, sqrt_p, U256::from(1)).ok_or_else(|| {
                    V3MathError::overflow("get_amount_out_with_ticks", "no-ticks product overflow")
                })?;
                let den = lq_q96.checked_add(product).ok_or_else(|| {
                    V3MathError::overflow(
                        "get_amount_out_with_ticks",
                        "no-ticks denominator overflow",
                    )
                })?;
                let next_sp = mul_div(lq_q96, sqrt_p, den).ok_or_else(|| {
                    V3MathError::overflow("get_amount_out_with_ticks", "no-ticks next_sp overflow")
                })?;
                let delta = amount1_delta(next_sp, sqrt_p, liq).ok_or_else(|| {
                    V3MathError::overflow(
                        "get_amount_out_with_ticks",
                        "no-ticks amount1_delta overflow",
                    )
                })?;
                (delta, next_sp)
            } else {
                let d_sp = shl96_div(a, liq).ok_or_else(|| {
                    V3MathError::overflow("get_amount_out_with_ticks", "no-ticks d_sp overflow")
                })?;
                let next_sp = sqrt_p.checked_add(d_sp).ok_or_else(|| {
                    V3MathError::overflow(
                        "get_amount_out_with_ticks",
                        "no-ticks next_sp add overflow",
                    )
                })?;
                let delta = amount0_delta(sqrt_p, next_sp, liq).ok_or_else(|| {
                    V3MathError::overflow(
                        "get_amount_out_with_ticks",
                        "no-ticks amount0_delta overflow",
                    )
                })?;
                (delta, next_sp)
            };

            amount_out = amount_out.checked_add(part_out).ok_or_else(|| {
                V3MathError::overflow(
                    "get_amount_out_with_ticks",
                    "no-ticks output accumulation overflow",
                )
            })?;
            sqrt_p = new_sqrt;
            break;
        }
    }

    let final_amount_out = u256_to_u128_checked(amount_out, "get_amount_out_with_ticks")?;
    Ok((final_amount_out, sqrt_p))
}

/// Compatibility type for price.rs (matches old ethers-based implementation)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniswapV3Slot0 {
    pub sqrt_p_x96: U256,
    pub liquidity: U256,
}

/// Extract fee from 3-byte path segment (compatibility for trade_simulator.rs)
#[inline(always)]
pub fn fee_from_path_bytes(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]])
}

#[cfg(test)]
mod tick_at_sqrt_ratio_tests {
    use super::*;

    #[test]
    fn test_min_max_sqrt_ratio_constants_match_tick_math() {
        assert_eq!(*MIN_SQRT_RATIO, sqrt_ratio_at_tick(MIN_TICK).unwrap());
        assert_eq!(*MAX_SQRT_RATIO, sqrt_ratio_at_tick(MAX_TICK).unwrap());
    }

    #[test]
    fn test_round_trip_known_ticks() {
        let test_ticks = [
            MIN_TICK,
            MIN_TICK + 1,
            -887_000,
            -100_000,
            -10_000,
            -1000,
            -100,
            -10,
            -1,
            0,
            1,
            10,
            100,
            1000,
            10_000,
            100_000,
            887_000,
            MAX_TICK - 1,
            MAX_TICK,
        ];
        for &tick in &test_ticks {
            let sqrt_price = sqrt_ratio_at_tick(tick).unwrap();
            let recovered = tick_at_sqrt_ratio(sqrt_price).unwrap();
            assert_eq!(recovered, tick, "Round-trip failed for tick {}", tick);
        }
    }

    #[test]
    fn test_between_ticks_floor_semantics() {
        // Price between tick 1000 and 1001 should return 1000 (floor semantics)
        let sqrt_at_tick = sqrt_ratio_at_tick(1000).unwrap();
        let sqrt_at_next = sqrt_ratio_at_tick(1001).unwrap();
        let midpoint = (sqrt_at_tick + sqrt_at_next) / U256::from(2u64);
        assert_eq!(tick_at_sqrt_ratio(midpoint).unwrap(), 1000);
        assert_eq!(
            tick_at_sqrt_ratio(sqrt_at_tick + U256::from(1u64)).unwrap(),
            1000
        );
        assert_eq!(
            tick_at_sqrt_ratio(sqrt_at_next - U256::from(1u64)).unwrap(),
            1000
        );
    }

    #[test]
    fn test_boundary_values() {
        assert_eq!(tick_at_sqrt_ratio(*MIN_SQRT_RATIO).unwrap(), MIN_TICK);
        assert_eq!(tick_at_sqrt_ratio(*MAX_SQRT_RATIO).unwrap(), MAX_TICK);
    }

    #[test]
    fn test_rejects_out_of_range() {
        assert!(matches!(
            tick_at_sqrt_ratio(*MIN_SQRT_RATIO - U256::from(1u64)),
            Err(crate::scythe::errors::V3MathError::InvalidSqrtPrice { .. })
        ));
        assert!(matches!(
            tick_at_sqrt_ratio(*MAX_SQRT_RATIO + U256::from(1u64)),
            Err(crate::scythe::errors::V3MathError::InvalidSqrtPrice { .. })
        ));
        assert!(matches!(
            tick_at_sqrt_ratio(U256::ZERO),
            Err(crate::scythe::errors::V3MathError::InvalidSqrtPrice { .. })
        ));
    }

    #[test]
    fn test_official_vectors() {
        assert_eq!(
            tick_at_sqrt_ratio(U256::from(79_228_162_514_264_337_593_543_950_336u128)).unwrap(),
            0
        );
        assert_eq!(
            tick_at_sqrt_ratio(U256::from(79_232_123_823_359_799_118_286_999_568u128)).unwrap(),
            1
        );
        assert_eq!(
            tick_at_sqrt_ratio(U256::from(79_224_201_403_219_477_170_569_942_574u128)).unwrap(),
            -1
        );
    }

    #[test]
    fn test_exhaustive_near_zero() {
        for tick in -500..=500 {
            let sqrt_price = sqrt_ratio_at_tick(tick).unwrap();
            assert_eq!(
                tick_at_sqrt_ratio(sqrt_price).unwrap(),
                tick,
                "Failed at tick {}",
                tick
            );
        }
    }
}
