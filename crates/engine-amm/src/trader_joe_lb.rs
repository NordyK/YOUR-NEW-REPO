//! Trader Joe Liquidity Book V2.1 — bin-based AMM math
//!
//! LB pools use discrete price bins where each bin is a constant-sum market:
//!   L = p * x + y  (within a single bin, zero price impact)
//!
//! Price: price(id) = (1 + binStep/10_000)^(id - 8_388_608) in 128.128 fixed-point.
//!
//! Fee model: totalFee = baseFee + variableFee (in 1e18 precision, capped at 10%).

// The crate sets `#![deny(missing_docs)]` at the root. The ported LB structs
// expose many self-describing public reserve/fee fields that are documented in
// aggregate via the struct-level docs above each definition; allow the field
// granularity here so the bit-exact port stays a mechanical copy.
#![allow(missing_docs)]

use alloy_primitives::U256;
use smallvec::SmallVec;
use std::{collections::BTreeMap, sync::Arc};

/// Checked U256 → u128 conversion: returns `None` if the value does not fit.
///
/// Mirrors the `crate::numeric::u256_to_u128_checked` helper from the source
/// crate; kept local here so the swap math stays bit-for-bit identical.
#[inline]
fn u256_to_u128_checked(x: U256) -> Option<u128> {
    if x.bit_len() > 128 {
        None
    } else {
        let limbs = x.as_limbs();
        Some((limbs[0] as u128) | ((limbs[1] as u128) << 64))
    }
}

/// Maximum bins to traverse during simulation.
/// Must match the snapshot window in snapshot_loader.rs (±50 bins).
pub const MAX_TRAVERSAL_BINS: u32 = 50;

/// Maximum empty/missing bins to skip before giving up.
/// Prevents exhausting the traversal budget on sparse distributions.
const MAX_GAP_BINS: u32 = 200;

/// Wider integer type for intermediate multiplication.
type U512 = alloy_primitives::U512;

/// Reserves for a single bin
#[derive(Clone, Debug, Default)]
pub struct BinData {
    pub reserve_x: u128,
    pub reserve_y: u128,
}

/// State for a Trader Joe Liquidity Book V2.1/V2.2 pool.
/// Tracks the active bin's reserves, fee parameters, and neighboring bins.
#[derive(Clone, Debug)]
pub struct LBState {
    pub active_id: u32,
    pub bin_step: u16,
    pub reserve_x: u128,
    pub reserve_y: u128,
    // Fee parameters (from getStaticFeeParameters)
    pub base_factor: u16,
    pub filter_period: u16,
    pub decay_period: u16,
    pub reduction_factor: u16,
    pub variable_fee_control: u32,
    pub protocol_share: u16,
    pub max_volatility_accumulator: u32,
    // Volatile state (from last Swap event / getVariableFeeParameters)
    pub volatility_accumulator: u32,
    pub volatility_reference: u32,
    pub id_reference: u32,
    pub time_of_last_update: u64,
    // Metadata
    pub block_number: u64,
    pub log_index: u32,
    /// Set when cross-bin swap detected and no neighbor bin data; cleared on re-fetch
    pub stale: bool,
    /// Neighboring bin data: bin_id → reserves (window around active_id)
    /// Arc-wrapped to make clones cheap (~8 bytes vs deep BTreeMap copy)
    pub bin_map: Arc<BTreeMap<u32, BinData>>,
}

// =============================================================================
// 128.128 Fixed-Point Arithmetic
// =============================================================================

/// SCALE = 1 << 128 (= 1.0 in 128.128 fixed-point)
const SCALE: U256 = U256::from_limbs([0, 0, 1, 0]); // 2^128

/// Uncached bin price in 128.128 fixed-point (the canonical pow).
/// price = (1 + binStep/10_000) ^ (id - 8_388_608)
pub fn get_price_from_id_uncached(id: u32, bin_step: u16) -> U256 {
    let base = SCALE + (U256::from(bin_step as u64) << 128) / U256::from(10_000u64);
    let exponent = (id as i64) - 8_388_608i64;
    pow_128x128(base, exponent)
}

/// T2.2: bounded direct-mapped memo for bin prices. 4096 slots × (8B key + 32B U256) ≈ 160 KB
/// per sim thread (fits L2). Key `u64::MAX` is the empty sentinel (never a real `(id<<16)|bs`).
const LB_PRICE_CACHE_BITS: u32 = 12;
const LB_PRICE_CACHE_SIZE: usize = 1 << LB_PRICE_CACHE_BITS;

thread_local! {
    static LB_PRICE_CACHE: std::cell::RefCell<Vec<(u64, U256)>> =
        std::cell::RefCell::new(vec![(u64::MAX, U256::ZERO); LB_PRICE_CACHE_SIZE]);
}

#[inline]
fn lb_price_key(id: u32, bin_step: u16) -> u64 {
    ((id as u64) << 16) | (bin_step as u64)
}

/// Bin price in 128.128 fixed-point, **memoized** per `(id, bin_step)`. Byte-identical to
/// [`get_price_from_id_uncached`] (the cache stores the exact pow result — proptest-guarded), so
/// it stays fast-lane-safe. During the sizing search the same bins are re-walked many times, so
/// the memo replaces ~24 `U512` square-and-multiply muls with one thread-local lookup. Bounded
/// direct-mapped: a collision simply overwrites (always correct — the stored key is verified).
pub fn get_price_from_id(id: u32, bin_step: u16) -> U256 {
    let key = lb_price_key(id, bin_step);
    let slot = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - LB_PRICE_CACHE_BITS)) as usize;
    LB_PRICE_CACHE.with(|c| {
        {
            let cache = c.borrow();
            let (k, v) = cache[slot];
            if k == key {
                return v;
            }
        }
        let price = get_price_from_id_uncached(id, bin_step);
        c.borrow_mut()[slot] = (key, price);
        price
    })
}

/// Binary exponentiation for 128.128 fixed-point.
/// Handles both positive and negative exponents.
fn pow_128x128(base: U256, exp: i64) -> U256 {
    if exp == 0 {
        return SCALE;
    }
    let (positive_exp, invert) = if exp < 0 {
        ((-exp) as u64, true)
    } else {
        (exp as u64, false)
    };
    // Square-and-multiply
    let mut result = SCALE;
    let mut b = base;
    let mut e = positive_exp;
    while e > 0 {
        if e & 1 == 1 {
            result = mul_shift_round_down(result, b);
        }
        b = mul_shift_round_down(b, b);
        e >>= 1;
    }
    if invert {
        // Invert: SCALE^2 / result using U512 (SCALE*SCALE = 2^256 overflows U256)
        let wide: U512 = SCALE.widening_mul(SCALE);
        let inv = wide / U512::from(result);
        let limbs = inv.into_limbs();
        U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]])
    } else {
        result
    }
}

/// Multiply two 128.128 numbers, right-shift 128, round down.
fn mul_shift_round_down(a: U256, b: U256) -> U256 {
    let wide: U512 = a.widening_mul(b);
    let shifted: U512 = wide >> 128;
    let limbs: [u64; 8] = shifted.into_limbs();
    U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]])
}

fn mul_shift_round_up(value: U256, price: U256) -> U256 {
    let wide: U512 = value.widening_mul(price);
    let shifted: U512 = wide >> 128;
    let mask: U512 = (U512::from(1) << 128) - U512::from(1);
    let remainder = wide & mask;
    let limbs: [u64; 8] = shifted.into_limbs();
    let base = U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]);
    if remainder > U512::ZERO {
        base + U256::from(1)
    } else {
        base
    }
}

fn shift_div_round_up(value: U256, price: U256) -> U256 {
    let shifted = value << 128;
    (shifted + price - U256::from(1)) / price
}

fn shift_div_round_down(value: U256, price: U256) -> U256 {
    (value << 128) / price
}

// =============================================================================
// Fee Calculation
// =============================================================================

/// Calculate total fee in 1e18 precision using the cached chain timestamp.
///
/// This is a deterministic convenience wrapper around `get_total_fee_at` and
/// never advances fee decay using host wall clock.
pub fn get_total_fee(state: &LBState) -> u128 {
    get_total_fee_at(state, state.time_of_last_update)
}

/// Calculate total fee at a specific Unix timestamp (deterministic).
/// totalFee = baseFee + variableFee (1e18 precision, capped at 10%).
pub fn get_total_fee_at(state: &LBState, now_secs: u64) -> u128 {
    // Saturating arithmetic throughout: the result is hard-capped at 10% (`min` below), so an
    // overflow on crafted/extreme fee params saturates to u128::MAX → clamps to the 10% max fee
    // (over-charges fee → under-quotes output → fail-closed). Identical to plain `*`/`+` for any
    // real LB pool, whose params never approach u128 overflow.
    let base_fee = (state.base_factor as u128)
        .saturating_mul(state.bin_step as u128)
        .saturating_mul(10_000_000_000u128);

    let effective_va = compute_effective_va(state, now_secs);

    let variable_fee = if state.variable_fee_control != 0 && effective_va > 0 {
        let prod = (effective_va as u128).saturating_mul(state.bin_step as u128);
        prod.saturating_mul(prod)
            .saturating_mul(state.variable_fee_control as u128)
            .saturating_add(99)
            / 100
    } else {
        0u128
    };

    let total = base_fee.saturating_add(variable_fee);
    total.min(100_000_000_000_000_000) // Cap at 0.1e18 = 10%
}

