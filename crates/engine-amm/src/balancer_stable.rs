//! Balancer V2 stable-pool math (ComposableStable / MetaStable / legacy Stable),
//! integer-exact port of `StableMath.sol` from the Balancer V2 monorepo.
//!
//! The stable invariant `D` is the same StableSwap family as Curve, but Balancer's
//! exact iteration form, amplification precision (`_AMP_PRECISION = 1000`), and
//! fee-on-input convention differ — so we port Balancer's formulas verbatim rather
//! than reuse the Curve kernel (which takes the fee on the OUTPUT and uses
//! `A_PRECISION = 100`). Mis-matching either would mis-price the pool.
//!
//! Balances are passed already upscaled to 18 decimals via the pool's scaling
//! factors (`getScalingFactors()`, which fold in token decimals AND rate-provider
//! rates for ComposableStable LST pools). The amplification parameter is the raw
//! value from `getAmplificationParameter()` (`A * 1000`).
//!
//! `StableMath` uses RAW integer `mul`/`divDown`/`divUp` inside the invariant and
//! `getTokenBalance` Newton loops (the formula is homogeneous in the 18-dec
//! balances); only the swap fee uses 18-dec fixed-point `mulUp`.

use alloy_primitives::U256;

use crate::AmmError;

/// `_AMP_PRECISION` from StableMath — amplification is stored as `A * 1000`.
pub const AMP_PRECISION: u64 = 1000;
/// 18-decimal fixed-point one.
const ONE: u128 = 1_000_000_000_000_000_000;
/// Max tokens in a Balancer stable pool (ComposableStable supports up to 5 + BPT;
/// we allow 8 for headroom). The BPT index is excluded by the caller.
pub const MAX_TOKENS: usize = 8;

/// Per-pool stable state snapshot. `balances` and `scaling_factors` are indexed by
/// the pool's token order with the BPT entry already removed by the snapshot layer.
#[derive(Debug, Clone, Copy)]
pub struct BalancerStableState {
    /// Number of (non-BPT) tokens, 2..=MAX_TOKENS.
    pub n: usize,
    /// Raw token balances (smallest units), length `n`.
    pub balances: [u128; MAX_TOKENS],
    /// 18-dec scaling factors from `getScalingFactors()` (decimals × rate), length `n`.
    pub scaling_factors: [U256; MAX_TOKENS],
    /// Amplification parameter, raw `A * AMP_PRECISION`.
    pub amp: u64,
    /// Swap fee as an 18-dec fraction (e.g. `1e15` = 0.1%).
    pub swap_fee_1e18: u128,
}

#[inline]
fn one() -> U256 {
    U256::from(ONE)
}

/// `FixedPoint.mulDown(a, b) = a * b / 1e18`.
#[inline]
fn mul_down(a: U256, b: U256) -> Result<U256, AmmError> {
    Ok(a.checked_mul(b).ok_or(AmmError::Overflow)? / one())
}

/// `FixedPoint.mulUp(a, b) = ceil(a * b / 1e18)`.
#[inline]
fn mul_up(a: U256, b: U256) -> Result<U256, AmmError> {
    let p = a.checked_mul(b).ok_or(AmmError::Overflow)?;
    if p.is_zero() {
        return Ok(U256::ZERO);
    }
    Ok((p - U256::from(1u64)) / one() + U256::from(1u64))
}

/// `FixedPoint.divDown(a, b) = a * 1e18 / b`.
#[inline]
fn div_down(a: U256, b: U256) -> Result<U256, AmmError> {
    if b.is_zero() {
        return Err(AmmError::ZeroLiquidity);
    }
    Ok(a.checked_mul(one()).ok_or(AmmError::Overflow)? / b)
}

/// Raw integer divUp: `ceil(a / b)` (no fixed-point scaling).
#[inline]
fn raw_div_up(a: U256, b: U256) -> Result<U256, AmmError> {
    if b.is_zero() {
        return Err(AmmError::ZeroLiquidity);
    }
    if a.is_zero() {
        return Ok(U256::ZERO);
    }
    Ok((a - U256::from(1u64)) / b + U256::from(1u64))
}

