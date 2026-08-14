//! §10.6 negative-filter cascade.
//!
//! Generation freshness, whitelist mask, marginal-rate product, amount-grid scan, depth cap.
//! Each filter records a u8 reason code on rejection.

use alloy_primitives::U256;
use engine_amm::v2_get_amount_out;

/// One-byte reason code emitted on rejection.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterReason {
    /// Route passed all filters.
    Accepted = 0,
    /// State generation changed during evaluation.
    StaleGeneration = 1,
    /// Risk-flag whitelist mismatch.
    WhitelistMismatch = 2,
    /// Marginal-rate product fell below the cost floor.
    MarginalRateInsufficient = 3,
    /// Amount-grid scan saw no positive bucket.
    AmountGridNoCandidate = 4,
    /// Selected amount exceeded the route's depth cap.
    DepthCapExceeded = 5,
    /// Confidence chain rejected (kept here so callers can record a single reason).
    ConfidenceTooLow = 6,
    /// V3 pool has zero in-range liquidity — no swap possible without crossing a tick
    /// boundary the snapshot doesn't cover. Common when ticks weren't refreshed
    /// recently or when a pool genuinely has no LPs near the current price.
    ZeroLiquidity = 7,
    /// Cycle shape failed the evaluator's defence-in-depth family check (e.g. a triangle
    /// reached the V2-only triangle evaluator with one V3 pool). These should never
    /// happen if the CSR dispatch tagging is correct; if they fire frequently the
    /// dispatcher and enumerator have drifted out of sync.
    UnsupportedShape = 8,
    /// A hop's pool has a parameter-poison flag set (admin mutated tickSpacing/fee/stable
    /// mode at runtime — 2026-07-10 guard): the resident kernel parameters can no longer be
    /// trusted, so the route is refused fail-closed until resync repairs (SOFT) or reboot (HARD).
    ParamPoisoned = 9,
    /// WI-02b: a hop's family is INACTIVE under the boot-sealed `RuntimePolicy`. Recorded at
    /// the one evaluation-dispatch seam every production evaluator passes, so an inactive
    /// route (stale table, injected, or cache-resident) can never be priced by the
    /// specialized V2/V3/DODO/Solidly/Fluid kernels either — boundary 5 is universal.
    FamilyInactive = 10,
}

/// Filter result alias.
pub type FilterResult = Result<(), FilterReason>;