/// Total fee for ONE bin during a swap traversal, with the volatility accumulator recomputed for
/// that bin — `va = min(vol_ref + |bin − id_ref|·10_000, max_va)` — exactly the contract's
/// per-bin `updateVolatilityAccumulator(activeId)` inside `LBPair.swap`'s loop. A swap that
/// crosses bins pays a HIGHER variable fee on the later bins; charging the entry fee for the
/// whole traversal (the old behaviour) over-quotes multi-bin swaps (T2 found 3.9 bps on a real
/// 2-bin WETH/USDC.e fill).
#[inline]
fn total_fee_for_bin(state: &LBState, vol_ref: u32, id_ref: u32, bin: u32) -> u128 {
    let base_fee = (state.base_factor as u128)
        .saturating_mul(state.bin_step as u128)
        .saturating_mul(10_000_000_000u128);

    let variable_fee = if state.variable_fee_control != 0 {
        let delta_id = if bin >= id_ref {
            (bin - id_ref) as u64
        } else {
            (id_ref - bin) as u64
        };
        let mut va = (vol_ref as u64).saturating_add(delta_id.saturating_mul(10_000));
        if state.max_volatility_accumulator > 0 {
            va = va.min(state.max_volatility_accumulator as u64);
        }
        if va > 0 {
            let prod = (va as u128).saturating_mul(state.bin_step as u128);
            prod.saturating_mul(prod)
                .saturating_mul(state.variable_fee_control as u128)
                .saturating_add(99)
                / 100
        } else {
            0
        }
    } else {
        0u128
    };

    let total = base_fee.saturating_add(variable_fee);
    total.min(100_000_000_000_000_000) // Cap at 0.1e18 = 10%
}

/// Compute effective volatility accumulator at a given timestamp.
/// Implements the full Trader Joe V2.1 time-based decay algorithm.
///
/// On-chain: PairParameterHelper.updateVolatilityAccumulator()
/// Guard: time_of_last_update == 0 → returns raw VA (test/bench backward compat).
#[inline]
pub fn compute_effective_va(state: &LBState, now_secs: u64) -> u32 {
    if state.time_of_last_update == 0 {
        return state.volatility_accumulator;
    }
    if state.variable_fee_control == 0 {
        return 0;
    }

    let delta_time = now_secs.saturating_sub(state.time_of_last_update);

    // Step 2: Compute vol_ref (decayed volatility reference)
    let vol_ref = if delta_time >= state.filter_period as u64 {
        if delta_time >= state.decay_period as u64 {
            0u32
        } else {
            // Partial decay: reduction_factor * VA / 10_000
            if state.reduction_factor > 0 {
                ((state.volatility_accumulator as u64) * (state.reduction_factor as u64) / 10_000)
                    as u32
            } else {
                0
            }
        }
    } else {
        // Within filter window — use stored reference
        state.volatility_reference
    };

    // Step 3: delta_id = |active_id - id_reference|
    let delta_id = if state.active_id >= state.id_reference {
        (state.active_id - state.id_reference) as u64
    } else {
        (state.id_reference - state.active_id) as u64
    };

    // Step 4: new_va = vol_ref + delta_id * 10_000
    let new_va = (vol_ref as u64) + delta_id * 10_000;

    // Step 5: Cap at max_volatility_accumulator
    let capped = if state.max_volatility_accumulator > 0 {
        new_va.min(state.max_volatility_accumulator as u64)
    } else {
        new_va
    };
    capped as u32
}

/// Fee from amount that includes fees: fee = ceil(amount * totalFee / 1e18)
///
/// 256-bit intermediate: `amount * total_fee` overflows u128 for a single-bin amount above
/// ~2^128/totalFee (~3.4e21 wei at the 10% fee cap). The narrowing saturates to u128::MAX
/// (over-charges fee → under-quotes output → fail-closed), matching `get_total_fee_at`'s
/// saturating posture; the ceil division itself matches the LBPair contract exactly.
fn get_fee_amount_from(amount: u128, total_fee: u128) -> u128 {
    let num = U256::from(amount) * U256::from(total_fee);
    let den = U256::from(1_000_000_000_000_000_000u128);
    u128::try_from((num + den - U256::from(1u8)) / den).unwrap_or(u128::MAX)
}

/// Fee to add on top of base amount: fee = ceil(amount * totalFee / (1e18 - totalFee))
///
/// Same 256-bit widening + saturating narrow as `get_fee_amount_from` (see above).
/// `total_fee` is hard-capped at 10% of 1e18 upstream, so the denominator never underflows.
fn get_fee_amount(amount: u128, total_fee: u128) -> u128 {
    let num = U256::from(amount) * U256::from(total_fee);
    let den = U256::from(1_000_000_000_000_000_000u128 - total_fee);
    u128::try_from((num + den - U256::from(1u8)) / den).unwrap_or(u128::MAX)
}

// =============================================================================
// Single-Bin Swap Math (Constant-Sum)
// =============================================================================

/// Result from quoting a single bin swap
pub struct BinQuoteResult {
    pub amount_out: u128,
    pub fee: u128,
    pub amount_in_consumed: u128,
    pub drained: bool,
}

/// Post-swap state of a bin traversed during simulation.
#[derive(Debug, Clone, Copy)]
pub struct TraversedBin {
    pub bin_id: u32,
    pub reserve_x: u128,
    pub reserve_y: u128,
}

/// Detailed result from multi-bin LB swap simulation.
#[derive(Debug)]
pub struct LBSwapResult {
    pub amount_out: U256,
    pub final_active_id: u32,
    /// Input amount that could not be consumed (>0 means partial fill).
    pub remaining_in: u128,
    /// All bins traversed with their POST-SWAP reserves.
    /// Fully drained bins have their output reserve = 0 but input reserve > 0.
    pub traversed_bins: SmallVec<[TraversedBin; 8]>,
}

/// Quote exact input within a single bin.
pub fn quote_single_bin(
    amount_in: u128,
    reserve_x: u128,
    reserve_y: u128,
    bin_step: u16,
    active_id: u32,
    swap_for_y: bool, // true = X→Y
    total_fee: u128,  // in 1e18 precision
) -> Option<BinQuoteResult> {
    let price = get_price_from_id(active_id, bin_step);
    if price.is_zero() {
        return Some(BinQuoteResult {
            amount_out: 0,
            fee: 0,
            amount_in_consumed: 0,
            drained: false,
        });
    }
    let bin_reserve_out = if swap_for_y { reserve_y } else { reserve_x };

    if bin_reserve_out == 0 {
        return Some(BinQuoteResult {
            amount_out: 0,
            fee: 0,
            amount_in_consumed: 0,
            drained: false,
        });
    }

    // Max amount to drain this bin (before fee)
    let max_amount_in_no_fee = if swap_for_y {
        // X → Y: max_x = reserveY * 2^128 / price (round up)
        let v = shift_div_round_up(U256::from(bin_reserve_out), price);
        u256_to_u128_checked(v)?
    } else {
        // Y → X: max_y = reserveX * price / 2^128 (round up)
        let v = mul_shift_round_up(U256::from(bin_reserve_out), price);
        u256_to_u128_checked(v)?
    };
    let max_fee = get_fee_amount(max_amount_in_no_fee, total_fee);
    let max_amount_in = max_amount_in_no_fee.saturating_add(max_fee);

    if amount_in >= max_amount_in {
        // Drain entire bin
        Some(BinQuoteResult {
            amount_out: bin_reserve_out,
            fee: max_fee,
            amount_in_consumed: max_amount_in,
            drained: true,
        })
    } else {
        // Partial fill
        let fee = get_fee_amount_from(amount_in, total_fee);
        let amount_in_after_fee = amount_in - fee;
        let amount_out = if swap_for_y {
            // X * price / 2^128
            let v = mul_shift_round_down(U256::from(amount_in_after_fee), price);
            u256_to_u128_checked(v)?
        } else {
            // Y * 2^128 / price
            let v = shift_div_round_down(U256::from(amount_in_after_fee), price);
            u256_to_u128_checked(v)?
        };
        let amount_out = amount_out.min(bin_reserve_out);
        Some(BinQuoteResult {
            amount_out,
            fee,
            amount_in_consumed: amount_in,
            drained: false,
        })
    }
}

// Suppress the lint for this helper — it is read from mul_shift_round_down's return
#[allow(dead_code)]
fn mul_shift_round_down_u256(value: U256, price: U256) -> U256 {
    mul_shift_round_down(value, price)
}

/// Quote swap for simulation with cross-bin traversal.
///
/// Traverses bins when active bin is exhausted, up to `MAX_TRAVERSAL_BINS`.
/// Returns `None` if the swap cannot be fully filled (partial fills rejected).
pub fn simulate_lb_swap(
    state: &LBState,
    amount_in: U256,
    zero_for_one: bool,
) -> Option<(U256, u32)> {
    simulate_lb_swap_at(state, amount_in, zero_for_one, state.time_of_last_update)
}

/// Quote swap at an explicit chain timestamp.
///
/// Production callers should prefer this timestamp-explicit API.
pub fn simulate_lb_swap_at(
    state: &LBState,
    amount_in: U256,
    zero_for_one: bool,
    block_timestamp: u64,
) -> Option<(U256, u32)> {
    let result = simulate_lb_swap_inner_at(state, amount_in, zero_for_one, block_timestamp)?;
    if result.remaining_in > 0 {
        return None; // Reject partial fills
    }
    Some((result.amount_out, result.final_active_id))
}