/// `StableMath._calculateInvariant` — Newton iteration for `D`.
fn calculate_invariant(amp: u64, balances: &[U256]) -> Result<U256, AmmError> {
    let n = balances.len();
    let n_u = U256::from(n as u64);
    let mut sum = U256::ZERO;
    for &b in balances {
        sum = sum.checked_add(b).ok_or(AmmError::Overflow)?;
    }
    if sum.is_zero() {
        return Ok(U256::ZERO);
    }
    let amp_total = U256::from(amp).checked_mul(n_u).ok_or(AmmError::Overflow)?;
    let amp_prec = U256::from(AMP_PRECISION);
    let mut invariant = sum;
    for _ in 0..255 {
        // D_P = D^(n+1) / (n^n * prod(balances)), built iteratively.
        let mut d_p = invariant;
        for &b in balances {
            // d_p = d_p * D / (b * n)
            let denom = b.checked_mul(n_u).ok_or(AmmError::Overflow)?;
            if denom.is_zero() {
                return Err(AmmError::ZeroLiquidity);
            }
            d_p = d_p.checked_mul(invariant).ok_or(AmmError::Overflow)? / denom;
        }
        let prev = invariant;
        // numerator = (amp_total*sum/AMP_PREC + d_p*n) * invariant
        let num = (amp_total.checked_mul(sum).ok_or(AmmError::Overflow)? / amp_prec)
            .checked_add(d_p.checked_mul(n_u).ok_or(AmmError::Overflow)?)
            .ok_or(AmmError::Overflow)?
            .checked_mul(invariant)
            .ok_or(AmmError::Overflow)?;
        // denom = ((amp_total-AMP_PREC)*invariant)/AMP_PREC + (n+1)*d_p
        let denom = ((amp_total.checked_sub(amp_prec).ok_or(AmmError::Underflow)?)
            .checked_mul(invariant)
            .ok_or(AmmError::Overflow)?
            / amp_prec)
            .checked_add(
                U256::from((n + 1) as u64)
                    .checked_mul(d_p)
                    .ok_or(AmmError::Overflow)?,
            )
            .ok_or(AmmError::Overflow)?;
        if denom.is_zero() {
            return Err(AmmError::InvariantViolation);
        }
        invariant = num / denom;
        let diff = if invariant > prev {
            invariant - prev
        } else {
            prev - invariant
        };
        if diff <= U256::from(1u64) {
            return Ok(invariant);
        }
    }
    Err(AmmError::InvariantViolation)
}

/// `StableMath._getTokenBalanceGivenInvariantAndAllOtherBalances` — Newton solve
/// for the post-swap balance of `token_index`.
fn get_token_balance_given_invariant(
    amp: u64,
    balances: &[U256],
    invariant: U256,
    token_index: usize,
) -> Result<U256, AmmError> {
    let n = balances.len();
    let n_u = U256::from(n as u64);
    let amp_total = U256::from(amp).checked_mul(n_u).ok_or(AmmError::Overflow)?;
    let amp_prec = U256::from(AMP_PRECISION);

    let mut sum = balances[0];
    let mut p_d = balances[0].checked_mul(n_u).ok_or(AmmError::Overflow)?;
    for &b in balances.iter().skip(1) {
        // P_D = P_D * b * n / invariant
        p_d = p_d
            .checked_mul(b)
            .ok_or(AmmError::Overflow)?
            .checked_mul(n_u)
            .ok_or(AmmError::Overflow)?
            / invariant;
        sum = sum.checked_add(b).ok_or(AmmError::Overflow)?;
    }
    sum = sum
        .checked_sub(balances[token_index])
        .ok_or(AmmError::Underflow)?;

    let inv2 = invariant.checked_mul(invariant).ok_or(AmmError::Overflow)?;
    // c = divUp(inv2, amp_total*P_D) * AMP_PREC * balances[tokenIndex]
    let amp_pd = amp_total.checked_mul(p_d).ok_or(AmmError::Overflow)?;
    let c = raw_div_up(inv2, amp_pd)?
        .checked_mul(amp_prec)
        .ok_or(AmmError::Overflow)?
        .checked_mul(balances[token_index])
        .ok_or(AmmError::Overflow)?;
    // b = sum + (invariant / amp_total) * AMP_PREC
    let b = sum
        .checked_add(
            (invariant / amp_total)
                .checked_mul(amp_prec)
                .ok_or(AmmError::Overflow)?,
        )
        .ok_or(AmmError::Overflow)?;

    // token_balance = divUp(inv2 + c, invariant + b)
    let mut token_balance = raw_div_up(
        inv2.checked_add(c).ok_or(AmmError::Overflow)?,
        invariant.checked_add(b).ok_or(AmmError::Overflow)?,
    )?;
    for _ in 0..255 {
        let prev = token_balance;
        // tb = divUp(tb*tb + c, tb*2 + b - invariant)
        let num = token_balance
            .checked_mul(token_balance)
            .ok_or(AmmError::Overflow)?
            .checked_add(c)
            .ok_or(AmmError::Overflow)?;
        let denom = token_balance
            .checked_mul(U256::from(2u64))
            .ok_or(AmmError::Overflow)?
            .checked_add(b)
            .ok_or(AmmError::Overflow)?
            .checked_sub(invariant)
            .ok_or(AmmError::Underflow)?;
        token_balance = raw_div_up(num, denom)?;
        let diff = if token_balance > prev {
            token_balance - prev
        } else {
            prev - token_balance
        };
        if diff <= U256::from(1u64) {
            return Ok(token_balance);
        }
    }
    Err(AmmError::InvariantViolation)
}

