// Analytical solver for Uniswap V2 optimal input amounts
//
// Formula: A_optimal = (sqrt(reserve_in * reserve_out * fee_factor) - reserve_in * 10000) / fee_factor
// where fee_factor = 10000 - fee_bps

use alloy_primitives::{U256, U512};

use crate::scythe::fixed_point::isqrt_u256;

/// Solves for optimal input amount for a single V2 hop using analytical formula
///
/// For a single Uniswap V2 pool with constant product x*y=k, computes the
/// optimal input amount that maximizes output - input (before gas costs).
///
/// # Arguments
/// * `reserve_in` - Input token reserve in the pool
/// * `reserve_out` - Output token reserve in the pool
/// * `fee_bps` - Fee in basis points (e.g., 30 for 0.3%)
///
/// # Returns
/// Optimal input amount in wei, or None if:
/// - Reserves are zero
/// - Arithmetic overflow occurs
/// - Result would be negative (unbalanced reserves)
///
/// # Formula
/// ```text
/// fee_factor = 10000 - fee_bps
/// optimal = (sqrt(reserve_in * reserve_out * fee_factor) - reserve_in * 10000) / fee_factor
/// ```
///
/// # Example
/// ```no_run
/// use alloy_primitives::U256;
/// use engine_amm::scythe::solve_optimal_v2_single_hop;
///
/// let reserve_in = U256::from(1_000_000_000_000_000_000_000u128); // 1000e18
/// let reserve_out = U256::from(10_000_000_000_000_000_000u128);   // 10e18
/// let fee_bps = 30u32; // 0.3%
///
/// let _optimal = solve_optimal_v2_single_hop(reserve_in, reserve_out, fee_bps);
/// ```
pub fn solve_optimal_v2_single_hop(
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u32,
) -> Option<U256> {
    // Validate inputs
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return None;
    }

    // fee_factor = 10000 - fee_bps
    // For 0.3% fee: fee_factor = 10000 - 30 = 9970
    let fee_factor = 10000u32.checked_sub(fee_bps)?;
    if fee_factor == 0 {
        return None; // 100% fee is invalid
    }

    // Calculate: k = reserve_in * reserve_out * fee_factor
    let product = reserve_in.checked_mul(reserve_out)?;
    let k = product.checked_mul(U256::from(fee_factor))?;

    // sqrt(k)
    let sqrt_k = isqrt_u256(k)?;

    // reserve_in * 10000
    let reserve_in_scaled = reserve_in.checked_mul(U256::from(10000u32))?;

    // numerator = sqrt(k) - reserve_in * 10000
    // If sqrt(k) < reserve_in * 10000, reserves are too unbalanced for profitable arb
    if sqrt_k < reserve_in_scaled {
        return None;
    }
    let numerator = sqrt_k.checked_sub(reserve_in_scaled)?;

    // optimal = numerator / fee_factor
    let optimal = numerator.checked_div(U256::from(fee_factor))?;

    // Sanity check: result should be non-zero and less than reserve_in
    // (we can't trade more than the pool has)
    if optimal.is_zero() || optimal >= reserve_in {
        return None;
    }

    Some(optimal)
}