/// Detailed swap simulation returning full `LBSwapResult` including drained bins.
///
/// Unlike `simulate_lb_swap`, this does NOT reject partial fills — the caller
/// can inspect `remaining_in` to determine how much was actually consumed.
pub fn simulate_lb_swap_detailed(
    state: &LBState,
    amount_in: U256,
    zero_for_one: bool,
) -> Option<LBSwapResult> {
    simulate_lb_swap_detailed_at(state, amount_in, zero_for_one, state.time_of_last_update)
}

/// Predict the post-swap state of an LB swap for the feed-driven state predictor
/// (TAIL_LATENCY_PLAN C2). Returns the output and a new `LBState` with `active_id` advanced to the
/// final bin and the traversed bins' POST-SWAP reserves written into a cloned `bin_map` (and the
/// active bin's top-level `reserve_x/_y` synced to it). Fee/volatility params are left as-is
/// (they evolve with the on-chain Swap event, reconciled by the resync). Sub-block freshness only;
/// never used for execution sizing.
pub fn apply_swap(state: &LBState, amount_in: U256, zero_for_one: bool) -> Option<(U256, LBState)> {
    apply_swap_at(state, amount_in, zero_for_one, state.time_of_last_update)
}

/// [`apply_swap`] at an explicit chain timestamp — the form the T2 parity fixture generator uses
/// (the executed swap's fee references decay to the EXECUTION block's timestamp, so an exact
/// post-state prediction must evaluate there; production `apply_swap` deliberately stays on
/// `time_of_last_update` = least-decayed/highest fee = conservative under-quote).
pub fn apply_swap_at(
    state: &LBState,
    amount_in: U256,
    zero_for_one: bool,
    block_timestamp: u64,
) -> Option<(U256, LBState)> {
    let result = simulate_lb_swap_detailed_at(state, amount_in, zero_for_one, block_timestamp)?;
    let mut new = state.clone();
    new.active_id = result.final_active_id;
    // Clone the bin window and write each traversed bin's post-swap reserves.
    let mut map = (*state.bin_map).clone();
    for tb in &result.traversed_bins {
        map.insert(
            tb.bin_id,
            BinData {
                reserve_x: tb.reserve_x,
                reserve_y: tb.reserve_y,
            },
        );
    }
    if let Some(active) = map.get(&new.active_id) {
        new.reserve_x = active.reserve_x;
        new.reserve_y = active.reserve_y;
    }
    new.bin_map = Arc::new(map);
    // Roll the fee references forward the way LBPair.swap does: references update once at entry
    // (updateReferences(ts)), the accumulator lands at the FINAL bin's value, and the update time
    // becomes the swap's timestamp. Skip on the documented `time_of_last_update == 0` test guard
    // (those fixtures carry raw accumulators with no time semantics).
    if state.time_of_last_update != 0 {
        let (vol_ref, id_ref) = compute_post_swap_references(
            state.active_id,
            state.volatility_accumulator,
            state.volatility_reference,
            state.id_reference,
            state.time_of_last_update,
            block_timestamp,
            state.filter_period,
            state.decay_period,
            state.reduction_factor,
        );
        let delta_id = if new.active_id >= id_ref {
            (new.active_id - id_ref) as u64
        } else {
            (id_ref - new.active_id) as u64
        };
        let mut va = (vol_ref as u64).saturating_add(delta_id.saturating_mul(10_000));
        if state.max_volatility_accumulator > 0 {
            va = va.min(state.max_volatility_accumulator as u64);
        }
        new.volatility_reference = vol_ref;
        new.id_reference = id_ref;
        new.volatility_accumulator = va as u32;
        new.time_of_last_update = block_timestamp;
    }
    Some((result.amount_out, new))
}

/// Detailed swap simulation at an explicit chain timestamp.
///
/// Unlike `simulate_lb_swap_at`, this does NOT reject partial fills — the caller
/// can inspect `remaining_in` to determine how much was actually consumed.
pub fn simulate_lb_swap_detailed_at(
    state: &LBState,
    amount_in: U256,
    zero_for_one: bool,
    block_timestamp: u64,
) -> Option<LBSwapResult> {
    simulate_lb_swap_inner_at(state, amount_in, zero_for_one, block_timestamp)
}

/// Core cross-bin swap loop returning `LBSwapResult`.
fn simulate_lb_swap_inner_at(
    state: &LBState,
    amount_in: U256,
    zero_for_one: bool,
    block_timestamp: u64,
) -> Option<LBSwapResult> {
    if state.reserve_x == 0 && state.reserve_y == 0 && state.bin_map.is_empty() {
        return None;
    }

    // In LB, tokenX is always the lower-address token (same as token0).
    // So zero_for_one (selling token0) = swapForY (selling tokenX for tokenY).
    let swap_for_y = zero_for_one;
    // Fee model (matches LBPair.swap): references update ONCE at entry
    // (updateReferences(block.timestamp)), then the volatility accumulator — and so the variable
    // fee — is recomputed PER BIN as the traversal advances. The legacy constant-fee path is kept
    // for the documented `time_of_last_update == 0` test/bench guard and for pools with no
    // variable fee (where the per-bin recompute is a no-op by construction).
    let const_fee = if state.time_of_last_update == 0 || state.variable_fee_control == 0 {
        Some(get_total_fee_at(state, block_timestamp))
    } else {
        None
    };
    let (vol_ref, id_ref) = compute_post_swap_references(
        state.active_id,
        state.volatility_accumulator,
        state.volatility_reference,
        state.id_reference,
        state.time_of_last_update,
        block_timestamp,
        state.filter_period,
        state.decay_period,
        state.reduction_factor,
    );
    let mut remaining = u256_to_u128_checked(amount_in)?;
    let mut total_out: u128 = 0;
    let mut current_id = state.active_id;
    let mut last_processed_id = state.active_id;
    let mut traversed_bins: SmallVec<[TraversedBin; 8]> = SmallVec::new();

    let mut bins_processed: u32 = 0;
    let mut gaps_skipped: u32 = 0;

    loop {
        if bins_processed >= MAX_TRAVERSAL_BINS || gaps_skipped >= MAX_GAP_BINS {
            break;
        }

        let (rx, ry) = if current_id == state.active_id {
            (state.reserve_x, state.reserve_y)
        } else {
            match state.bin_map.get(&current_id) {
                Some(b) => (b.reserve_x, b.reserve_y),
                None => {
                    // Bin not in local map — skip and continue searching.
                    // On-chain, TreeMath would jump to next non-empty bin.
                    gaps_skipped += 1;
                    current_id = match next_bin_id(current_id, swap_for_y) {
                        Some(id) => id,
                        None => break,
                    };
                    continue;
                }
            }
        };

        if (swap_for_y && ry == 0) || (!swap_for_y && rx == 0) {
            // This bin has no output reserves, skip to next
            gaps_skipped += 1;
            current_id = match next_bin_id(current_id, swap_for_y) {
                Some(id) => id,
                None => break,
            };
            continue;
        }

        bins_processed += 1;
        let bin_fee =
            const_fee.unwrap_or_else(|| total_fee_for_bin(state, vol_ref, id_ref, current_id));
        let result = quote_single_bin(
            remaining,
            rx,
            ry,
            state.bin_step,
            current_id,
            swap_for_y,
            bin_fee,
        )?;
        total_out = total_out.saturating_add(result.amount_out);
        remaining = remaining.saturating_sub(result.amount_in_consumed);

        // Track post-swap reserves for this bin
        let (post_rx, post_ry) = if swap_for_y {
            // X→Y: bin gained input X, lost output Y
            (
                rx.saturating_add(result.amount_in_consumed),
                ry.saturating_sub(result.amount_out),
            )
        } else {
            // Y→X: bin lost output X, gained input Y
            (
                rx.saturating_sub(result.amount_out),
                ry.saturating_add(result.amount_in_consumed),
            )
        };
        traversed_bins.push(TraversedBin {
            bin_id: current_id,
            reserve_x: post_rx,
            reserve_y: post_ry,
        });
        last_processed_id = current_id;

        if !result.drained || remaining == 0 {
            break; // Swap complete
        }

        // Move to next bin
        current_id = match next_bin_id(current_id, swap_for_y) {
            Some(id) => id,
            None => break,
        };
    }

    if total_out == 0 {
        None
    } else {
        Some(LBSwapResult {
            amount_out: U256::from(total_out),
            final_active_id: last_processed_id,
            remaining_in: remaining,
            traversed_bins,
        })
    }
}

/// Get next bin ID in traversal direction.
///
/// Trader Joe's on-chain TreeMath uses counter-intuitive naming:
/// `findFirstRight(id)` → first non-empty bin at LOWER ID (bitmap convention).
/// `findFirstLeft(id)` → first non-empty bin at HIGHER ID.
///
/// LB bin reserves: lower IDs hold Y, higher IDs hold X.
/// - swapForY (X→Y): need Y → traverse DOWN (checked_sub)
/// - !swapForY (Y→X): need X → traverse UP (checked_add)
fn next_bin_id(current: u32, swap_for_y: bool) -> Option<u32> {
    if swap_for_y {
        current.checked_sub(1)
    } else {
        current.checked_add(1)
    }
}

