//! Sizing kernels per §10.7.
//!
//! For V2/V2 routes:
//! * `grid_search_v2v2` — exact integer grid search over an AmountGrid.
//! * `bounded_golden_section_v2v2` — exact integer golden-section search inside the bracket
//!   returned by the grid scan.
//!
//! For V3/V3 routes:
//! * `grid_search_v3v3` — exact integer grid search using the V3 swap kernel for each
//!   hop. Uses the resident `V3Slot0` and warmed tick window per pool.
//!
//! Mixed V2/V3 routes use the per-family kernel for each hop directly via the route
//! evaluator in `engine`; this module only ships the common closed-cycle helpers.

use alloy_primitives::U256;
use engine_amm::{v2_get_amount_out, v3_get_amount_out, TickInfo, V3Slot0};

/// Output of a sizing call.
#[derive(Debug, Clone, Copy)]
pub struct SizingOutcome {
    /// Selected input.
    pub amount_in: u128,
    /// Resulting output.
    pub amount_out: u128,
    /// Gross profit in base-token units (amount_out - amount_in if positive, else 0).
    pub gross_profit: u128,
    /// Number of iterations consumed.
    pub iterations: u32,
}

/// Scan a fixed amount grid through a V3/V3 two-hop cycle. Returns the best bucket and its
/// outcome, or `None` if no bucket is profitable.
///
/// `slot0_a` / `ticks_a` are the first pool's state + warm tick window; `slot0_b` /
/// `ticks_b` are the second pool's. `zero_for_one_a` / `zero_for_one_b` select the swap
/// direction per hop.
pub fn grid_search_v3v3(
    grid: &[u128],
    slot0_a: V3Slot0,
    ticks_a: &[TickInfo],
    zero_for_one_a: bool,
    slot0_b: V3Slot0,
    ticks_b: &[TickInfo],
    zero_for_one_b: bool,
) -> Option<(usize, SizingOutcome)> {
    let mut best: Option<(usize, SizingOutcome)> = None;
    for (i, &amount) in grid.iter().enumerate() {
        if amount == 0 {
            continue;
        }
        let amount_in = U256::from(amount);
        let Ok(mid) = v3_get_amount_out(slot0_a, amount_in, zero_for_one_a, ticks_a) else {
            continue;
        };
        if mid.amount_out.is_zero() {
            continue;
        }
        let Ok(out) = v3_get_amount_out(slot0_b, mid.amount_out, zero_for_one_b, ticks_b) else {
            continue;
        };
        if out.amount_out <= amount_in {
            continue;
        }
        let profit = out.amount_out - amount_in;
        // Convert to u128 for the SizingOutcome (fits as long as we stay below 2^128).
        let amount_in_u =
            amount_in.as_limbs()[0] as u128 | ((amount_in.as_limbs()[1] as u128) << 64);
        let amount_out_u =
            out.amount_out.as_limbs()[0] as u128 | ((out.amount_out.as_limbs()[1] as u128) << 64);
        let profit_u = profit.as_limbs()[0] as u128 | ((profit.as_limbs()[1] as u128) << 64);
        let candidate = SizingOutcome {
            amount_in: amount_in_u,
            amount_out: amount_out_u,
            gross_profit: profit_u,
            iterations: 1,
        };
        match best {
            None => best = Some((i, candidate)),
            Some((_, b)) if candidate.gross_profit > b.gross_profit => best = Some((i, candidate)),
            _ => {}
        }
    }
    best
}

/// Scan a fixed amount grid through a V2/V2 two-hop cycle. Returns the best bucket and its
/// (input, output, gross_profit). Returns `None` if no bucket is profitable.
pub fn grid_search_v2v2(
    grid: &[u128],
    r_in_1: u128,
    r_out_1: u128,
    fee_bps_1: u32,
    r_in_2: u128,
    r_out_2: u128,
    fee_bps_2: u32,
) -> Option<(usize, SizingOutcome)> {
    let mut best: Option<(usize, SizingOutcome)> = None;
    for (i, &amount) in grid.iter().enumerate() {
        if amount == 0 {
            continue;
        }
        let Ok(mid) = v2_get_amount_out(amount, r_in_1, r_out_1, fee_bps_1) else {
            continue;
        };
        let Ok(out) = v2_get_amount_out(mid, r_in_2, r_out_2, fee_bps_2) else {
            continue;
        };
        if out <= amount {
            continue;
        }
        let profit = out - amount;
        let candidate = SizingOutcome {
            amount_in: amount,
            amount_out: out,
            gross_profit: profit,
            iterations: 1,
        };
        match best {
            None => best = Some((i, candidate)),
            Some((_, b)) if candidate.gross_profit > b.gross_profit => best = Some((i, candidate)),
            _ => {}
        }
    }
    best
}

// Golden ratio reciprocal scaled to fixed-point Q32:
//   phi_inv = 0.618033988... * 2^32 ≈ 2654435769
const PHI_INV_Q32: u128 = 2_654_435_769;
const ONE_Q32: u128 = 1u128 << 32;

