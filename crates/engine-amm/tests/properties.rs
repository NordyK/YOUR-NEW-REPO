//! Property-based tests for the AMM kernels. Each property is a load-bearing invariant
//! that must hold for every legal input across the V2, V3, and Curve families.

use engine_amm::{
    curve_get_dy, sqrt_price_at_tick, tick_at_sqrt_price, v2_get_amount_in, v2_get_amount_out,
    AmmError, CurveStableSwapState, CURVE_A_PRECISION, MAX_TICK, MIN_TICK,
};
use proptest::prelude::*;

// -------- V2 invariants --------

proptest! {
    /// Output never strictly exceeds the available reserve; otherwise the pool would be
    /// drained which the swap math must prevent.
    #[test]
    fn v2_output_bounded_by_reserve(
        amount_in in 1u128..1_000_000_000_000_000_000_000u128,
        r_in     in 1_000u128..1_000_000_000_000_000_000_000u128,
        r_out    in 1_000u128..1_000_000_000_000_000_000_000u128,
        fee_bps  in 0u32..=9_999u32,
    ) {
        let out = v2_get_amount_out(amount_in, r_in, r_out, fee_bps);
        if let Ok(out) = out {
            prop_assert!(out < r_out, "out={out} r_out={r_out}");
        }
    }

    /// Monotonicity: increasing amount_in never decreases amount_out (holding reserves &
    /// fee constant). Compares two adjacent inputs.
    #[test]
    fn v2_output_monotonic_in_amount(
        a in 1u128..1_000_000_000_000_000_000u128,
        delta in 1u128..1_000_000_000_000_000_000u128,
        r_in in 1_000_000u128..1_000_000_000_000_000_000_000u128,
        r_out in 1_000_000u128..1_000_000_000_000_000_000_000u128,
    ) {
        let lo = v2_get_amount_out(a, r_in, r_out, 30);
        let hi = v2_get_amount_out(a + delta, r_in, r_out, 30);
        if let (Ok(lo), Ok(hi)) = (lo, hi) {
            prop_assert!(hi >= lo, "monotonicity violated: a={a} delta={delta} lo={lo} hi={hi}");
        }
    }

    /// Larger fee → smaller (or equal) output for the same input/reserves.
    #[test]
    fn v2_output_monotonic_in_fee(
        amount in 1u128..1_000_000_000_000_000_000u128,
        r_in in 1_000_000u128..1_000_000_000_000_000_000_000u128,
        r_out in 1_000_000u128..1_000_000_000_000_000_000_000u128,
        fee_lo in 0u32..=4_000u32,
        delta in 1u32..=4_000u32,
    ) {
        let out_lo = v2_get_amount_out(amount, r_in, r_out, fee_lo);
        let out_hi = v2_get_amount_out(amount, r_in, r_out, fee_lo + delta);
        if let (Ok(lo), Ok(hi)) = (out_lo, out_hi) {
            prop_assert!(hi <= lo, "higher fee should not produce more output");
        }
    }

    /// Inverse: `get_amount_out(get_amount_in(target_out)) >= target_out`. This is the
    /// direction Solidity guarantees: `getAmountIn` rounds up, so feeding the result back
    /// through `getAmountOut` must produce at least the original target.
    #[test]
    fn v2_get_amount_in_meets_target(
        target_out in 1u128..1_000_000_000_000u128,
        r_in in 1_000_000_000_000u128..1_000_000_000_000_000_000_000u128,
        r_out in 1_000_000_000_000u128..1_000_000_000_000_000_000_000u128,
    ) {
        prop_assume!(target_out < r_out / 2);
        if let Ok(req_in) = v2_get_amount_in(target_out, r_in, r_out, 30) {
            if let Ok(realised_out) = v2_get_amount_out(req_in, r_in, r_out, 30) {
                prop_assert!(
                    realised_out >= target_out,
                    "get_amount_in did not produce enough: target={target_out} req_in={req_in} realised={realised_out}",
                );
            }
        }
    }

    /// Constant-product invariant: (r_in + amount_in_eff) * (r_out - out) >= r_in * r_out
    /// where amount_in_eff = amount_in * (10_000 - fee) / 10_000.
    #[test]
    fn v2_constant_product_invariant_holds(
        amount in 1u128..1_000_000_000_000u128,
        r_in   in 1_000_000_000_000u128..100_000_000_000_000_000_000_000u128,
        r_out  in 1_000_000_000_000u128..100_000_000_000_000_000_000_000u128,
        fee    in 0u32..=9_999u32,
    ) {
        let out = v2_get_amount_out(amount, r_in, r_out, fee);
        if let Ok(out) = out {
            // amount_in_with_fee = amount * (10_000 - fee). We compare in u256 space via wider
            // arithmetic to avoid overflow.
            use ruint::aliases::U256;
            let fee_factor = U256::from(10_000u64 - fee as u64);
            let amount_in_with_fee = U256::from(amount) * fee_factor;
            let new_r_in_times_10000 = U256::from(r_in) * U256::from(10_000u64) + amount_in_with_fee;
            let new_r_out = U256::from(r_out) - U256::from(out);
            let lhs = new_r_in_times_10000 * new_r_out;
            let rhs = U256::from(r_in) * U256::from(r_out) * U256::from(10_000u64);
            prop_assert!(lhs >= rhs, "constant-product invariant violated");
        }
    }

    /// Zero input / zero reserves are explicitly rejected with the right error.
    #[test]
    fn v2_zero_paths_rejected(
        r_in   in 0u128..1_000_000u128,
        r_out  in 0u128..1_000_000u128,
        amount in 0u128..1_000_000u128,
    ) {
        if amount == 0 {
            prop_assert_eq!(v2_get_amount_out(amount, 1_000, 1_000, 30), Err(AmmError::ZeroInput));
        }
        if (r_in == 0 || r_out == 0) && amount > 0 {
            prop_assert_eq!(v2_get_amount_out(amount, r_in, r_out, 30), Err(AmmError::ZeroLiquidity));
        }
    }
}