/// Replay on-chain PairParameterHelper.updateReferences() to compute
/// the correct volatility_reference and id_reference after a swap event.
///
/// On-chain, updateReferences runs BEFORE the swap's bin traversal, using
/// the PRE-SWAP state. The returned values match what the contract stores
/// in its FeeParameters after the swap.
///
/// Returns (new_volatility_reference, new_id_reference).
pub fn compute_post_swap_references(
    pre_swap_active_id: u32,
    stored_vol_acc: u32,
    stored_vol_ref: u32,
    stored_id_ref: u32,
    stored_time_of_last_update: u64,
    block_timestamp: u64,
    filter_period: u16,
    decay_period: u16,
    reduction_factor: u16,
) -> (u32, u32) {
    // Guard: uninitialized state (backward compat for tests/benches)
    if stored_time_of_last_update == 0 {
        return (stored_vol_ref, stored_id_ref);
    }

    let dt = block_timestamp.saturating_sub(stored_time_of_last_update);

    if dt >= filter_period as u64 {
        // On-chain: updateIdReference() sets idRef = getActiveId() (pre-swap)
        let new_id_ref = pre_swap_active_id;

        // On-chain: updateVolatilityReference() or setVolatilityReference(0)
        let new_vol_ref = if dt >= decay_period as u64 {
            0 // Full decay
        } else if reduction_factor > 0 {
            ((stored_vol_acc as u64) * (reduction_factor as u64) / 10_000) as u32
        } else {
            0
        };
        (new_vol_ref, new_id_ref)
    } else {
        // Within filter window: references unchanged
        (stored_vol_ref, stored_id_ref)
    }
}

// =============================================================================
// Fee Encoding Helpers
// =============================================================================

/// Encode LB fee for pool discovery path (executor routing).
/// Format: `((version-2) << 14) | binStep`. Fits u16 for CompositeTrade fee_tier.
/// Supports binStep 0–16383 and versions 2–3.
#[inline]
pub fn lb_fee_encode_discovery(version: u32, bin_step: u32) -> u32 {
    ((version.saturating_sub(2)) << 14) | (bin_step & 0x3FFF)
}

/// Decode LB fee from discovery encoding. Returns `(version, bin_step)`.
#[inline]
pub fn lb_fee_decode_discovery(fee: u32) -> (u32, u32) {
    let version = ((fee >> 14) & 0x3) + 2;
    let bin_step = fee & 0x3FFF;
    (version, bin_step)
}

/// Encode LB fee for swap hop (backrun/state_predictor path).
/// Format: `(version << 16) | binStep`. Version in upper bits avoids V3 fee collision.
#[inline]
pub fn lb_fee_encode_hop(version: u32, bin_step: u32) -> u32 {
    (version << 16) | (bin_step & 0xFFFF)
}

/// Decode LB fee from hop encoding. Returns `(version, bin_step)`.
#[inline]
pub fn lb_fee_decode_hop(fee: u32) -> (u32, u16) {
    ((fee >> 16) & 0xFF, (fee & 0xFFFF) as u16)
}

/// Extract raw binStep from a discovery-encoded fee.
#[inline]
pub fn lb_bin_step_from_discovery_fee(fee: u32) -> u32 {
    fee & 0x3FFF
}

/// Default Trader Joe base_factor on Arbitrum (used when real value unavailable).
const LB_DEFAULT_BASE_FACTOR: u32 = 5000;

/// Approximate LB base fee in ppm from binStep.
/// Uses default base_factor=5000 (standard on Arbitrum).
/// baseFee_ppm = baseFactor * binStep / 100
#[inline]
pub fn lb_fee_ppm_from_bin_step(bin_step: u32) -> u32 {
    (LB_DEFAULT_BASE_FACTOR * bin_step) / 100
}

/// Total LB fee in ppm from actual pool state (base + variable).
/// More accurate than `lb_fee_ppm_from_bin_step` which only uses base fee.
#[inline]
pub fn lb_total_fee_ppm_at(state: &LBState, now_secs: u64) -> u32 {
    let total_fee_1e18 = get_total_fee_at(state, now_secs);
    // 1e18 → ppm: divide by 1e12
    (total_fee_1e18 / 1_000_000_000_000u128) as u32
}

/// Total LB fee in ppm from actual pool state (base + variable) using the
/// cached chain timestamp.
#[inline]
pub fn lb_total_fee_ppm(state: &LBState) -> u32 {
    lb_total_fee_ppm_at(state, state.time_of_last_update)
}