/// Compute the marginal-rate product for a V2/V2 two-hop route.
///
/// "Marginal rate" = `dy/dx` at the current reserve point. For Uniswap V2 with reserves
/// `(R_in, R_out)` and fee bps `f`, the marginal exchange rate is `R_out * (1-f) / R_in`.
/// The cycle product must exceed `(1 + cost_floor_bps / 10_000)` to be worth evaluating.
///
/// Implemented over `U256` because reserves on Arbitrum routinely exceed `2^64` (WETH
/// pools hold billions of wei × 1e18 per ETH = > 1e27), and the cross-product
/// `r_in × r_out × 10_000^2` reaches well past `2^128`.
pub fn marginal_rate_product_v2v2(
    r_in_1: u128,
    r_out_1: u128,
    fee_bps_1: u32,
    r_in_2: u128,
    r_out_2: u128,
    fee_bps_2: u32,
    cost_floor_bps: u32,
) -> FilterResult {
    if r_in_1 == 0 || r_out_1 == 0 || r_in_2 == 0 || r_out_2 == 0 {
        return Err(FilterReason::MarginalRateInsufficient);
    }
    // C4 fail-closed guard: a fee at or above 100% (10_000 bps) leaves no value after fees and,
    // more importantly, would underflow `10_000 - fee_bps` (panic in debug, wrap in release).
    // Reject rather than risk a wrapped fee factor producing a spurious PASS.
    if fee_bps_1 >= 10_000 || fee_bps_2 >= 10_000 {
        return Err(FilterReason::MarginalRateInsufficient);
    }
    // Strict comparison after fees on each hop, using U256 throughout to avoid overflow:
    //   r_out_1 * (10_000 - f1) * r_out_2 * (10_000 - f2) > r_in_1 * r_in_2 *
    //   10_000^2 * (10_000 + cost_floor_bps) / 10_000
    // Multiplying everything by 10_000 to clear the fraction gives:
    //   r_out_1 * (10_000 - f1) * r_out_2 * (10_000 - f2) * 10_000 >
    //   r_in_1 * r_in_2 * 10_000^2 * (10_000 + cost_floor_bps)
    let fee_factor_1 = U256::from(10_000u64 - fee_bps_1 as u64);
    let fee_factor_2 = U256::from(10_000u64 - fee_bps_2 as u64);
    let ten_thousand = U256::from(10_000u64);
    // C5: `saturating_mul` here is the INTENDED fail-closed behavior. The only way these products
    // saturate `U256::MAX` is with unreachably-large reserves (≫ total token supply). If `lhs`
    // saturates it can only make `lhs > rhs` more likely — so to stay fail-closed we accept that a
    // saturated `rhs` (the input-side product) clamps to MAX and forces the route to be REJECTED
    // (`lhs > rhs` is false when both are MAX), never spuriously passed. No behavior change.
    let lhs = U256::from(r_out_1)
        .saturating_mul(fee_factor_1)
        .saturating_mul(U256::from(r_out_2))
        .saturating_mul(fee_factor_2)
        .saturating_mul(ten_thousand);
    let rhs = U256::from(r_in_1)
        .saturating_mul(U256::from(r_in_2))
        .saturating_mul(ten_thousand)
        .saturating_mul(ten_thousand)
        .saturating_mul(U256::from(10_000u64 + cost_floor_bps as u64));
    if lhs > rhs {
        Ok(())
    } else {
        Err(FilterReason::MarginalRateInsufficient)
    }
}