/// Exact-input swap: amount of token `j` out for `amount_in_raw` units of token `i`
/// (both in raw smallest units). Applies the swap fee on the input, upscales by the
/// pool scaling factors, runs StableMath, and downscales the output.
pub fn calc_out_given_in(
    state: &BalancerStableState,
    i: usize,
    j: usize,
    amount_in_raw: u128,
) -> Result<u128, AmmError> {
    if state.n < 2 || state.n > MAX_TOKENS {
        return Err(AmmError::InvariantViolation);
    }
    if i >= state.n || j >= state.n || i == j {
        return Err(AmmError::InvariantViolation);
    }
    if amount_in_raw == 0 {
        return Err(AmmError::ZeroInput);
    }
    // Upscale balances to 18-dec.
    let mut bal = [U256::ZERO; MAX_TOKENS];
    for k in 0..state.n {
        bal[k] = mul_down(U256::from(state.balances[k]), state.scaling_factors[k])?;
        if bal[k].is_zero() {
            return Err(AmmError::ZeroLiquidity);
        }
    }
    // Fee on input (raw), then upscale the post-fee input.
    let fee = mul_up(U256::from(amount_in_raw), U256::from(state.swap_fee_1e18))?;
    let amount_in_after_fee = U256::from(amount_in_raw)
        .checked_sub(fee)
        .ok_or(AmmError::Underflow)?;
    let amount_in_scaled = mul_down(amount_in_after_fee, state.scaling_factors[i])?;

    let invariant = calculate_invariant(state.amp, &bal[..state.n])?;

    let mut bal_after = bal;
    bal_after[i] = bal_after[i]
        .checked_add(amount_in_scaled)
        .ok_or(AmmError::Overflow)?;
    let final_out =
        get_token_balance_given_invariant(state.amp, &bal_after[..state.n], invariant, j)?;
    // amountOut(scaled) = balances[j] - finalBalanceOut - 1
    if final_out
        .checked_add(U256::from(1u64))
        .ok_or(AmmError::Overflow)?
        >= bal[j]
    {
        return Ok(0);
    }
    let out_scaled = bal[j] - final_out - U256::from(1u64);
    // Downscale to raw token j units (divDown rounds in the pool's favor).
    let out_raw = div_down(out_scaled, state.scaling_factors[j])?;
    out_raw.try_into().map_err(|_| AmmError::Overflow)
}