/// Aggregate LB reserves from active bin + all cached neighboring bins.
/// Returns (total_x, total_y, bin_count).
///
/// Active bin reserves (`state.reserve_x/y`) are always included.
/// Neighboring bins from `state.bin_map` (typically ±50 bins around active_id)
/// are added via saturating arithmetic to prevent overflow.
pub fn lb_aggregate_reserves(state: &LBState) -> (u128, u128, usize) {
    let mut total_x: u128 = state.reserve_x;
    let mut total_y: u128 = state.reserve_y;
    let mut bin_count: usize = 1;
    for (_bin_id, bin_data) in state.bin_map.iter() {
        total_x = total_x.saturating_add(bin_data.reserve_x);
        total_y = total_y.saturating_add(bin_data.reserve_y);
        bin_count += 1;
    }
    (total_x, total_y, bin_count)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_at_mid_bin() {
        // binStep=20, id=8388608 → price = 1.0 (SCALE)
        let price = get_price_from_id(8_388_608, 20);
        assert_eq!(price, SCALE, "Mid bin should have price = 1.0");
    }

    #[test]
    fn test_price_one_above_mid() {
        // binStep=20, id=8388609 → price ≈ 1.002
        let price = get_price_from_id(8_388_609, 20);
        // Should be slightly above SCALE
        assert!(price > SCALE, "One bin above mid should be > 1.0");
        // 1.002 * SCALE ≈ SCALE + 0.002 * SCALE
        let expected_approx = SCALE + (U256::from(2u64) << 128) / U256::from(1000u64);
        // Allow 0.01% tolerance
        let diff = if price > expected_approx {
            price - expected_approx
        } else {
            expected_approx - price
        };
        let tolerance = SCALE / U256::from(10000u64); // 0.01%
        assert!(diff < tolerance, "Price should be ≈ 1.002");
    }

    #[test]
    fn test_fee_calculation() {
        // baseFactor=5000, binStep=20 → baseFee = 5000*20*1e10 = 1e15 → 0.1%
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 1_000_000_000_000_000_000, // 1e18
            reserve_y: 1_000_000_000_000_000_000,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 0,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        let fee = get_total_fee(&state);
        // 5000 * 20 * 1e10 = 1e15 = 0.001 * 1e18 = 0.1%
        assert_eq!(fee, 1_000_000_000_000_000u128);
    }

    #[test]
    fn test_single_bin_swap_partial() {
        // At mid-bin, price = 1.0, so X→Y gives roughly amount_in * (1-fee)
        let total_fee = 1_000_000_000_000_000u128; // 0.1%
        let result = quote_single_bin(
            1_000_000, 10_000_000, 10_000_000, 20, 8_388_608, true, total_fee,
        )
        .expect("single-bin quote should fit within u128");
        assert!(!result.drained, "Should not drain bin with small swap");
        assert!(result.amount_out > 0, "Should get some output");
        assert!(result.fee > 0, "Should charge some fee");
        // amount_out should be approximately 999_000 (1M * 0.999)
        assert!(result.amount_out > 990_000 && result.amount_out < 1_000_000);
    }

    #[test]
    fn test_stale_pool_with_reserves_can_simulate() {
        // With cross-bin support, stale pools with valid reserves can still simulate
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 1_000_000,
            reserve_y: 1_000_000,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 0,
            log_index: 0,
            stale: true,
            bin_map: Arc::new(BTreeMap::new()),
        };
        let result = simulate_lb_swap(&state, U256::from(1000u64), true);
        assert!(
            result.is_some(),
            "Pool with reserves should simulate even if stale"
        );
    }

    #[test]
    fn test_empty_pool_returns_none() {
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 0,
            reserve_y: 0,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 0,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        let result = simulate_lb_swap(&state, U256::from(1000u64), true);
        assert!(result.is_none(), "Empty pool should return None");
    }

    #[test]
    fn test_simulate_lb_swap_basic() {
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 10_000_000_000_000_000_000u128, // 10 ETH
            reserve_y: 10_000_000_000_000_000_000u128,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        let result = simulate_lb_swap(&state, amount_in, true);
        assert!(result.is_some(), "Should produce output");
        let (out, final_id) = result.unwrap();
        assert_eq!(
            final_id, 8_388_608,
            "Same-bin swap should not change active_id"
        );
        // At price=1.0 with 0.1% fee: out ≈ 0.999 ETH
        assert!(out > U256::from(990_000_000_000_000_000u128));
        assert!(out < U256::from(1_000_000_000_000_000_000u128));
    }

    #[test]
    fn test_simulate_lb_swap_skips_missing_bin() {
        // Active bin drained of X, next bin (active_id+1) missing, bin at active_id+2 has X reserves
        // Uses Y→X (swap_for_y=false) to traverse UP where price exponents are positive
        let active = 8_388_608u32;
        let mut bin_map = BTreeMap::new();
        // Skip active_id+1 (not in map) — bin at active_id+2 has X reserves
        bin_map.insert(
            active + 2,
            BinData {
                reserve_x: 5_000_000_000_000_000_000u128, // 5 ETH of X
                reserve_y: 0,
            },
        );
        let state = LBState {
            active_id: active,
            bin_step: 20,
            reserve_x: 0, // Active bin drained of X
            reserve_y: 1_000_000_000_000_000_000u128,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bin_map),
        };
        // Swap Y→X: should skip missing bin and find reserves 2 bins away
        let result = simulate_lb_swap(&state, U256::from(1_000_000u128), false);
        assert!(
            result.is_some(),
            "Should find reserves by skipping missing bin"
        );
        let (out, final_id) = result.unwrap();
        assert!(out > U256::ZERO, "Should produce output");
        assert_eq!(final_id, active + 2, "Should land on bin with reserves");
    }

    #[test]
    fn test_simulate_lb_swap_no_bins_available() {
        // Active bin drained, bin_map empty — traversal should exhaust max_bins
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 0, // Drained of X
            reserve_y: 1_000_000,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        // Y→X traversal (upward) with empty bin_map
        let result = simulate_lb_swap(&state, U256::from(1_000_000u128), false);
        assert!(result.is_none(), "No bins available should return None");
    }

    #[test]
    fn test_lb_fee_encode_decode_discovery_round_trip() {
        for (version, bin_step) in [
            (2, 20),
            (3, 100),
            (2, 255),
            (2, 500),
            (3, 1000),
            (2, 16383),
            (5, 200),
        ] {
            let encoded = lb_fee_encode_discovery(version, bin_step);
            let (dec_v, dec_bs) = lb_fee_decode_discovery(encoded);
            assert_eq!(
                dec_v, version,
                "version mismatch for ({version}, {bin_step})"
            );
            assert_eq!(
                dec_bs, bin_step,
                "bin_step mismatch for ({version}, {bin_step})"
            );
            // Must fit in u16 for CompositeTrade fee_tier
            assert!(
                encoded <= 0xFFFF,
                "encoded {encoded} exceeds u16 for ({version}, {bin_step})"
            );
        }
    }

    #[test]
    fn test_lb_fee_encode_decode_hop_round_trip() {
        for (version, bin_step) in [(2u32, 20u32), (3, 100), (2, 255)] {
            let encoded = lb_fee_encode_hop(version, bin_step);
            let (dec_v, dec_bs) = lb_fee_decode_hop(encoded);
            assert_eq!(
                dec_v, version,
                "version mismatch for ({version}, {bin_step})"
            );
            assert_eq!(
                dec_bs, bin_step as u16,
                "bin_step mismatch for ({version}, {bin_step})"
            );
        }
        // V3 fees (version=0) should decode with version=0
        for v3_fee in [500u32, 3000, 10000] {
            let (dec_v, dec_bs) = lb_fee_decode_hop(v3_fee);
            assert_eq!(dec_v, 0, "V3 fee {v3_fee} should have version=0");
            assert_eq!(dec_bs, v3_fee as u16, "V3 fee {v3_fee} bin_step mismatch");
        }
    }

    #[test]
    fn test_lb_bin_step_from_discovery_fee() {
        assert_eq!(
            lb_bin_step_from_discovery_fee(lb_fee_encode_discovery(2, 20)),
            20
        );
        assert_eq!(
            lb_bin_step_from_discovery_fee(lb_fee_encode_discovery(3, 100)),
            100
        );
        assert_eq!(
            lb_bin_step_from_discovery_fee(lb_fee_encode_discovery(2, 255)),
            255
        );
        assert_eq!(
            lb_bin_step_from_discovery_fee(lb_fee_encode_discovery(2, 500)),
            500
        );
        assert_eq!(
            lb_bin_step_from_discovery_fee(lb_fee_encode_discovery(3, 16383)),
            16383
        );
    }

    #[test]
    fn test_max_traversal_bins_constant() {
        assert_eq!(MAX_TRAVERSAL_BINS, 50);
    }

    #[test]
    fn test_simulate_lb_swap_truncation_returns_none() {
        // Pool with tiny X reserves across 2 bins — massive Y input cannot be fully filled.
        // Uses Y→X swap (zero_for_one=false) to traverse UPWARD (positive exponents).
        let active = 8_388_608u32;
        let mut bin_map = BTreeMap::new();
        bin_map.insert(
            active + 1,
            BinData {
                reserve_x: 100, // tiny X
                reserve_y: 0,
            },
        );
        let state = LBState {
            active_id: active,
            bin_step: 20,
            reserve_x: 100, // tiny X in active bin
            reserve_y: 0,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bin_map),
        };
        // Massive Y input that cannot be fully consumed by 200 units of X
        let result = simulate_lb_swap(&state, U256::from(1_000_000_000_000u128), false);
        assert!(
            result.is_none(),
            "Partial fill should return None from simulate_lb_swap"
        );
    }

    #[test]
    fn test_simulate_lb_swap_detailed_tracks_drains() {
        // Pool with 3 bins: active + 2 neighbors above (for Y→X traversal upward).
        // Uses zero_for_one=false so traversal goes to HIGHER IDs (positive exponents).
        let active = 8_388_608u32;
        let mut bin_map = BTreeMap::new();
        // Neighbor bins with X reserves (Y→X traverses upward)
        bin_map.insert(
            active + 1,
            BinData {
                reserve_x: 1_000_000_000_000_000_000u128, // 1 ETH
                reserve_y: 0,
            },
        );
        bin_map.insert(
            active + 2,
            BinData {
                reserve_x: 1_000_000_000_000_000_000u128, // 1 ETH
                reserve_y: 0,
            },
        );
        let state = LBState {
            active_id: active,
            bin_step: 20,
            reserve_x: 1_000_000_000_000_000_000u128, // 1 ETH in active
            reserve_y: 0,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bin_map),
        };
        // Send enough Y to drain active + neighbor (at price≈1.0, ~2.5 ETH drains 2 bins)
        let result =
            simulate_lb_swap_detailed(&state, U256::from(2_500_000_000_000_000_000u128), false);
        assert!(result.is_some(), "Should produce output");
        let res = result.unwrap();
        assert!(res.amount_out > U256::ZERO, "Should have output");
        // Active bin and at least one neighbor should be traversed
        assert!(
            !res.traversed_bins.is_empty(),
            "Should track at least one traversed bin"
        );
        assert!(
            res.traversed_bins.iter().any(|tb| tb.bin_id == active),
            "Active bin should be traversed"
        );
        assert!(
            res.traversed_bins.iter().any(|tb| tb.bin_id == active + 1),
            "First neighbor should be traversed"
        );
        assert_eq!(res.remaining_in, 0, "Input should be fully consumed");
    }

    // =========================================================================
    // LB-FIX-001: Aggregate reserves regression tests
    // =========================================================================

    #[test]
    fn test_lb_aggregate_reserves_active_only() {
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 1_000_000,
            reserve_y: 2_000_000,
            base_factor: 0,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        let (x, y, count) = lb_aggregate_reserves(&state);
        assert_eq!(x, 1_000_000);
        assert_eq!(y, 2_000_000);
        assert_eq!(count, 1, "Active bin only");
    }

    #[test]
    fn test_lb_aggregate_reserves_with_neighbors() {
        let mut bin_map = BTreeMap::new();
        for i in 1..=10 {
            bin_map.insert(
                8_388_608 + i,
                BinData {
                    reserve_x: 500_000,
                    reserve_y: 300_000,
                },
            );
        }
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 1_000_000,
            reserve_y: 2_000_000,
            base_factor: 0,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bin_map),
        };
        let (x, y, count) = lb_aggregate_reserves(&state);
        // active (1M) + 10 bins * 500k = 6M
        assert_eq!(x, 6_000_000);
        // active (2M) + 10 bins * 300k = 5M
        assert_eq!(y, 5_000_000);
        assert_eq!(count, 11, "Active + 10 neighbor bins");
    }

    #[test]
    fn test_lb_aggregate_reserves_saturating() {
        let mut bin_map = BTreeMap::new();
        bin_map.insert(
            1,
            BinData {
                reserve_x: u128::MAX,
                reserve_y: 0,
            },
        );
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 1,
            reserve_y: 0,
            base_factor: 0,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bin_map),
        };
        let (x, _y, _) = lb_aggregate_reserves(&state);
        assert_eq!(x, u128::MAX, "Should saturate, not overflow");
    }

    // C2: apply_swap returns the same output as the detailed sim, and writes the post-swap reserves
    // into the cloned bin_map (active bin gets more X, less Y on an X→Y swap) + advances active_id.
    #[test]
    fn apply_swap_updates_bin_map_and_active_id() {
        let active = 8_388_608u32;
        let mut bin_map = BTreeMap::new();
        bin_map.insert(
            active,
            BinData {
                reserve_x: 1_000_000_000_000_000_000u128,
                reserve_y: 1_000_000_000_000_000_000u128,
            },
        );
        let state = LBState {
            active_id: active,
            bin_step: 20,
            reserve_x: 1_000_000_000_000_000_000u128,
            reserve_y: 1_000_000_000_000_000_000u128,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bin_map),
        };
        // Small X→Y swap (zero_for_one=true) — stays in the active bin.
        let dx = U256::from(10_000_000_000_000_000u128); // 0.01 X
        let detailed = simulate_lb_swap_detailed(&state, dx, true).expect("sim");
        let (out, new) = apply_swap(&state, dx, true).expect("apply");
        assert_eq!(
            out, detailed.amount_out,
            "apply output == detailed sim output"
        );
        let active_bin = new.bin_map.get(&new.active_id).expect("active bin present");
        assert!(
            active_bin.reserve_x > 1_000_000_000_000_000_000u128,
            "X up after X→Y"
        );
        assert!(
            active_bin.reserve_y < 1_000_000_000_000_000_000u128,
            "Y down after X→Y"
        );
        assert_eq!(
            new.reserve_x, active_bin.reserve_x,
            "top-level synced to active bin"
        );
    }

    // =========================================================================
    // BUG-1 FIX: final_active_id always in traversed_bins
    // =========================================================================

    #[test]
    fn test_simulate_lb_swap_final_id_in_traversed_bins_with_gap() {
        // Setup: active bin drained of X, then a gap (missing bin at active+1),
        // then a reachable bin at active+2 with X reserves.
        // Y→X swap drains active's X (zero), skips missing bin, lands on active+2.
        // Verify: final_active_id is always found in traversed_bins.
        let active = 8_388_608u32;
        let mut bin_map = BTreeMap::new();
        // Gap: active+1 NOT in map. Bin at active+2 has X reserves.
        bin_map.insert(
            active + 2,
            BinData {
                reserve_x: 5_000_000_000_000_000_000u128,
                reserve_y: 0,
            },
        );
        let state = LBState {
            active_id: active,
            bin_step: 20,
            reserve_x: 1_000_000_000_000_000_000u128, // 1 ETH X in active
            reserve_y: 0,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bin_map),
        };
        // Y→X swap (zero_for_one=false) large enough to drain active bin X
        // and continue traversing upward through the gap to active+2
        let result =
            simulate_lb_swap_detailed(&state, U256::from(2_000_000_000_000_000_000u128), false);
        let res = result.expect("should produce output");
        assert!(
            res.traversed_bins
                .iter()
                .any(|tb| tb.bin_id == res.final_active_id),
            "final_active_id {} must be in traversed_bins {:?}",
            res.final_active_id,
            res.traversed_bins
                .iter()
                .map(|tb| tb.bin_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_simulate_lb_swap_final_id_single_bin_no_gap() {
        // Same-bin swap: final_active_id should be the active bin itself
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 0,
            reserve_y: 10_000_000_000_000_000_000u128,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        let result =
            simulate_lb_swap_detailed(&state, U256::from(1_000_000_000_000_000_000u128), true);
        let res = result.expect("should produce output");
        assert_eq!(res.final_active_id, 8_388_608);
        assert!(res
            .traversed_bins
            .iter()
            .any(|tb| tb.bin_id == res.final_active_id));
    }

    // =========================================================================
    // LB-FIX: Issue 5 — U256→u128 saturation safety
    // =========================================================================

    #[test]
    fn test_u256_max_input_no_panic() {
        // Pass U256::MAX to simulate_lb_swap — must not panic
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 10_000_000_000_000_000_000u128,
            reserve_y: 10_000_000_000_000_000_000u128,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        // Should not panic — partial fill rejected returns None
        let result = simulate_lb_swap(&state, U256::MAX, true);
        // Either None (partial fill rejected) or Some — no panic
        let _ = result;
    }

    // =========================================================================
    // LB-FIX: Issue 6 — Zero-price guard in quote_single_bin
    // =========================================================================

    #[test]
    fn test_quote_single_bin_zero_price() {
        // Directly test the zero-price guard by calling with a price of zero
        // (get_price_from_id may not return zero for valid bin IDs, but the guard
        // protects against it if it does)
        let price = get_price_from_id(8_388_608, 20);
        assert!(!price.is_zero(), "Mid bin price should be non-zero (SCALE)");

        // Test the guard indirectly: a bin_id far below midpoint produces very small
        // price that approaches zero — the function should handle gracefully
        // Use a moderately low bin ID that won't cause overflow in pow_128x128
        let low_bin_id = 8_388_608 - 10_000; // reasonable negative exponent
        let price_low = get_price_from_id(low_bin_id, 20);
        // If price rounds to zero, quote_single_bin should return zero output
        if price_low.is_zero() {
            let result = quote_single_bin(
                1_000_000,
                10_000_000,
                10_000_000,
                20,
                low_bin_id,
                true,
                1_000_000_000_000_000u128,
            )
            .expect("zero-price guard should still return a quote result");
            assert_eq!(result.amount_out, 0);
            assert_eq!(result.fee, 0);
            assert_eq!(result.amount_in_consumed, 0);
        }
        // Either way, verify the guard is in place for the literal zero case
    }

    #[test]
    fn test_quote_single_bin_with_zero_price_direct() {
        // Simulate a zero-price scenario by testing the boundary:
        // At very low bin IDs, price can underflow to 0 in 128.128 fixed-point
        // Our guard returns a zero-output result instead of panicking
        let result = quote_single_bin(
            0, // zero input with zero reserves — should handle gracefully
            0,
            0,
            20,
            8_388_608,
            true,
            1_000_000_000_000_000u128,
        )
        .expect("zero-input quote should fit within u128");
        assert_eq!(result.amount_out, 0);
        assert!(!result.drained);
    }

    // =========================================================================
    // LB-FIX: Issue 7 — Dual-counter traversal for sparse bins
    // =========================================================================

    #[test]
    fn test_traversal_sparse_bins() {
        // Place bins 100 apart — old loop would exhaust MAX_TRAVERSAL_BINS
        // on gaps before reaching the initialized bin
        let active = 8_388_608u32;
        let mut bin_map = BTreeMap::new();
        // Place a bin 100 steps away (sparse)
        bin_map.insert(
            active + 100,
            BinData {
                reserve_x: 5_000_000_000_000_000_000u128,
                reserve_y: 0,
            },
        );
        let state = LBState {
            active_id: active,
            bin_step: 20,
            reserve_x: 0, // Active bin drained of X
            reserve_y: 1_000_000_000_000_000_000u128,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(bin_map),
        };
        // Y→X traversal should skip 100 empty bins and reach the initialized one
        let result = simulate_lb_swap(&state, U256::from(1_000_000u128), false);
        assert!(
            result.is_some(),
            "Dual-counter traversal should reach sparse bin 100 steps away"
        );
        let (out, final_id) = result.unwrap();
        assert!(out > U256::ZERO);
        assert_eq!(final_id, active + 100);
    }

    // =========================================================================
    // LB-FIX: Issue 8 — lb_total_fee_ppm
    // =========================================================================

    #[test]
    fn test_lb_total_fee_ppm_base_only() {
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 1_000_000,
            reserve_y: 1_000_000,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0, // no variable fee
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        // baseFee = 5000 * 20 * 1e10 = 1e15 → 1e15 / 1e12 = 1000 ppm
        assert_eq!(lb_total_fee_ppm(&state), 1000);
    }

    #[test]
    fn test_lb_total_fee_ppm_with_variable() {
        let state = LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 1_000_000,
            reserve_y: 1_000_000,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 100_000,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 50_000, // significant volatility
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        };
        let total = lb_total_fee_ppm(&state);
        // Should be > base fee (1000 ppm) due to variable component
        assert!(
            total > 1000,
            "Variable fee should increase total: got {}",
            total
        );
    }

    // =========================================================================
    // LB-FIX: checked U256 -> u128 conversions fail closed
    // =========================================================================

    #[test]
    fn test_quote_single_bin_rejects_overflowing_intermediate_amounts() {
        let total_fee = 1_000_000_000_000_000u128; // 0.1%
        let huge_reserve = u128::MAX;
        let below_midpoint = 8_388_607u32; // price < 1.0 but still non-zero
        assert!(
            quote_single_bin(
                huge_reserve,
                huge_reserve,
                huge_reserve,
                20,
                below_midpoint,
                true,
                total_fee,
            )
            .is_none(),
            "overflowing intermediate amounts must fail closed instead of saturating"
        );
    }

    // =========================================================================
    // V2.1 Volatility Decay — compute_effective_va / get_total_fee_at
    // =========================================================================

    /// Helper: create an LBState with specified decay parameters.
    fn make_decay_state(
        va: u32,
        vol_ref: u32,
        id_ref: u32,
        active_id: u32,
        filter_period: u16,
        decay_period: u16,
        reduction_factor: u16,
        max_va: u32,
        vfc: u32,
        time_of_last_update: u64,
    ) -> LBState {
        LBState {
            active_id,
            bin_step: 20,
            reserve_x: 0,
            reserve_y: 0,
            base_factor: 5000,
            filter_period,
            decay_period,
            reduction_factor,
            variable_fee_control: vfc,
            protocol_share: 0,
            max_volatility_accumulator: max_va,
            volatility_accumulator: va,
            volatility_reference: vol_ref,
            id_reference: id_ref,
            time_of_last_update,
            block_number: 100,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        }
    }

    #[test]
    fn test_compute_effective_va_uninitialized() {
        // time_of_last_update == 0 → returns raw VA (backward compat guard)
        let state = make_decay_state(
            200_000, // va
            50_000,  // vol_ref
            8_388_608, 8_388_608, 30, 600, 5000, 350_000, 100_000,
            0, // time_of_last_update = 0
        );
        assert_eq!(compute_effective_va(&state, 1_700_000_000), 200_000);
    }

    #[test]
    fn test_compute_effective_va_full_decay() {
        // delta_time = 700 >= decay_period = 600, delta_id = 0 → VA = 0
        let state = make_decay_state(
            200_000, 50_000, 8_388_608, 8_388_608, // active == id_ref → delta_id = 0
            30, 600, 5000, 350_000, 100_000, 1_000, // time_of_last_update
        );
        let va = compute_effective_va(&state, 1_700); // delta = 700 >= 600
        assert_eq!(va, 0);
    }

    #[test]
    fn test_compute_effective_va_partial_decay() {
        // filter_period=30 <= delta_time=100 < decay_period=600
        // reduction_factor=5000 (50%), VA=200_000
        // vol_ref = 5000 * 200_000 / 10_000 = 100_000
        // delta_id = 0 → new_va = 100_000
        let state = make_decay_state(
            200_000, 50_000, 8_388_608, 8_388_608, 30, 600, 5000, 350_000, 100_000, 1_000,
        );
        let va = compute_effective_va(&state, 1_100); // delta = 100
        assert_eq!(va, 100_000);
    }

    #[test]
    fn test_compute_effective_va_within_filter_period() {
        // delta_time=10 < filter_period=30 → returns stored volatility_reference
        let state = make_decay_state(
            200_000, 75_000, // vol_ref = 75_000
            8_388_608, 8_388_608, 30, 600, 5000, 350_000, 100_000, 1_000,
        );
        let va = compute_effective_va(&state, 1_010); // delta = 10 < 30
        assert_eq!(va, 75_000);
    }

    #[test]
    fn test_compute_effective_va_delta_id() {
        // Full time decay (delta >= decay), active_id - id_ref = 10
        // new_va = 0 + 10 * 10_000 = 100_000
        let state = make_decay_state(
            200_000, 50_000, 8_388_608, // id_reference
            8_388_618, // active_id (10 bins away)
            30, 600, 5000, 350_000, 100_000, 1_000,
        );
        let va = compute_effective_va(&state, 1_700); // delta = 700 >= 600
        assert_eq!(va, 100_000);
    }

    #[test]
    fn test_compute_effective_va_max_cap() {
        // Large delta_id = 100 → uncapped VA = 0 + 100 * 10_000 = 1_000_000
        // max_volatility_accumulator = 350_000 → capped
        let state = make_decay_state(
            200_000, 50_000, 8_388_608, // id_reference
            8_388_708, // active_id (100 bins)
            30, 600, 5000, 350_000, 100_000, 1_000,
        );
        let va = compute_effective_va(&state, 1_700); // full decay
        assert_eq!(va, 350_000);
    }

    #[test]
    fn test_get_total_fee_at_with_decay() {
        // Full decay → only base fee
        let state = make_decay_state(
            200_000, 50_000, 8_388_608, 8_388_608, 30, 600, 5000, 350_000, 100_000, 1_000,
        );
        let fee_decayed = get_total_fee_at(&state, 1_700); // full decay, VA=0
                                                           // base_fee = 5000 * 20 * 1e10 = 1_000_000_000_000_000 = 1e15
        assert_eq!(fee_decayed, 1_000_000_000_000_000);

        // No decay (time_of_last_update=0 guard) → base + variable
        let state_no_decay = make_decay_state(
            200_000, 50_000, 8_388_608, 8_388_608, 30, 600, 5000, 350_000, 100_000,
            0, // guard: no decay
        );
        let fee_with_var = get_total_fee_at(&state_no_decay, 1_700);
        assert!(
            fee_with_var > fee_decayed,
            "Variable fee should add to base: decayed={}, with_var={}",
            fee_decayed,
            fee_with_var
        );
    }

    #[test]
    fn test_simulate_lb_swap_at_uses_explicit_timestamp() {
        let mut state = make_decay_state(
            200_000, 50_000, 8_388_608, 8_388_608, 30, 600, 5000, 350_000, 100_000, 1_000,
        );
        state.reserve_x = 1_000_000_000;
        state.reserve_y = 1_000_000_000;

        let amount_in = U256::from(100_000_000u64);
        let early = simulate_lb_swap_at(&state, amount_in, true, 1_000)
            .expect("swap should simulate")
            .0;
        let early_again = simulate_lb_swap_at(&state, amount_in, true, 1_000)
            .expect("swap should simulate")
            .0;
        let late = simulate_lb_swap_at(&state, amount_in, true, 1_700)
            .expect("swap should simulate")
            .0;

        assert_eq!(early, early_again, "same timestamp must be deterministic");
        assert!(
            late > early,
            "later chain timestamp should decay variable fee and improve output"
        );
    }

    #[test]
    fn test_compute_effective_va_no_variable_fee() {
        // variable_fee_control = 0 → fast path returns 0
        let state = make_decay_state(
            200_000, 50_000, 8_388_608, 8_388_608, 30, 600, 5000, 350_000, 0, // vfc = 0
            1_000,
        );
        assert_eq!(compute_effective_va(&state, 1_100), 0);
    }

    #[test]
    fn test_compute_effective_va_reduction_factor_zero() {
        // reduction_factor = 0, delta within [filter, decay) → vol_ref = 0
        // delta_id = 0 → new_va = 0
        let state = make_decay_state(
            200_000, 50_000, 8_388_608, 8_388_608, 30, 600,
            0, // reduction_factor = 0 → immediate full decay after filter
            350_000, 100_000, 1_000,
        );
        let va = compute_effective_va(&state, 1_100); // delta=100, within [30,600)
        assert_eq!(va, 0);
    }

    // =============================================================================
    // compute_post_swap_references() tests
    // =============================================================================

    #[test]
    fn test_references_within_filter_period() {
        // dt=10 < filter_period=30 → references unchanged
        let (vol_ref, id_ref) = compute_post_swap_references(
            8_388_610, // pre_swap_active_id
            200_000,   // stored_vol_acc
            50_000,    // stored_vol_ref
            8_388_608, // stored_id_ref
            1_000,     // stored_time_of_last_update
            1_010,     // block_timestamp (dt=10)
            30,        // filter_period
            600,       // decay_period
            5000,      // reduction_factor
        );
        assert_eq!(vol_ref, 50_000, "vol_ref unchanged within filter period");
        assert_eq!(id_ref, 8_388_608, "id_ref unchanged within filter period");
    }

    #[test]
    fn test_references_partial_decay() {
        // filter_period=30 <= dt=100 < decay_period=600
        // vol_ref = VA * reduction_factor / 10_000 = 200_000 * 5000 / 10_000 = 100_000
        // id_ref = pre_swap_active_id
        let (vol_ref, id_ref) = compute_post_swap_references(
            8_388_610, // pre_swap_active_id
            200_000,   // stored_vol_acc
            50_000,    // stored_vol_ref
            8_388_608, // stored_id_ref
            1_000,     // stored_time_of_last_update
            1_100,     // block_timestamp (dt=100)
            30,        // filter_period
            600,       // decay_period
            5000,      // reduction_factor
        );
        assert_eq!(vol_ref, 100_000, "vol_ref = VA * reduction / 10000");
        assert_eq!(id_ref, 8_388_610, "id_ref = pre_swap_active_id");
    }

    #[test]
    fn test_references_full_decay() {
        // dt=700 >= decay_period=600 → vol_ref=0
        let (vol_ref, id_ref) = compute_post_swap_references(
            8_388_610, // pre_swap_active_id
            200_000,   // stored_vol_acc
            50_000,    // stored_vol_ref
            8_388_608, // stored_id_ref
            1_000,     // stored_time_of_last_update
            1_700,     // block_timestamp (dt=700)
            30,        // filter_period
            600,       // decay_period
            5000,      // reduction_factor
        );
        assert_eq!(vol_ref, 0, "vol_ref = 0 after full decay");
        assert_eq!(id_ref, 8_388_610, "id_ref = pre_swap_active_id");
    }

    #[test]
    fn test_references_uninitialized_guard() {
        // time_of_last_update=0 → returns existing values unchanged
        let (vol_ref, id_ref) = compute_post_swap_references(
            8_388_610, // pre_swap_active_id
            200_000,   // stored_vol_acc
            50_000,    // stored_vol_ref
            8_388_608, // stored_id_ref
            0,         // stored_time_of_last_update (uninitialized)
            1_100,     // block_timestamp
            30,        // filter_period
            600,       // decay_period
            5000,      // reduction_factor
        );
        assert_eq!(vol_ref, 50_000, "vol_ref unchanged when uninitialized");
        assert_eq!(id_ref, 8_388_608, "id_ref unchanged when uninitialized");
    }

    #[test]
    fn test_references_reduction_factor_zero() {
        // reduction_factor=0, dt in [filter, decay) → vol_ref=0
        let (vol_ref, id_ref) = compute_post_swap_references(
            8_388_610, 200_000, 50_000, 8_388_608, 1_000, 1_100, // dt=100, within [30, 600)
            30, 600, 0, // reduction_factor = 0
        );
        assert_eq!(vol_ref, 0, "vol_ref = 0 when reduction_factor is 0");
        assert_eq!(id_ref, 8_388_610);
    }

    #[test]
    fn test_end_to_end_fee_parity() {
        // Full sequence: create state → compute references → apply event → verify
        // that the CORRECT references produce a DIFFERENT effective VA than the OLD
        // buggy values (vol_ref=event_VA, id_ref=post-swap).

        let pre_swap_active_id: u32 = 8_388_608;
        let post_swap_active_id: u32 = 8_388_610; // moved 2 bins
        let event_va: u32 = 250_000;
        let stored_va: u32 = 200_000;
        let stored_vol_ref: u32 = 50_000;
        let stored_id_ref: u32 = 8_388_607;
        let time_of_last_update: u64 = 1_000;
        let block_timestamp: u64 = 1_100; // dt=100, within [30, 600)
        let filter_period: u16 = 30;
        let decay_period: u16 = 600;
        let reduction_factor: u16 = 5000;

        // Correct: replay updateReferences
        let (correct_vol_ref, correct_id_ref) = compute_post_swap_references(
            pre_swap_active_id,
            stored_va,
            stored_vol_ref,
            stored_id_ref,
            time_of_last_update,
            block_timestamp,
            filter_period,
            decay_period,
            reduction_factor,
        );

        // correct_vol_ref = stored_va * reduction / 10000 = 200000 * 5000 / 10000 = 100000
        assert_eq!(correct_vol_ref, 100_000);
        // correct_id_ref = pre_swap_active_id
        assert_eq!(correct_id_ref, pre_swap_active_id);

        // OLD buggy values would have been:
        let buggy_vol_ref = event_va; // 250_000 (too high!)
        let buggy_id_ref = post_swap_active_id; // 8_388_610 (wrong bin!)

        // Verify they differ
        assert_ne!(
            correct_vol_ref, buggy_vol_ref,
            "Correct vol_ref should differ from buggy"
        );
        assert_ne!(
            correct_id_ref, buggy_id_ref,
            "Correct id_ref should differ from buggy"
        );

        // Build state with CORRECT references and verify compute_effective_va
        let state_correct = make_decay_state(
            event_va,            // volatility_accumulator (from event)
            correct_vol_ref,     // volatility_reference (from updateReferences)
            correct_id_ref,      // id_reference (from updateReferences)
            post_swap_active_id, // active_id (from event)
            filter_period,
            decay_period,
            reduction_factor,
            350_000,         // max_va
            100_000,         // vfc
            block_timestamp, // time_of_last_update
        );

        // Build state with BUGGY references
        let state_buggy = make_decay_state(
            event_va,
            buggy_vol_ref, // WRONG: event VA used as vol_ref
            buggy_id_ref,  // WRONG: post-swap active_id used as id_ref
            post_swap_active_id,
            filter_period,
            decay_period,
            reduction_factor,
            350_000,
            100_000,
            block_timestamp,
        );

        // At a future timestamp (dt=50, within filter period so references unchanged),
        // the effective VA should differ because vol_ref and id_ref are different
        let future_ts = block_timestamp + 50;
        let va_correct = compute_effective_va(&state_correct, future_ts);
        let va_buggy = compute_effective_va(&state_buggy, future_ts);

        // id_ref difference: correct uses pre_swap (8_388_608), buggy uses post_swap (8_388_610)
        // delta_id for correct: |8_388_610 - 8_388_608| = 2
        // delta_id for buggy:   |8_388_610 - 8_388_610| = 0
        // This means buggy underestimates the id component contribution
        assert_ne!(
            va_correct, va_buggy,
            "Correct and buggy effective VAs should differ: correct={}, buggy={}",
            va_correct, va_buggy
        );
    }
}