/// Closed-form optimal input for a **two-pool cyclic** arbitrage (borrow `x` of token A,
/// swap A→B on pool 1, then B→A on pool 2), with independent per-pool fees.
///
/// Composing the two constant-product hops gives a single Möbius map
/// `out_A(x) = E·x / (F + G·x)` with
/// * `E = g1·g2·ra_out·rb_out`, `F = ra_in·rb_in`, `G = g1·(rb_in + g2·ra_out)`,
/// * `g_i = (10000 − fee_i_bps)/10000`.
///
/// Maximising `out_A(x) − x` (set derivative to zero) yields `x* = (√(E·F) − F) / G`.
/// Profitable iff `√(E·F) > F`, i.e. `g1·g2·ra_out·rb_out > ra_in·rb_in` (the round-trip
/// marginal-rate product exceeds 1).
///
/// Everything is integer math. To clear the `g_i` denominators we scale by `1e8`:
/// `E8 = f1·f2·ra_out·rb_out`, `G8 = f1·10000·rb_in + f1·f2·ra_out` (with
/// `f_i = 10000 − fee_i_bps`), giving
/// `x* = (√(E8·F) − 10000·F) · 10000 / G8`. The radicand `E8·F` can exceed 2²⁵⁶ for deep
/// pools, so it is computed in `U512`.
///
/// Returns the **continuous** optimum floored to an integer — a *seed*. Because the
/// on-chain kernel floors at each hop, the true integer argmax can sit ±a few wei away;
/// callers must do a tiny exact-profit polish around this value.
///
/// `None` when reserves are zero, the cycle is unprofitable, or arithmetic would overflow.
pub fn solve_optimal_v2_two_hop(
    ra_in: U256,
    ra_out: U256,
    f1_bps: u32,
    rb_in: U256,
    rb_out: U256,
    f2_bps: u32,
) -> Option<U256> {
    if ra_in.is_zero() || ra_out.is_zero() || rb_in.is_zero() || rb_out.is_zero() {
        return None;
    }
    // Fee factors f_i = 10000 − fee_i_bps (matches the kernel's 1e4-scaled fee).
    let f1 = U256::from(10_000u32.checked_sub(f1_bps)?);
    let f2 = U256::from(10_000u32.checked_sub(f2_bps)?);
    if f1.is_zero() || f2.is_zero() {
        return None; // 100% fee is invalid
    }
    let ten_k = U256::from(10_000u32);
    let f1f2 = f1.checked_mul(f2)?;

    // E8 = f1·f2·ra_out·rb_out  ;  F = ra_in·rb_in.
    let e8 = f1f2.checked_mul(ra_out)?.checked_mul(rb_out)?;
    let f_term = ra_in.checked_mul(rb_in)?;

    // √(E8·F). Use the fast U256 isqrt when the radicand fits; widen to U512 only for deep
    // pools whose `E8·F` exceeds 2²⁵⁶ (e.g. ≥1e18-scale 18-decimal reserves).
    let sqrt_rad = match e8.checked_mul(f_term) {
        Some(rad_256) => isqrt_u256(rad_256)?,
        None => {
            let rad_512 = U512::from(e8).checked_mul(U512::from(f_term))?;
            let sqrt_512 = isqrt_u512(rad_512);
            // √(E8·F) < 2²⁵⁶ for all realistic (≤u112) reserves; guard the narrowing anyway.
            if sqrt_512 > U512::from(U256::MAX) {
                return None;
            }
            let limbs = sqrt_512.into_limbs();
            U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]])
        }
    };

    // Profitable iff √(E8·F) > 10000·F (round-trip marginal-rate product > 1).
    let ten_k_f = f_term.checked_mul(ten_k)?;
    if sqrt_rad <= ten_k_f {
        return None;
    }
    // numerator = (√(E8·F) − 10000·F) · 10000
    let numerator = sqrt_rad.checked_sub(ten_k_f)?.checked_mul(ten_k)?;

    // G8 = f1·10000·rb_in + f1·f2·ra_out
    let g8 = f1
        .checked_mul(ten_k)?
        .checked_mul(rb_in)?
        .checked_add(f1f2.checked_mul(ra_out)?)?;
    if g8.is_zero() {
        return None;
    }

    let optimal = numerator.checked_div(g8)?;
    if optimal.is_zero() {
        return None;
    }
    Some(optimal)
}

/// `floor(sqrt(n))` for a `U512` via Newton's method, seeded with a bit-length initial guess
/// `x0 = 2^ceil(bits/2) ≥ sqrt(n)` so the descending iteration converges in ~8 steps (a naive
/// `x0 = n` would take O(bits/2) huge 512-bit divisions instead).
fn isqrt_u512(n: U512) -> U512 {
    if n.is_zero() {
        return U512::ZERO;
    }
    let bits = 512 - n.leading_zeros() as u32;
    let shift = bits.div_ceil(2);
    let mut x = if shift >= 512 {
        n
    } else {
        U512::from(1u64) << shift as usize
    };
    loop {
        let nx = (x + n / x) >> 1;
        if nx >= x {
            return x;
        }
        x = nx;
    }
}

#[cfg(test)]
mod two_hop_tests {
    use super::*;
    use crate::uniswap_v2::get_amount_out;

    /// Byte-exact two-hop cycle output using the production V2 kernel.
    fn cycle(
        r_in1: u128,
        r_out1: u128,
        f1: u32,
        r_in2: u128,
        r_out2: u128,
        f2: u32,
        x: u128,
    ) -> Option<u128> {
        let mid = get_amount_out(x, r_in1, r_out1, f1).ok()?;
        if mid == 0 {
            return None;
        }
        get_amount_out(mid, r_in2, r_out2, f2).ok()
    }