/// Marginal-rate product for a V3/V3 two-hop cycle, computed from each pool's
/// `sqrtPriceX96` rather than reserves. The cycle's pre-fee marginal price ratio is
/// `(sqrt_b / sqrt_a)^2` per pool; cycle product across both hops must exceed
/// `(1 + cost_floor_bps / 10_000)` after fees. Implemented in `U256` to avoid the
/// 256-bit overflow that bit `marginal_rate_product_v2v2` for full-decimal reserves.
///
/// For hop direction `zero_for_one_a / zero_for_one_b`, the effective marginal rate of
/// the hop is `(sqrtA1 / sqrtA0)^2` flipped if `zero_for_one`. We work in the unflipped
/// representation `(token1 / token0)` to keep arithmetic uniform, and apply the
/// direction by inverting one of the squared ratios.
pub fn marginal_rate_product_v3v3(
    sqrt_a: alloy_primitives::U256,
    fee_pips_a: u32,
    zero_for_one_a: bool,
    sqrt_b: alloy_primitives::U256,
    fee_pips_b: u32,
    zero_for_one_b: bool,
    cost_floor_bps: u32,
) -> FilterResult {
    use alloy_primitives::U256;
    use ruint::aliases::U512;
    if sqrt_a.is_zero() || sqrt_b.is_zero() {
        return Err(FilterReason::MarginalRateInsufficient);
    }
    // C4 fail-closed guard: a V3 fee at or above 100% (1_000_000 pips) would underflow
    // `1_000_000 - fee_pips` (panic in debug, wrap in release). Reject rather than risk a wrapped
    // fee factor producing a spurious PASS.
    if fee_pips_a >= 1_000_000 || fee_pips_b >= 1_000_000 {
        return Err(FilterReason::MarginalRateInsufficient);
    }
    // For each hop, output/input marginal rate (pre-fee, in 18-decimal-equivalent units)
    // is `(sqrtP / 2^96)^2` if direction is one_for_zero (output is token0, input is
    // token1) — but actually we want to *check the cycle return*, not directly. For a
    // closed cycle the cross-fee math collapses to:
    //   cycle_return = (1 - fee_a/1e6) * (1 - fee_b/1e6) * (price_a_norm * inv_price_b_norm)
    //
    // Here price_a_norm = (sqrt_a)^2 if zero_for_one_a==true means we sell token0 → get
    // token1 at price (sqrt_a/2^96)^2. The return leg sells token1 → token0 at
    // (1 / ((sqrt_b/2^96)^2)). Combined ratio:
    //   r = (sqrt_a^2 / sqrt_b^2)   if zero_for_one_a == true and zero_for_one_b == false
    //   r = (sqrt_b^2 / sqrt_a^2)   if zero_for_one_a == false and zero_for_one_b == true
    // (other direction combinations don't close the cycle).
    //
    // The fee factor is `(1e6 - fee_pips_a) * (1e6 - fee_pips_b) / 1e12`.
    //
    // We require r * fee_factor > (1 + cost_floor_bps / 10_000).
    let (num_sqrt, den_sqrt) = match (zero_for_one_a, zero_for_one_b) {
        (true, false) => (sqrt_a, sqrt_b),
        (false, true) => (sqrt_b, sqrt_a),
        _ => return Err(FilterReason::MarginalRateInsufficient),
    };
    // (num/den)^2 * fee_factor > (1 + cost_floor_bps/10000)
    // <=> num^2 * (1e6 - fee_a) * (1e6 - fee_b) * 10_000 >
    //     den^2 * 1e12 * (10_000 + cost_floor_bps)
    let num = U512::from(num_sqrt);
    let den = U512::from(den_sqrt);
    let num_sq: U512 = num * num;
    let den_sq: U512 = den * den;
    let fee_a = U512::from(1_000_000u64 - fee_pips_a as u64);
    let fee_b = U512::from(1_000_000u64 - fee_pips_b as u64);
    let ten_k = U512::from(10_000u64);
    let one_e12 = U512::from(1_000_000_000_000u64);
    // C5: `saturating_mul` is the INTENDED fail-closed behavior. Saturation requires
    // unreachably-large `sqrtP` values (a U512 squared product near U512::MAX); should it occur, a
    // clamped `rhs` (input side) at MAX forces `lhs > rhs` to be false → route REJECTED, never a
    // spurious pass. No behavior change.
    let lhs = num_sq
        .saturating_mul(fee_a)
        .saturating_mul(fee_b)
        .saturating_mul(ten_k);
    let rhs = den_sq
        .saturating_mul(one_e12)
        .saturating_mul(U512::from(10_000u64 + cost_floor_bps as u64));
    let _ = U256::ZERO;
    if lhs > rhs {
        Ok(())
    } else {
        Err(FilterReason::MarginalRateInsufficient)
    }
}

/// Diagnostic variant of `marginal_rate_product_v3v3` — returns the cycle return
/// after fees as a Q32.32 fixed-point ratio (1.0 = `1 << 32`). Used to instrument
/// "why no candidates": when the engine sees real dislocations the operator can
/// confirm cycle returns are in the 0.99x range (just under 1.0 — fees eat any
/// dislocation) versus genuinely above 1.0 (which would emit a candidate).
///
/// Returns `None` for invalid inputs (zero sqrt prices, wrong direction pairing,
/// or arithmetic overflow). Calls into the same U512 math as the pass/fail filter
/// so the diagnostic value is exactly the value the filter compares against.
pub fn marginal_rate_ratio_q32_v3v3(
    sqrt_a: alloy_primitives::U256,
    fee_pips_a: u32,
    zero_for_one_a: bool,
    sqrt_b: alloy_primitives::U256,
    fee_pips_b: u32,
    zero_for_one_b: bool,
) -> Option<u64> {
    use ruint::aliases::U512;
    if sqrt_a.is_zero() || sqrt_b.is_zero() {
        return None;
    }
    // C4 fail-closed guard: a fee at or above 100% (1_000_000 pips) would underflow
    // `1_000_000 - fee_pips`. Treat as invalid input (return None) like the other guards.
    if fee_pips_a >= 1_000_000 || fee_pips_b >= 1_000_000 {
        return None;
    }
    let (num_sqrt, den_sqrt) = match (zero_for_one_a, zero_for_one_b) {
        (true, false) => (sqrt_a, sqrt_b),
        (false, true) => (sqrt_b, sqrt_a),
        _ => return None,
    };
    let num = U512::from(num_sqrt);
    let den = U512::from(den_sqrt);
    let num_sq: U512 = num * num;
    let den_sq: U512 = den * den;
    let fee_a = U512::from(1_000_000u64 - fee_pips_a as u64);
    let fee_b = U512::from(1_000_000u64 - fee_pips_b as u64);
    let one_e12 = U512::from(1_000_000_000_000u64);
    // ratio_q32 = (num^2 * fee_a * fee_b * (1<<32)) / (den^2 * 1e12)
    let one_shift_32 = U512::from(1u64 << 32);
    let numerator = num_sq
        .saturating_mul(fee_a)
        .saturating_mul(fee_b)
        .saturating_mul(one_shift_32);
    let denominator = den_sq.saturating_mul(one_e12);
    if denominator.is_zero() {
        return None;
    }
    let r = numerator / denominator;
    // Clamp to u64; ratios > 2^32 (i.e. cycle return > 1) are interesting in
    // their full precision but for the diagnostic dump u64 is plenty.
    let limbs = r.as_limbs();
    if limbs[1] != 0 || limbs[2] != 0 || limbs[3] != 0 {
        Some(u64::MAX)
    } else {
        Some(limbs[0])
    }
}