/// K2: randomized property tests for the Trader Joe LB (bin-liquidity) kernel.
/// Small in-bin swaps: no panic; output monotonic in input.
#[cfg(test)]
mod lb_property {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn sample() -> LBState {
        LBState {
            active_id: 8_388_608,
            bin_step: 20,
            reserve_x: 1_000_000_000_000_000_000,
            reserve_y: 1_000_000_000_000_000_000,
            base_factor: 5000,
            filter_period: 0,
            decay_period: 0,
            reduction_factor: 0,
            variable_fee_control: 0,
            protocol_share: 0,
            max_volatility_accumulator: 0,
            volatility_accumulator: 0,
            volatility_reference: 0,
            id_reference: 0,
            time_of_last_update: 0,
            block_number: 0,
            log_index: 0,
            stale: false,
            bin_map: Arc::new(BTreeMap::new()),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1200))]

        #[test]
        fn monotonic_in_amount(
            a in 1u64..=500_000u64,
            bump in 1u64..=500_000u64,
            zero_for_one in any::<bool>(),
        ) {
            let s = sample();
            let o1 = simulate_lb_swap(&s, U256::from(a), zero_for_one);
            let o2 = simulate_lb_swap(&s, U256::from(a + bump), zero_for_one);
            if let (Some((out1, _)), Some((out2, _))) = (o1, o2) {
                prop_assert!(out2 >= out1, "more in ⇒ not less out ({out1} → {out2})");
            }
        }
    }
}