/// Predict the post-swap state of a Balancer stable `swap(i→j, amount_in)` for the feed-driven state
/// predictor (TAIL_LATENCY_PLAN C2). Returns the output and a new state with `balances[i] +=
/// amount_in` and `balances[j] -= amount_out`. The swap fee stays in the pool (raising `balances[i]`
/// by the full input is correct); the protocol-fee skim is taken separately and is corrected by the
/// next resync. `amp`/`scaling_factors`/`swap_fee` are unchanged. Closes the Balancer 30 s blind
/// spot (the audit's worst case — no event path). Never used for execution sizing.
pub fn apply_swap(
    state: &BalancerStableState,
    i: usize,
    j: usize,
    amount_in_raw: u128,
) -> Result<(u128, BalancerStableState), AmmError> {
    let amount_out = calc_out_given_in(state, i, j, amount_in_raw)?;
    let mut new = *state;
    new.balances[i] = state.balances[i]
        .checked_add(amount_in_raw)
        .ok_or(AmmError::Overflow)?;
    new.balances[j] = state.balances[j]
        .checked_sub(amount_out)
        .ok_or(AmmError::Underflow)?;
    Ok((amount_out, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf18() -> U256 {
        U256::from(ONE)
    }

    // Balanced 2-token stable pool, high amp: out ≈ in minus fee, minimal slippage.
    #[test]
    fn balanced_stable_pool_near_one_to_one() {
        let bal = 1_000_000u128 * ONE; // 1e6 of each, 18-dec
        let st = BalancerStableState {
            n: 2,
            balances: {
                let mut b = [0u128; MAX_TOKENS];
                b[0] = bal;
                b[1] = bal;
                b
            },
            scaling_factors: {
                let mut s = [U256::ZERO; MAX_TOKENS];
                s[0] = sf18();
                s[1] = sf18();
                s
            },
            amp: 1000 * AMP_PRECISION,            // A=1000
            swap_fee_1e18: 1_000_000_000_000_000, // 0.1%
        };
        let dx = 1000u128 * ONE; // swap 1000
        let out = calc_out_given_in(&st, 0, 1, dx).unwrap();
        // Expect ~999 out (0.1% fee + tiny slippage), well within [990, 1000].
        let out_h = out / ONE;
        assert!((990..=1000).contains(&out_h), "out={out_h}");

        // C2 apply_swap: balances move by (+dx on i, -out on j); params unchanged; price-impacting.
        let (dy, new) = apply_swap(&st, 0, 1, dx).unwrap();
        assert_eq!(dy, out);
        assert_eq!(new.balances[0], bal + dx);
        assert_eq!(new.balances[1], bal - out);
        assert_eq!(new.amp, st.amp);
        assert_eq!(new.swap_fee_1e18, st.swap_fee_1e18);
        let dy2 = calc_out_given_in(&new, 0, 1, dx).unwrap();
        assert!(dy2 < dy, "post-trade rate must be worse: dy2={dy2} dy={dy}");
    }

    // AUD-017: prove the kernel FOLDS IN the rate provider's scaling factor for LST ComposableStable
    // pools (wstETH/rETH carry getScalingFactors() = 10^(18-dec) × getRate() ≠ 1e18). Build two pools
    // with IDENTICAL 18-dec *scaled* balances, but pool A gives token0 a 1.2× rate (scaling 1.2e18, raw
    // 1e24) while pool B is rate-free (scaling 1e18, raw 1.2e24). The scaled-space StableMath is then
    // byte-identical, so both produce the SAME scaled output; only the final downscale by token0's
    // scaling factor differs. Hence the rate pool's RAW output must be exactly 1.2× smaller — proving
    // the rate is applied to balances AND output. If scaling were ignored (the AUD-017 bug), out_a would
    // equal out_b and the first assert fails — so this test is discriminating, not just a lock.
    #[test]
    fn rate_provider_scaling_is_applied_to_balances_and_output() {
        let scaled_bal = 1_200_000u128 * ONE; // 1.2e24 scaled units per side in BOTH pools
        let rate = U256::from(12u64) * U256::from(ONE) / U256::from(10u64); // 1.2e18

        // Pool A: token0 = 1.2× rate token. raw0=1e24 (×1.2 ⇒ 1.2e24 scaled); raw1=1.2e24 (×1.0).
        let pool_a = BalancerStableState {
            n: 2,
            balances: {
                let mut b = [0u128; MAX_TOKENS];
                b[0] = 1_000_000u128 * ONE;
                b[1] = scaled_bal;
                b
            },
            scaling_factors: {
                let mut s = [U256::ZERO; MAX_TOKENS];
                s[0] = rate;
                s[1] = sf18();
                s
            },
            amp: 1000 * AMP_PRECISION,
            swap_fee_1e18: 0,
        };
        // Pool B (control): same SCALED balances (1.2e24 each), token0 rate-free.
        let pool_b = BalancerStableState {
            n: 2,
            balances: {
                let mut b = [0u128; MAX_TOKENS];
                b[0] = scaled_bal;
                b[1] = scaled_bal;
                b
            },
            scaling_factors: {
                let mut s = [U256::ZERO; MAX_TOKENS];
                s[0] = sf18();
                s[1] = sf18();
                s
            },
            amp: 1000 * AMP_PRECISION,
            swap_fee_1e18: 0,
        };

        let dx = 10_000u128 * ONE; // swap token1 → token0
        let out_a = calc_out_given_in(&pool_a, 1, 0, dx).unwrap();
        let out_b = calc_out_given_in(&pool_b, 1, 0, dx).unwrap();

        // The rate token yields FEWER raw units (each raw unit is worth 1.2×).
        assert!(
            out_a < out_b,
            "rate must be applied — rate token must yield fewer raw units: out_a={out_a} out_b={out_b}"
        );
        // EXACTLY ~1.2× fewer: out_a = floor(scaled_out/1.2), out_b = scaled_out ⇒ 0 ≤ 10·out_b − 12·out_a < 12.
        let lhs = out_b * 10;
        let rhs = out_a * 12;
        assert!(
            lhs >= rhs && lhs - rhs <= 24,
            "rate factor 1.2 not folded into output: out_a={out_a} out_b={out_b} (10·out_b={lhs} 12·out_a={rhs})"
        );
    }

    #[test]
    fn zero_input_rejected() {
        let st = BalancerStableState {
            n: 2,
            balances: [ONE; MAX_TOKENS],
            scaling_factors: [sf18(); MAX_TOKENS],
            amp: 1000 * AMP_PRECISION,
            swap_fee_1e18: 0,
        };
        assert!(matches!(
            calc_out_given_in(&st, 0, 1, 0),
            Err(AmmError::ZeroInput)
        ));
    }

    // Output strictly increases with input (monotonicity).
    #[test]
    fn monotonic_in_amount() {
        let bal = 500_000u128 * ONE;
        let st = BalancerStableState {
            n: 3,
            balances: {
                let mut b = [0u128; MAX_TOKENS];
                b[0] = bal;
                b[1] = bal;
                b[2] = bal;
                b
            },
            scaling_factors: {
                let mut s = [U256::ZERO; MAX_TOKENS];
                s[0] = sf18();
                s[1] = sf18();
                s[2] = sf18();
                s
            },
            amp: 500 * AMP_PRECISION,
            swap_fee_1e18: 0,
        };
        let a = calc_out_given_in(&st, 0, 1, 100 * ONE).unwrap();
        let b = calc_out_given_in(&st, 0, 1, 200 * ONE).unwrap();
        assert!(b > a, "expected monotonic, a={a} b={b}");
    }
}

/// K1/K2: randomized differential + property tests for the Balancer StableMath Newton kernel.
///
/// Plan invariant #8. Checks the `U256` invariant Newton loop against an independent `U512`
/// re-derivation (catches `U256` intermediate truncation / overflow-guard placement) plus the
/// StableSwap model invariants (`0 < D ≤ Σ`, monotonic output, balanced fee-free swaps never
/// return more than input). Inputs are bounded so the `U512` oracle never itself overflows.
#[cfg(test)]
mod balancer_stable_differential {
    use super::*;
    use proptest::prelude::*;
    use ruint::aliases::U512;

    fn u512(x: U256) -> U512 {
        U512::from(x)
    }

    /// Independent `U512` re-derivation of `StableMath._calculateInvariant`.
    fn calculate_invariant_u512(amp: u64, balances: &[U256]) -> Option<U512> {
        let n_u = U512::from(balances.len() as u64);
        let mut sum = U512::ZERO;
        for &b in balances {
            sum += u512(b);
        }
        if sum.is_zero() {
            return Some(U512::ZERO);
        }
        let amp_total = U512::from(amp) * n_u;
        let amp_prec = U512::from(AMP_PRECISION);
        let mut invariant = sum;
        for _ in 0..255 {
            let mut d_p = invariant;
            for &b in balances {
                let denom = u512(b) * n_u;
                if denom.is_zero() {
                    return None;
                }
                d_p = d_p * invariant / denom;
            }
            let prev = invariant;
            let num = (amp_total * sum / amp_prec + d_p * n_u) * invariant;
            let denom = ((amp_total - amp_prec) * invariant) / amp_prec
                + U512::from((balances.len() + 1) as u64) * d_p;
            if denom.is_zero() {
                return None;
            }
            invariant = num / denom;
            let diff = if invariant > prev {
                invariant - prev
            } else {
                prev - invariant
            };
            if diff <= U512::from(1u64) {
                return Some(invariant);
            }
        }
        None
    }

    /// 2–4 token state, 18-dec balances, scaling factor 1e18 (so upscaled == raw).
    fn arb_state() -> impl Strategy<Value = BalancerStableState> {
        (
            2usize..=4usize,
            1u64..=5_000u64,                    // A
            0u128..=10_000_000_000_000_000u128, // swap fee (0..1%)
            prop::collection::vec(
                1_000_000_000_000_000_000u128..=1_000_000_000_000_000_000_000_000_000_000u128, // 1..1e12 tokens at 18dp
                4,
            ),
        )
            .prop_map(|(n, a, fee, bals)| {
                let mut balances = [0u128; MAX_TOKENS];
                let mut scaling_factors = [U256::ZERO; MAX_TOKENS];
                for k in 0..n {
                    balances[k] = bals[k];
                    scaling_factors[k] = U256::from(ONE);
                }
                BalancerStableState {
                    n,
                    balances,
                    scaling_factors,
                    amp: a * AMP_PRECISION,
                    swap_fee_1e18: fee,
                }
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1500))]

        /// `calculate_invariant` (U256) is byte-identical to the U512 re-derivation when it returns
        /// Ok — the U256 fast path never truncates or mis-converges vs full precision.
        #[test]
        fn invariant_matches_u512(state in arb_state()) {
            let bal: Vec<U256> = (0..state.n).map(|k| U256::from(state.balances[k])).collect();
            if let Ok(d) = calculate_invariant(state.amp, &bal) {
                prop_assert_eq!(
                    Some(u512(d)),
                    calculate_invariant_u512(state.amp, &bal),
                    "U256 invariant diverged from U512 oracle"
                );
            }
        }

        /// `0 < D ≤ Σ balances` (StableSwap AM-GM bound).
        #[test]
        fn invariant_bounded_by_sum(state in arb_state()) {
            let bal: Vec<U256> = (0..state.n).map(|k| U256::from(state.balances[k])).collect();
            if let Ok(d) = calculate_invariant(state.amp, &bal) {
                let sum = bal.iter().copied().fold(U256::ZERO, |a, b| a + b);
                prop_assert!(d > U256::ZERO);
                prop_assert!(d <= sum, "D={d} exceeded Σ={sum}");
            }
        }

        /// `calc_out_given_in` never panics; more input ⇒ not less output.
        #[test]
        fn calc_out_monotonic(
            state in arb_state(),
            dx in 1_000_000_000_000_000u128..=1_000_000_000_000_000_000_000u128,
            bump in 1u128..=1_000_000_000_000_000_000_000u128,
        ) {
            let o1 = calc_out_given_in(&state, 0, 1, dx);
            let o2 = calc_out_given_in(&state, 0, 1, dx.saturating_add(bump));
            if let (Ok(a), Ok(b)) = (o1, o2) {
                prop_assert!(b >= a, "more dx ⇒ not less out ({a} → {b})");
            }
        }

        /// Balanced, fee-free: output never exceeds input (no free lunch on a stable pool).
        #[test]
        fn balanced_fee_free_no_free_lunch(
            bal in 1_000_000_000_000_000_000_000u128..=1_000_000_000_000_000_000_000_000_000u128,
            a in 1u64..=5_000u64,
            dx in 1_000_000_000_000_000u128..=1_000_000_000_000_000_000u128,
        ) {
            let mut balances = [0u128; MAX_TOKENS];
            let mut sf = [U256::ZERO; MAX_TOKENS];
            balances[0] = bal; balances[1] = bal;
            sf[0] = U256::from(ONE); sf[1] = U256::from(ONE);
            let st = BalancerStableState { n: 2, balances, scaling_factors: sf, amp: a * AMP_PRECISION, swap_fee_1e18: 0 };
            if let Ok(out) = calc_out_given_in(&st, 0, 1, dx) {
                prop_assert!(out <= dx, "fee-free balanced swap returned more than input: dx={dx} out={out}");
            }
        }
    }
}