/// Pre-evaluation gates per §10.6:
/// * generation freshness
/// * whitelist mask
/// * marginal-rate product (passed in by the worker as a precomputed boolean)
/// * amount-grid scan via the supplied `amount_grid` closure
/// * depth cap
pub fn route_passes_filters(
    expected_generations: &[u64],
    current_generations: &[u64],
    risk_flags: u64,
    required_mask: u64,
    forbidden_mask: u64,
    marginal_product_ok: bool,
    has_positive_bucket: bool,
    selected_amount_within_cap: bool,
) -> FilterResult {
    if expected_generations.len() != current_generations.len() {
        return Err(FilterReason::StaleGeneration);
    }
    for (e, c) in expected_generations.iter().zip(current_generations.iter()) {
        if e != c {
            return Err(FilterReason::StaleGeneration);
        }
    }
    if (risk_flags & required_mask) != required_mask {
        return Err(FilterReason::WhitelistMismatch);
    }
    if (risk_flags & forbidden_mask) != 0 {
        return Err(FilterReason::WhitelistMismatch);
    }
    if !marginal_product_ok {
        return Err(FilterReason::MarginalRateInsufficient);
    }
    if !has_positive_bucket {
        return Err(FilterReason::AmountGridNoCandidate);
    }
    if !selected_amount_within_cap {
        return Err(FilterReason::DepthCapExceeded);
    }
    Ok(())
}

/// Plan §10.6 **fee-floor bound** — cheap reject for routes whose per-hop fee sum already
/// exceeds the maximum dislocation we'd ever see on a deep pool. For a closed cycle the
/// gross spread must cover the sum of per-hop fees (in bps); if the dislocation upper
/// bound (`max_dislocation_bps`) doesn't clear the fee floor, the route cannot be
/// profitable no matter the size.
///
/// `fee_bps_per_hop` are basis-point fees per hop (V3: `fee_pips / 100`; V2: `fee_bps`).
/// `max_dislocation_bps` is the operator-set ceiling on what spread we'd ever see — a
/// pool dislocated by more than this is either a genuine arb opportunity or a black-swan
/// price discontinuity. Default ceiling: 200 bps (2%).
#[inline]
pub fn fee_floor_passes(fee_bps_per_hop: &[u32], max_dislocation_bps: u32) -> FilterResult {
    let total: u32 = fee_bps_per_hop.iter().copied().sum();
    if total >= max_dislocation_bps {
        Err(FilterReason::MarginalRateInsufficient)
    } else {
        Ok(())
    }
}