#[cfg(test)]
mod lb_price_memo_tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn memo_hits_and_keeps_distinct_keys_byte_exact() {
        let a1 = get_price_from_id(8_388_650, 20);
        let a2 = get_price_from_id(8_388_650, 20); // cache hit
        assert_eq!(a1, a2);
        assert_eq!(a1, get_price_from_id_uncached(8_388_650, 20));
        // a distinct key must not be contaminated by the cached one
        let b = get_price_from_id(8_388_650, 25);
        assert_eq!(b, get_price_from_id_uncached(8_388_650, 25));
        assert_ne!(a1, b);
        // negative exponent (id below center) is also exact
        let c = get_price_from_id(8_388_000, 15);
        assert_eq!(c, get_price_from_id_uncached(8_388_000, 15));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]
        /// THE fast-lane guard: the memo returns the EXACT pow value for every `(id, bin_step)`,
        /// across cache hits AND direct-mapped collisions.
        #[test]
        fn memo_is_byte_exact_vs_pow(
            id in 8_000_000u32..=8_800_000u32,
            bs in prop::sample::select(vec![1u16, 2, 5, 10, 15, 20, 25, 50, 100]),
        ) {
            prop_assert_eq!(get_price_from_id(id, bs), get_price_from_id_uncached(id, bs));
        }
    }

    /// Manual micro-bench: `cargo test -p engine-amm --release lb_memo_speedup -- --ignored --nocapture`.
    /// Models the sizing search re-walking ~40 bins ~200× (each sizing quote re-walks the bins).
    #[test]
    #[ignore = "manual micro-bench"]
    fn lb_memo_speedup() {
        let ids: Vec<u32> = (8_388_580u32..8_388_620).collect(); // ~40 active bins
        let reps = 200usize;
        for &id in &ids {
            let _ = get_price_from_id(id, 20); // warm
        }
        let t0 = std::time::Instant::now();
        let mut acc = U256::ZERO;
        for _ in 0..reps {
            for &id in &ids {
                acc = acc.wrapping_add(get_price_from_id(id, 20));
            }
        }
        let cached = t0.elapsed();
        let t1 = std::time::Instant::now();
        for _ in 0..reps {
            for &id in &ids {
                acc = acc.wrapping_add(get_price_from_id_uncached(id, 20));
            }
        }
        let uncached = t1.elapsed();
        std::hint::black_box(acc);
        eprintln!(
            "LB price memo over {} calls: cached={:?} uncached={:?} -> {:.1}x",
            reps * ids.len(),
            cached,
            uncached,
            uncached.as_secs_f64() / cached.as_secs_f64().max(1e-12)
        );
    }
}