    fn profit(
        r_in1: u128,
        r_out1: u128,
        f1: u32,
        r_in2: u128,
        r_out2: u128,
        f2: u32,
        x: u128,
    ) -> i128 {
        match cycle(r_in1, r_out1, f1, r_in2, r_out2, f2, x) {
            Some(o) => o as i128 - x as i128,
            None => i128::MIN,
        }
    }

    /// Production-style polish: best exact profit (and its argmax) in `seed ± window`.
    /// Mirrors what `evaluate_two_hop` does around the closed-form seed.
    const POLISH_WINDOW: u128 = 4;
    fn polish(
        seed: u128,
        r_in1: u128,
        r_out1: u128,
        f1: u32,
        r_in2: u128,
        r_out2: u128,
        f2: u32,
    ) -> (u128, i128) {
        let mut best = (seed, i128::MIN);
        let mut d = 0u128;
        while d <= POLISH_WINDOW {
            for x in [seed.saturating_sub(d), seed.saturating_add(d)] {
                if x == 0 {
                    continue;
                }
                let p = profit(r_in1, r_out1, f1, r_in2, r_out2, f2, x);
                if p > best.1 {
                    best = (x, p);
                }
            }
            d += 1;
        }
        best
    }

    fn check(r_in1: u128, r_out1: u128, f1: u32, r_in2: u128, r_out2: u128, f2: u32) {
        let sol = solve_optimal_v2_two_hop(
            U256::from(r_in1),
            U256::from(r_out1),
            f1,
            U256::from(r_in2),
            U256::from(r_out2),
            f2,
        )
        .expect("profitable cycle should solve");
        let seed = u128::try_from(sol).expect("optimum fits u128");
        let (px, pp) = polish(seed, r_in1, r_out1, f1, r_in2, r_out2, f2);
        assert!(pp > 0, "fixture must be profitable");

        // Profit near the optimum is a flat plateau (curvature → 0), so the exact integer
        // argmax is ambiguous to within a few wei; what matters is the profit. We require
        // closed-form + ±4 polish to be within 1 wei of the true maximum — note the incumbent
        // golden-section only converges to 0.1% (≈hundreds of wei here), so this is ~100×+
        // tighter. With the CPMM cycle's proven unimodality, "no point beats us by >1 wei
        // within ±256" ⇒ global optimum to 1-wei profit.
        const TOL: i128 = 1;

        // (a) Local optimality within ±256 of the polished argmax.
        for k in 1..=256u128 {
            for x in [px.saturating_sub(k), px.saturating_add(k)] {
                if x == 0 {
                    continue;
                }
                let p = profit(r_in1, r_out1, f1, r_in2, r_out2, f2, x);
                assert!(
                    p <= pp + TOL,
                    "size {x} nets {p} > closed-form+polish {pp} at {px} (+{TOL}) — not optimal"
                );
            }
        }

        // (b) Right basin: beats a coarse log grid spanning the whole [1, r_in1] range
        //     (guards against the closed form converging to a wrong region entirely).
        let mut amt = 1u128;
        while amt < r_in1 {
            let p = profit(r_in1, r_out1, f1, r_in2, r_out2, f2, amt);
            assert!(
                p <= pp + TOL,
                "grid size {amt} nets {p} > closed-form optimum {pp}"
            );
            amt = amt.saturating_mul(2);
        }
    }

    #[test]
    fn matches_brute_optimum_equal_fees() {
        check(
            1_000_000_000,
            2_000_000_000,
            30,
            2_000_000_000,
            1_050_000_000,
            30,
        );
    }

    #[test]
    fn matches_brute_optimum_mixed_fees() {
        check(
            1_000_000_000,
            2_000_000_000,
            30,
            2_000_000_000,
            1_050_000_000,
            5,
        );
        check(
            1_000_000_000,
            2_000_000_000,
            5,
            2_000_000_000,
            1_050_000_000,
            1,
        );
    }

    #[test]
    fn matches_brute_optimum_large_reserves() {
        // Exercises the U512 radicand path with u112-scale reserves.
        check(
            1_000_000_000_000_000_000_000_000,
            2_000_000_000_000_000_000_000_000,
            30,
            2_000_000_000_000_000_000_000_000,
            1_050_000_000_000_000_000_000_000,
            30,
        );
    }

    #[test]
    fn unprofitable_returns_none() {
        // Round-trip product < 1 → no profitable size.
        let r = solve_optimal_v2_two_hop(
            U256::from(1_000_000_000u128),
            U256::from(2_000_000_000u128),
            30,
            U256::from(1_000_000_000u128),
            U256::from(400_000_000u128),
            30,
        );
        assert!(r.is_none());
    }
}