/// The golden-section interior step `(b - a) * PHI_INV_Q32 / ONE_Q32`, computed through a U256
/// intermediate. The naive `u128` product overflows once `(b - a) > ~2^96` (panic in debug, wrap
/// in release); widening the multiply keeps it exact. The quotient is `(b - a) * 0.618…`, always
/// `< (b - a) <= u128::MAX`, so it fits back in `u128` (the saturating fallback never triggers).
#[inline]
fn golden_step(a: u128, b: u128) -> u128 {
    debug_assert!(b >= a);
    let width = b.saturating_sub(a);
    // FAST PATH (the overwhelmingly common case): `width * PHI_INV_Q32` fits u128 whenever
    // `width < 2^128 / PHI_INV_Q32 ≈ 2^96.7`. Real V2 reserves/amounts are u128 token units far
    // below that, so this stays a single 128-bit multiply — no U256 on the sizing path.
    match width.checked_mul(PHI_INV_Q32 as u128) {
        Some(p) => p / ONE_Q32 as u128,
        // OVERFLOW GUARD: only a pathological bracket width > ~2^96 widens to U256 (vs the prior
        // unchecked `*` which panicked in debug / wrapped in release).
        None => u128::try_from(
            U256::from(width).saturating_mul(U256::from(PHI_INV_Q32)) / U256::from(ONE_Q32),
        )
        .unwrap_or(u128::MAX),
    }
}

/// Golden-section search refinement inside a bracket `[lo, hi]`. Iteration cap is bounded
/// by `max_iter`. Profit is the V2/V2 two-hop gross profit.
pub fn bounded_golden_section_v2v2(
    lo: u128,
    hi: u128,
    r_in_1: u128,
    r_out_1: u128,
    fee_bps_1: u32,
    r_in_2: u128,
    r_out_2: u128,
    fee_bps_2: u32,
    max_iter: u32,
) -> Option<SizingOutcome> {
    if lo >= hi {
        return None;
    }
    let mut a = lo;
    let mut b = hi;
    let mut x1 = b - golden_step(a, b);
    let mut x2 = a + golden_step(a, b);
    let (mut o1, mut s1) =
        signed_outcome_v2v2(x1, r_in_1, r_out_1, fee_bps_1, r_in_2, r_out_2, fee_bps_2);
    let (mut o2, mut s2) =
        signed_outcome_v2v2(x2, r_in_1, r_out_1, fee_bps_1, r_in_2, r_out_2, fee_bps_2);
    let mut iter = 0u32;
    while iter < max_iter && (b > a + 1) {
        if s1 > s2 {
            b = x2;
            x2 = x1;
            o2 = o1;
            s2 = s1;
            x1 = b - golden_step(a, b);
            let (no, ns) =
                signed_outcome_v2v2(x1, r_in_1, r_out_1, fee_bps_1, r_in_2, r_out_2, fee_bps_2);
            o1 = no;
            s1 = ns;
        } else {
            a = x1;
            x1 = x2;
            o1 = o2;
            s1 = s2;
            x2 = a + golden_step(a, b);
            let (no, ns) =
                signed_outcome_v2v2(x2, r_in_1, r_out_1, fee_bps_1, r_in_2, r_out_2, fee_bps_2);
            o2 = no;
            s2 = ns;
        }
        iter += 1;
    }
    let best = if s1 >= s2 { o1 } else { o2 };
    if best.gross_profit == 0 {
        // Probe a couple of evenly-spaced candidates inside the final bracket as a last
        // resort — golden-section can finish in a flat-zero plateau even though a tiny
        // positive bucket exists between `a` and `b`.
        let mut best_profit = 0u128;
        let mut best_outcome = best;
        for k in 1..=8 {
            let mid_amt = a + (b - a) * (k as u128) / 9;
            let o = profit_v2v2(
                mid_amt, r_in_1, r_out_1, fee_bps_1, r_in_2, r_out_2, fee_bps_2,
            );
            if o.gross_profit > best_profit {
                best_profit = o.gross_profit;
                best_outcome = o;
            }
        }
        if best_profit == 0 {
            return None;
        }
        return Some(SizingOutcome {
            amount_in: best_outcome.amount_in,
            amount_out: best_outcome.amount_out,
            gross_profit: best_profit,
            iterations: iter,
        });
    }
    Some(SizingOutcome {
        amount_in: best.amount_in,
        amount_out: best.amount_out,
        gross_profit: best.gross_profit,
        iterations: iter,
    })
}