#[cfg(test)]
mod fee_overflow_tests {
    //! AUD-LB-FEE: `get_fee_amount{,_from}` must match a 256-bit reference at ANY u128 amount.
    //! The raw u128 `amount * total_fee` wraps for amount > ~3.4e21 wei at the 10% fee cap
    //! (release: wrapped product → under-charged fee → OVER-QUOTE; debug: panic).
    use super::*;

    const ONE_E18: u128 = 1_000_000_000_000_000_000;

    /// ceil(amount * total_fee / 1e18) computed in 256-bit (never wraps).
    fn fee_from_u256_ref(amount: u128, total_fee: u128) -> u128 {
        let num = U256::from(amount) * U256::from(total_fee);
        let den = U256::from(ONE_E18);
        u128::try_from((num + den - U256::from(1u8)) / den).expect("fee <= amount/10 fits u128")
    }

    /// ceil(amount * total_fee / (1e18 - total_fee)) computed in 256-bit.
    fn fee_on_top_u256_ref(amount: u128, total_fee: u128) -> u128 {
        let num = U256::from(amount) * U256::from(total_fee);
        let den = U256::from(ONE_E18 - total_fee);
        u128::try_from((num + den - U256::from(1u8)) / den).expect("fee <= amount/9 fits u128")
    }

    #[test]
    fn lb_fee_amount_matches_u256_reference_at_whale_size() {
        // 10%-cap fee × 4e21 wei (≈4000 tokens in one bin): u128 product wraps.
        let total_fee = 100_000_000_000_000_000u128; // 1e17 = the 10% cap
        let amount = 4_000_000_000_000_000_000_000u128; // 4e21 > 2^128/1e17
        assert_eq!(
            get_fee_amount_from(amount, total_fee),
            fee_from_u256_ref(amount, total_fee),
            "get_fee_amount_from under-charges past the u128 wrap point"
        );
        assert_eq!(
            get_fee_amount(amount, total_fee),
            fee_on_top_u256_ref(amount, total_fee),
            "get_fee_amount under-charges past the u128 wrap point"
        );
    }

    #[test]
    fn lb_fee_amount_small_values_byte_identical_to_reference() {
        // Regression guard: realistic sizes must stay exactly as before the widening.
        let total_fee = 3_000_000_000_000_000u128; // 0.3%
        for amount in [1u128, 1_000, ONE_E18 / 2, ONE_E18, 5 * ONE_E18, 100 * ONE_E18] {
            assert_eq!(get_fee_amount_from(amount, total_fee), fee_from_u256_ref(amount, total_fee));
            assert_eq!(get_fee_amount(amount, total_fee), fee_on_top_u256_ref(amount, total_fee));
        }
    }
}