// -------- V3 invariants --------

proptest! {
    /// sqrt_price_at_tick is strictly monotonic across the canonical tick range.
    #[test]
    fn v3_sqrt_price_strictly_monotonic(
        a in (MIN_TICK + 1)..(MAX_TICK - 1),
        delta in 1i32..=10_000i32,
    ) {
        // Avoid stepping out of range.
        let b: i32 = a.saturating_add(delta);
        if b >= MAX_TICK { return Ok(()); }
        let sa = sqrt_price_at_tick(a).unwrap();
        let sb = sqrt_price_at_tick(b).unwrap();
        prop_assert!(sb > sa, "sa={sa} sb={sb} a={a} b={b}");
    }

    /// tick_at_sqrt_price(sqrt_price_at_tick(t)) == t for the canonical range.
    /// (Holds because we constructed sqrt_price values to be exact powers of 1.0001.)
    #[test]
    fn v3_tick_inverse_holds(t in MIN_TICK..=MAX_TICK) {
        let s = sqrt_price_at_tick(t).unwrap();
        let t2 = tick_at_sqrt_price(s).unwrap();
        prop_assert_eq!(t2, t, "tick inverse mismatch: t={} t2={}", t, t2);
    }
}

// -------- Curve invariants --------

proptest! {
    /// Curve get_dy is monotonic in dx for a fixed (symmetric) pool state.
    #[test]
    fn curve_dy_monotonic_in_dx(
        dx in 1u128..1_000_000_000_000_000_000u128,
        delta in 1u128..1_000_000_000_000_000_000u128,
    ) {
        let state = CurveStableSwapState {
            n_coins: 2,
            balances: [
                1_000_000_000_000_000_000_000,
                1_000_000_000_000_000_000_000,
                0, 0, 0, 0, 0, 0,
            ],
            rates: [
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000,
                0, 0, 0, 0, 0, 0,
            ],
            a_param: 200 * CURVE_A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        let lo = curve_get_dy(&state, 0, 1, dx);
        let hi = curve_get_dy(&state, 0, 1, dx + delta);
        if let (Ok(lo), Ok(hi)) = (lo, hi) {
            prop_assert!(hi >= lo);
        }
    }

    /// Curve get_dy never returns more than the target-coin balance.
    #[test]
    fn curve_dy_bounded_by_target_balance(
        dx in 1u128..100_000_000_000_000_000_000u128,
    ) {
        let bal_j: u128 = 1_000_000_000_000_000_000_000;
        let state = CurveStableSwapState {
            n_coins: 2,
            balances: [
                1_000_000_000_000_000_000_000,
                bal_j,
                0, 0, 0, 0, 0, 0,
            ],
            rates: [
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000,
                0, 0, 0, 0, 0, 0,
            ],
            a_param: 200 * CURVE_A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        if let Ok(dy) = curve_get_dy(&state, 0, 1, dx) {
            prop_assert!(dy < bal_j);
        }
    }
}