/// Plan §10.6 **depth-upper bound** — cheap reject for routes whose minimum-viable input
/// (the entry-token amount that would produce a gross profit clearing the cost floor)
/// already exceeds the smallest hop's depth cap. If we can't push enough through the
/// shallowest hop in the cycle to clear costs, no sizing within depth makes the route
/// profitable.
///
/// `min_viable_amount_in` is the smallest input that, given the cycle's gross-rate
/// estimate, produces a profit above `cost_floor_wei`. `per_hop_depth_caps` is the
/// per-hop maximum input the route is allowed to use (e.g. `reserve_in * max_pct / 100`).
#[inline]
pub fn depth_upper_passes(min_viable_amount_in: u128, per_hop_depth_caps: &[u128]) -> FilterResult {
    let shallowest = per_hop_depth_caps.iter().copied().min().unwrap_or(0);
    if min_viable_amount_in > shallowest {
        Err(FilterReason::DepthCapExceeded)
    } else {
        Ok(())
    }
}

/// Convenience: run a v2/v2 amount-grid scan, returning the bucket with the highest gross
/// output minus input (gross profit in the base token). Returns `None` if every bucket is
/// non-positive.
pub fn best_v2v2_bucket(
    grid: &[u128],
    r_in_1: u128,
    r_out_1: u128,
    fee_bps_1: u32,
    r_in_2: u128,
    r_out_2: u128,
    fee_bps_2: u32,
) -> Option<(usize, u128)> {
    let mut best: Option<(usize, u128)> = None;
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
        if out > amount {
            let profit = out - amount;
            match best {
                None => best = Some((i, profit)),
                Some((_, bp)) if profit > bp => best = Some((i, profit)),
                _ => {}
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_product_basic() {
        // 2/1 * 1/2 = 1; no edge over 0 cost floor.
        let r = marginal_rate_product_v2v2(1_000, 2_000, 30, 2_000, 1_000, 30, 0);
        // After fees it strictly < 1, so this should fail.
        assert!(r.is_err());
    }

    #[test]
    fn rate_product_passes_with_edge() {
        // 2/1 * 1.01/2 > 1
        let r = marginal_rate_product_v2v2(1_000, 2_000, 30, 2_000, 1_010, 30, 0);
        assert!(r.is_ok());
    }

    #[test]
    fn generation_mismatch_rejects() {
        let r = route_passes_filters(&[1, 2], &[1, 3], 0, 0, 0, true, true, true);
        assert_eq!(r, Err(FilterReason::StaleGeneration));
    }

    #[test]
    fn whitelist_mask_rejects() {
        let r = route_passes_filters(&[1], &[1], 0b001, 0b010, 0, true, true, true);
        assert_eq!(r, Err(FilterReason::WhitelistMismatch));
    }

    #[test]
    fn amount_grid_picks_best() {
        // Pool 1: R = (1e8 A, 2e8 B); Pool 2: R = (1e8 B, 5e7 A) but dislocated to 4e7 A
        // (after some big buy) — i.e. effective marginal rate of pool 2 is ~0.4 vs the
        // expected 0.5. Cycle product (200/100 * 40/100) = 0.8 still < 1; arbitrage in
        // the *other* direction. Pool 1 expensive A→B; pool 2 cheap B→A → buy B on 2 and
        // sell on 1. We model that by swapping (r_in_2, r_out_2) here.
        let grid = vec![1_000u128, 10_000, 100_000, 1_000_000];
        // Constructed numbers: r_in_1 = 1e9 token0, r_out_1 = 4e9 token1
        // r_in_2 = 4e9 token1, r_out_2 = 1.01e9 token0
        let best = best_v2v2_bucket(
            &grid,
            1_000_000_000u128,
            4_000_000_000u128,
            30,
            4_000_000_000u128,
            1_010_000_000u128,
            30,
        );
        assert!(best.is_some(), "expected a positive bucket: {best:?}");
    }

    #[test]
    fn fee_floor_rejects_when_fees_exceed_ceiling() {
        // 2-hop with 30 bps each = 60 bps total; ceiling 50 bps → reject.
        let r = fee_floor_passes(&[30, 30], 50);
        assert_eq!(r, Err(FilterReason::MarginalRateInsufficient));
    }

    #[test]
    fn fee_floor_passes_when_fees_under_ceiling() {
        // 2-hop with 5 bps each = 10 bps total; ceiling 200 bps → pass.
        let r = fee_floor_passes(&[5, 5], 200);
        assert!(r.is_ok());
    }

    #[test]
    fn fee_floor_three_hop_fast_pools() {
        // 3-hop triangle on Camelot 5 bps + Sushi V3 5 bps + Uniswap V3 5 bps = 15 bps;
        // typical Arbitrum dislocations are 5–50 bps, so even 15 bps over a 50 bps
        // ceiling is profitable; the filter must not reject.
        let r = fee_floor_passes(&[5, 5, 5], 50);
        assert!(r.is_ok());
    }

    #[test]
    fn depth_upper_rejects_when_min_viable_too_big() {
        // Route needs $100 000 entry to clear costs, but the shallowest hop's depth cap
        // is only $50 000 → reject.
        let r = depth_upper_passes(
            100_000_000_000_000_000_000_000,
            &[
                50_000_000_000_000_000_000_000,
                200_000_000_000_000_000_000_000,
            ],
        );
        assert_eq!(r, Err(FilterReason::DepthCapExceeded));
    }

    #[test]
    fn depth_upper_passes_when_route_fits() {
        // Min viable amount is small relative to the shallowest hop → pass.
        let r = depth_upper_passes(
            1_000_000_000_000_000_000,
            &[
                50_000_000_000_000_000_000_000,
                200_000_000_000_000_000_000_000,
            ],
        );
        assert!(r.is_ok());
    }

    // C4: an out-of-range fee must fail closed (reject) and must NOT underflow-panic (debug) or
    // wrap (release) on `10_000 - fee_bps` / `1_000_000 - fee_pips`.
    #[test]
    fn v2v2_out_of_range_fee_rejects_without_panic() {
        // 10_500 bps > 100% — would underflow `10_000 - fee_bps`. Must reject, not panic/PASS.
        let r = marginal_rate_product_v2v2(1_000, 2_000, 10_500, 2_000, 1_010, 30, 0);
        assert_eq!(r, Err(FilterReason::MarginalRateInsufficient));
        // Symmetric: second-hop fee out of range.
        let r2 = marginal_rate_product_v2v2(1_000, 2_000, 30, 2_000, 1_010, 10_500, 0);
        assert_eq!(r2, Err(FilterReason::MarginalRateInsufficient));
        // Exactly 100% (10_000 bps) is also out of range (no value survives).
        let r3 = marginal_rate_product_v2v2(1_000, 2_000, 10_000, 2_000, 1_010, 30, 0);
        assert_eq!(r3, Err(FilterReason::MarginalRateInsufficient));
    }

    #[test]
    fn v3v3_out_of_range_fee_rejects_without_panic() {
        use alloy_primitives::U256;
        let sqrt = U256::from(1u64) << 96; // price 1.0
                                           // 1_500_000 pips > 100% — would underflow `1_000_000 - fee_pips`. Must reject.
        let r = marginal_rate_product_v3v3(sqrt, 1_500_000, true, sqrt, 3_000, false, 0);
        assert_eq!(r, Err(FilterReason::MarginalRateInsufficient));
        // Diagnostic twin returns None (its fail-closed) on the same input.
        let d = marginal_rate_ratio_q32_v3v3(sqrt, 1_500_000, true, sqrt, 3_000, false);
        assert!(d.is_none());
    }

    // C5: confirm a saturating-large-reserve case fails closed (rejects), never spuriously passes.
    #[test]
    fn v2v2_saturating_reserves_reject_not_pass() {
        // Reserves at u128::MAX make the cross-products saturate U256::MAX on both sides. With
        // lhs and rhs both clamped to MAX, `lhs > rhs` is false → reject (fail-closed), as intended.
        let r = marginal_rate_product_v2v2(u128::MAX, u128::MAX, 30, u128::MAX, u128::MAX, 30, 0);
        assert_eq!(r, Err(FilterReason::MarginalRateInsufficient));
    }
}