/// Closed-form V2/V2 optimal input (`engine_amm::scythe::solve_optimal_v2_two_hop`) clamped to
/// `[lo, hi]`, then a ±4-wei exact-profit polish. Mirrors the engine's production `evaluate_two_hop`
/// path; benched against [`bounded_golden_section_v2v2`] to quantify the sizing speedup. Returns
/// `None` when the cycle is unprofitable / the closed form can't solve.
pub fn closed_form_v2v2(
    lo: u128,
    hi: u128,
    r_in_1: u128,
    r_out_1: u128,
    fee_bps_1: u32,
    r_in_2: u128,
    r_out_2: u128,
    fee_bps_2: u32,
) -> Option<SizingOutcome> {
    use alloy_primitives::U256;
    let x = engine_amm::scythe::solve_optimal_v2_two_hop(
        U256::from(r_in_1),
        U256::from(r_out_1),
        fee_bps_1,
        U256::from(r_in_2),
        U256::from(r_out_2),
        fee_bps_2,
    )?;
    let x = u128::try_from(x).ok()?.clamp(lo, hi);
    let mut best: Option<SizingOutcome> = None;
    for d in 0..=4u128 {
        for cand in [x.saturating_sub(d).max(lo), x.saturating_add(d).min(hi)] {
            let o = profit_v2v2(cand, r_in_1, r_out_1, fee_bps_1, r_in_2, r_out_2, fee_bps_2);
            if o.gross_profit > 0
                && best
                    .as_ref()
                    .is_none_or(|b| o.gross_profit > b.gross_profit)
            {
                best = Some(o);
            }
        }
    }
    best
}

/// Signed profit used by the golden-section comparator so it can climb back toward the
/// peak even when both probes are on the "after-peak" side of the bell curve.
fn signed_profit_v2v2(
    amount: u128,
    r_in_1: u128,
    r_out_1: u128,
    fee_bps_1: u32,
    r_in_2: u128,
    r_out_2: u128,
    fee_bps_2: u32,
) -> (u128, u128, i128) {
    if amount == 0 {
        return (0, 0, 0);
    }
    let mid = v2_get_amount_out(amount, r_in_1, r_out_1, fee_bps_1).unwrap_or(0);
    let out = v2_get_amount_out(mid, r_in_2, r_out_2, fee_bps_2).unwrap_or(0);
    let signed = (out as i128) - (amount as i128);
    (mid, out, signed)
}

fn profit_v2v2(
    amount: u128,
    r_in_1: u128,
    r_out_1: u128,
    fee_bps_1: u32,
    r_in_2: u128,
    r_out_2: u128,
    fee_bps_2: u32,
) -> SizingOutcome {
    let (_mid, out, signed) = signed_profit_v2v2(
        amount, r_in_1, r_out_1, fee_bps_1, r_in_2, r_out_2, fee_bps_2,
    );
    let profit = if signed > 0 { signed as u128 } else { 0 };
    SizingOutcome {
        amount_in: amount,
        amount_out: out,
        gross_profit: profit,
        iterations: 0,
    }
}

/// Signed-profit variant used by the golden-section search.
fn signed_outcome_v2v2(
    amount: u128,
    r_in_1: u128,
    r_out_1: u128,
    fee_bps_1: u32,
    r_in_2: u128,
    r_out_2: u128,
    fee_bps_2: u32,
) -> (SizingOutcome, i128) {
    let (_mid, out, signed) = signed_profit_v2v2(
        amount, r_in_1, r_out_1, fee_bps_1, r_in_2, r_out_2, fee_bps_2,
    );
    let profit = if signed > 0 { signed as u128 } else { 0 };
    (
        SizingOutcome {
            amount_in: amount,
            amount_out: out,
            gross_profit: profit,
            iterations: 0,
        },
        signed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_search_finds_profit() {
        // Set up a clearly profitable cycle (see engine-detect filters test).
        let grid = vec![1_000u128, 10_000, 100_000, 1_000_000];
        let r = grid_search_v2v2(
            &grid,
            1_000_000_000u128,
            4_000_000_000u128,
            30,
            4_000_000_000u128,
            1_010_000_000u128,
            30,
        );
        assert!(r.is_some());
    }

    /// A bracket wider than ~2^96 makes the naive `(b - a) * PHI_INV_Q32` overflow u128 (panic in
    /// debug, wrap in release). The golden-section split must widen the intermediate (U256) so a
    /// huge bracket neither panics nor wraps, and still returns a sane outcome (or None).
    #[test]
    fn golden_section_huge_bracket_does_not_overflow() {
        let hi = 1u128 << 100; // (hi - lo) >> 2^96
        let outcome = bounded_golden_section_v2v2(
            1u128,
            hi,
            1_000_000_000u128,
            4_000_000_000u128,
            30,
            4_000_000_000u128,
            1_010_000_000u128,
            30,
            32,
        );
        // The contract is "no panic / no wrap, sane result". If profitable, the chosen input must
        // lie inside the bracket; either way amount_in <= amount_out is not required (gross may be 0).
        if let Some(o) = outcome {
            assert!(
                o.amount_in <= hi,
                "selected input must stay within the bracket"
            );
        }
    }

    #[test]
    fn golden_section_refines_grid() {
        // Search inside a wide bracket; should find profit if the grid scan found any.
        let outcome = bounded_golden_section_v2v2(
            1u128,
            10_000_000u128,
            1_000_000_000u128,
            4_000_000_000u128,
            30,
            4_000_000_000u128,
            1_010_000_000u128,
            30,
            32,
        );
        assert!(outcome.is_some());
        let o = outcome.unwrap();
        assert!(o.gross_profit > 0);
    }
}
