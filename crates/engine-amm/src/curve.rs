//! Curve StableSwap-NG math, integer-exact.
//!
//! Reference: Curve `StableSwap-NG` plain-pool contracts (factory
//! `0x9AF14D26075f142eb3F292D5065EB3faa646167b` on Arbitrum One).
//!
//! For an `N`-coin pool with balances `b[0..N]`, amplification coefficient `A`, and
//! per-coin rates `rate[i]` that scale balances into 18-decimal units, the invariant `D`
//! satisfies:
//!
//! ```text
//! A * N^N * sum(x_i) + D = A * N^N * D + D^(N+1) / (N^N * prod(x_i))
//! ```
//!
//! where `x_i = b[i] * rate[i] / 1e18`. We compute `D` and `get_y` (next balance given a
//! swap from coin `i` to coin `j`) by Newton iteration.

use alloy_primitives::U256;

use crate::AmmError;

/// Fee denominator used by StableSwap-NG.
pub const FEE_DENOMINATOR: u64 = 10u64.pow(10);

/// `A` is stored in the contract as `A * A_PRECISION`. `A_PRECISION = 100`.
pub const A_PRECISION: u64 = 100;

/// Maximum supported coin count. StableSwap-NG factory pools allow up to 8; we match.
pub const MAX_COINS: usize = 8;

/// Per-pool state snapshot used by `get_dy`.
#[derive(Debug, Clone, Copy)]
pub struct StableSwapState {
    /// Number of coins (2..=8).
    pub n_coins: usize,
    /// Coin balances in raw token units, length = `n_coins`.
    pub balances: [u128; MAX_COINS],
    /// Per-coin rate scalers, length = `n_coins`. For non-rebasing coins this is `10^18`
    /// times the decimal multiplier (e.g. a 6-decimal coin has rate `10^30`).
    pub rates: [u128; MAX_COINS],
    /// Amplification coefficient (`A * A_PRECISION`).
    pub a_param: u64,
    /// Fee in 10^10 units (e.g. `4_000_000` = 4 bps).
    pub fee: u64,
    /// StableSwap-NG dynamic *offpeg* fee multiplier (`offpeg_fee_multiplier`, 10^10 units).
    /// Canonical NG pools scale the base fee UP as the pair de-pegs:
    /// `dynamic_fee = offpeg_mul·fee / (((offpeg_mul−1e10)·4·xpi·xpj)/(xpi+xpj)² + 1e10)`.
    /// At balance this equals `fee`; off-peg it rises toward `(offpeg_mul/1e10)·fee`. A value
    /// `<= FEE_DENOMINATOR` (incl. `0`, the back-compat default) means "no dynamic component" →
    /// the kernel uses the static `fee`, exactly as before. The multiplier only ever RAISES the
    /// effective fee (lowers output), i.e. it can only make a quote MORE conservative — the safe
    /// direction for a revert-on-shortfall executor.
    pub offpeg_fee_multiplier: u64,
}

/// StableSwap-NG dynamic offpeg fee (10^10 units), evaluated at the swap midpoint
/// `((xp[i]+x)/2, (xp[j]+y)/2)` exactly as the canonical Vyper `_dynamic_fee`. Returns the static
/// `base_fee` when `offpeg_multiplier <= FEE_DENOMINATOR` (the back-compat / no-dynamic case).
/// U512 intermediates keep it overflow-safe up to the kernel's 1e36 normalized-balance guard.
fn dynamic_fee(xpi_mid: U256, xpj_mid: U256, base_fee: u64, offpeg_multiplier: u64) -> u64 {
    let fee_denom = FEE_DENOMINATOR;
    if offpeg_multiplier <= fee_denom {
        return base_fee;
    }
    use ruint::aliases::U512;
    let xpi = U512::from(xpi_mid);
    let xpj = U512::from(xpj_mid);
    let sum = xpi + xpj;
    let xps2 = sum * sum;
    if xps2.is_zero() {
        return base_fee;
    }
    // term = (offpeg − FEE_DENOM)·4·xpi·xpj / (xpi+xpj)²  ∈ [0, offpeg − FEE_DENOM]
    let num =
        (U512::from(offpeg_multiplier) - U512::from(fee_denom)) * U512::from(4u64) * xpi * xpj;
    let term = num / xps2;
    let denom = term + U512::from(fee_denom);
    if denom.is_zero() {
        return base_fee;
    }
    let dfee = (U512::from(offpeg_multiplier) * U512::from(base_fee)) / denom;
    // dfee ∈ [base_fee, (offpeg/FEE_DENOM)·base_fee]; fits u64 for any realistic multiplier.
    let dfee_u128: u128 = dfee.try_into().unwrap_or(base_fee as u128);
    u64::try_from(dfee_u128.max(base_fee as u128)).unwrap_or(base_fee)
}

/// Fail-closed magnitude guard for normalized (18-dp) balances. `compute_d`/`get_y` multiply
/// normalized balances by `ann` (≤~1e27) in unguarded U256 ops. Real normalized balances are <<1e30
/// (a billion-dollar 18-dp pool ≈1e27); 1e36 is generous headroom yet far below the ~1e50 point
/// where `ann * s` would overflow U256. Crafted pools above it fail closed.
const MAX_NORMALIZED_BALANCE: u128 = 1_000_000_000_000_000_000_000_000_000_000_000_000; // 1e36

/// Scale every coin balance to 18-decimal `xp` units and apply the liquidity/magnitude guards.
/// Shared by [`get_dy`]/[`get_dy_cached`]/[`precompute_d`] so all three validate identically.
fn scaled_xp(state: &StableSwapState) -> Result<[U256; MAX_COINS], AmmError> {
    let max_norm = U256::from(MAX_NORMALIZED_BALANCE);
    let mut xp = [U256::ZERO; MAX_COINS];
    for k in 0..state.n_coins {
        xp[k] = scaled(state.balances[k], state.rates[k])?;
        if xp[k].is_zero() {
            return Err(AmmError::ZeroLiquidity);
        }
        if xp[k] > max_norm {
            return Err(AmmError::Overflow);
        }
    }
    Ok(xp)
}

/// Precompute the StableSwap invariant `D` — the size-INdependent half of every `get_dy` (a Newton
/// solve over the balances + `A`, unaffected by `(i, j, dx)`). Cache it at state-write time and pass
/// to [`get_dy_cached`] to skip the solve on each sizing probe. Byte-exact with the `D` that
/// [`get_dy`] computes internally; `Err` exactly when `get_dy` would on the same state.
pub fn precompute_d(state: &StableSwapState) -> Result<U256, AmmError> {
    if state.n_coins < 2 || state.n_coins > MAX_COINS {
        return Err(AmmError::InvariantViolation);
    }
    let xp = scaled_xp(state)?;
    compute_d(&xp[..state.n_coins], state.a_param)
}

/// Compute the swap output: amount of coin `j` received for `dx` units of coin `i`. Both
/// `dx` and the return value are in raw token units (not rate-scaled).
pub fn get_dy(state: &StableSwapState, i: usize, j: usize, dx: u128) -> Result<u128, AmmError> {
    get_dy_cached(state, i, j, dx, U256::ZERO)
}

/// [`get_dy`] with a caller-supplied precomputed invariant `d` (from [`precompute_d`] on the *same*
/// state). Pass `U256::ZERO` to compute `D` internally — then this is byte-identical to [`get_dy`].
/// The per-quote balance/index/magnitude guards still run, so a cached `D` never weakens safety; it
/// only removes the redundant Newton `D` solve when the sizer re-quotes one pool at many sizes.
pub fn get_dy_cached(
    state: &StableSwapState,
    i: usize,
    j: usize,
    dx: u128,
    d: U256,
) -> Result<u128, AmmError> {
    if state.n_coins < 2 || state.n_coins > MAX_COINS {
        return Err(AmmError::InvariantViolation);
    }
    if i >= state.n_coins || j >= state.n_coins || i == j {
        return Err(AmmError::InvariantViolation);
    }
    if dx == 0 {
        return Err(AmmError::ZeroInput);
    }
    let max_norm = U256::from(MAX_NORMALIZED_BALANCE);
    let xp = scaled_xp(state)?;
    // Use the cached invariant when supplied (non-zero); otherwise compute it — same value either
    // way. A real quotable pool always has D > 0 (zero balances are rejected above), so ZERO is an
    // unambiguous "not cached" sentinel.
    let d = if d.is_zero() {
        compute_d(&xp[..state.n_coins], state.a_param)?
    } else {
        d
    };
    // New balance for coin i after adding dx (in 18-dec units).
    let new_x_i = xp[i] + scaled(dx, state.rates[i])?;
    if new_x_i > max_norm {
        return Err(AmmError::Overflow);
    }
    let new_y = get_y(
        state.n_coins,
        state.a_param,
        &xp[..state.n_coins],
        i,
        j,
        new_x_i,
        d,
    )?;
    // dy in 18-dec = old_y - new_y - 1 (Curve rounds down).
    if new_y >= xp[j] {
        // No output (numerically zero); return 0.
        return Ok(0);
    }
    let dy_scaled = xp[j] - new_y - U256::from(1u64);
    // Apply fee — StableSwap-NG dynamic offpeg fee at the swap midpoint (static `fee` when the
    // pool carries no multiplier). The midpoint is `((xp[i]+x)/2, (xp[j]+y)/2)` per canonical Vyper.
    let xpi_mid = (xp[i] + new_x_i) / U256::from(2u64);
    let xpj_mid = (xp[j] + new_y) / U256::from(2u64);
    let eff_fee = dynamic_fee(xpi_mid, xpj_mid, state.fee, state.offpeg_fee_multiplier);
    let fee_amount = dy_scaled
        .checked_mul(U256::from(eff_fee))
        .ok_or(AmmError::Overflow)?
        / U256::from(FEE_DENOMINATOR);
    let dy_after_fee = dy_scaled - fee_amount;
    // Convert back to raw units of coin j: exactly invert the `scaled()` upscale done at the
    // top of get_dy. `rates[j]` carries the 1e18 decimal multiplier, so the raw amount is
    // `dy_after_fee * 1e18 / rates[j]`.
    let inv = U256::from(10u64).pow(U256::from(18));
    let dy_raw =
        dy_after_fee.checked_mul(inv).ok_or(AmmError::Overflow)? / U256::from(state.rates[j]);
    let out = u256_to_u128(dy_raw)?;
    Ok(out)
}

/// Predict the post-trade state of an `exchange(i, j, dx)` for the feed-driven state predictor
/// (TAIL_LATENCY_PLAN C2). Returns the output `dy` (identical to [`get_dy`]) and a new state with
/// `balances[i] += dx` and `balances[j] -= dy`.
///
/// This deliberately OMITS the admin-fee portion that Curve also removes from `balances[j]`
/// (≈ half the swap fee — a few bps of `dy`): it is a SUB-BLOCK approximation to keep the state
/// mirror fresh between blocks, corrected exactly by the next `TokenExchange` log / resync. It is
/// **never** used for execution sizing (that path uses the kernel quote / REVM); only to react to a
/// pending Curve swap ~one block sooner. The omission makes the next predicted quote marginally
/// optimistic by the admin fee, which the log reconciles within ~250 ms.
pub fn apply_exchange(
    state: &StableSwapState,
    i: usize,
    j: usize,
    dx: u128,
) -> Result<(u128, StableSwapState), AmmError> {
    let dy = get_dy(state, i, j, dx)?;
    let mut new_state = *state;
    new_state.balances[i] = state.balances[i]
        .checked_add(dx)
        .ok_or(AmmError::Overflow)?;
    new_state.balances[j] = state.balances[j]
        .checked_sub(dy)
        .ok_or(AmmError::Overflow)?;
    Ok((dy, new_state))
}

fn scaled(balance: u128, rate: u128) -> Result<U256, AmmError> {
    // 18-decimal scaled amount = balance * rate / 1e18.
    let bal = U256::from(balance);
    let scale = U256::from(rate);
    let scale_denom = U256::from(10u64).pow(U256::from(18));
    let prod = bal.checked_mul(scale).ok_or(AmmError::Overflow)?;
    Ok(prod / scale_denom)
}

fn u256_to_u128(x: U256) -> Result<u128, AmmError> {
    if x.bit_len() > 128 {
        Err(AmmError::Overflow)
    } else {
        let limbs = x.as_limbs();
        Ok((limbs[0] as u128) | ((limbs[1] as u128) << 64))
    }
}

fn compute_d(xp: &[U256], a_param: u64) -> Result<U256, AmmError> {
    let n = U256::from(xp.len() as u64);
    let mut s = U256::ZERO;
    for &v in xp {
        s = s.checked_add(v).ok_or(AmmError::Overflow)?;
    }
    if s.is_zero() {
        return Ok(U256::ZERO);
    }
    // `Ann = A_precise * N_COINS` — EXACTLY the deployed Curve StableSwap-NG `get_D`/`get_y`
    // (`Ann: uint256 = _amp * N_COINS`). Curve's on-chain `A` (what `A_precise()` returns, scaled by
    // A_PRECISION) is already the whitepaper's `A·n^(n-1)`, so `amp·N_COINS == A_white·n^n` — the
    // contract never raises `n` to the `n`. Using `n.pow(n)` here over-amplified by `n^(n-1)×`
    // (2× for 2-coin, 9× for 3-coin), flattening the curve and OVER-quoting (e.g. +2.27 bps on a
    // real crvUSD/USDC pool). Verified byte-exact vs on-chain `get_dy` — see the reference test.
    let ann: U256 = U256::from(a_param) * n;
    let a_precision = U256::from(A_PRECISION);
    // N^N for the split-form D_P divide (n ≤ MAX_COINS = 8 ⇒ n^n ≤ 8^8 fits u64).
    let n_pow_n = U256::from((xp.len() as u64).pow(xp.len() as u32));
    let mut d = s;
    for _ in 0..255 {
        let mut d_p = d;
        // Deployed Curve StableSwap-NG SPLIT form: `D_P = D_P*D/x` per coin, then a SINGLE
        // `D_P /= N^N` after the loop (verified verbatim vs CurveStableSwapNGMath.vy @2abe778).
        // The classic combined `/(x*N)` per coin is algebraically equal but floor-diverges from
        // the contract on heavily-imbalanced pools (proven by
        // `compute_d_is_byte_exact_vs_ng_split_form_over_wide_search`). `checked_mul` fails closed
        // on exactly the intermediates where the contract's uint256 `D_P*D` would revert.
        for &x in xp {
            d_p = d_p.checked_mul(d).ok_or(AmmError::Overflow)? / x;
        }
        d_p /= n_pow_n;
        let d_prev = d;
        let numerator = (ann * s / a_precision + d_p * n)
            .checked_mul(d)
            .ok_or(AmmError::Overflow)?;
        let denom = (ann - a_precision) * d / a_precision + (n + U256::from(1u64)) * d_p;
        if denom.is_zero() {
            return Err(AmmError::InvariantViolation);
        }
        d = numerator / denom;
        let abs_diff = if d > d_prev { d - d_prev } else { d_prev - d };
        if abs_diff <= U256::from(1u64) {
            return Ok(d);
        }
    }
    Err(AmmError::InvariantViolation)
}

fn get_y(
    n_coins: usize,
    a_param: u64,
    xp_old: &[U256],
    i: usize,
    j: usize,
    new_x_i: U256,
    d: U256,
) -> Result<U256, AmmError> {
    let n = U256::from(n_coins as u64);
    let ann = U256::from(a_param) * n;
    let a_precision = U256::from(A_PRECISION);
    // c = D, S = 0
    let mut c = d;
    let mut s = U256::ZERO;
    let mut x_k;
    for k in 0..n_coins {
        if k == j {
            continue;
        }
        x_k = if k == i { new_x_i } else { xp_old[k] };
        if x_k.is_zero() {
            return Err(AmmError::ZeroLiquidity);
        }
        s = s.checked_add(x_k).ok_or(AmmError::Overflow)?;
        c = c.checked_mul(d).ok_or(AmmError::Overflow)? / (x_k * n);
    }
    c = c.checked_mul(d).ok_or(AmmError::Overflow)? * a_precision / (ann * n);
    let b = s + d * a_precision / ann;
    let mut y = d;
    for _ in 0..255 {
        let y_prev = y;
        let numerator = y.checked_mul(y).ok_or(AmmError::Overflow)? + c;
        let denom = (y + y)
            .checked_add(b)
            .ok_or(AmmError::Overflow)?
            .checked_sub(d)
            .ok_or(AmmError::Underflow)?;
        if denom.is_zero() {
            return Err(AmmError::InvariantViolation);
        }
        y = numerator / denom;
        let abs_diff = if y > y_prev { y - y_prev } else { y_prev - y };
        if abs_diff <= U256::from(1u64) {
            return Ok(y);
        }
    }
    Err(AmmError::InvariantViolation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offpeg_multiplier_below_denominator_is_static_fee() {
        // offpeg_fee_multiplier <= FEE_DENOMINATOR (incl. 0) must reproduce the static-fee output
        // bit-for-bit — the back-compat guarantee for every pool snapshotted before the field.
        let base = StableSwapState {
            n_coins: 2,
            balances: [
                75_156_140_434_690_947_953_802,
                43_034_902_322,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [1_000_000_000_000_000_000, 10u128.pow(30), 0, 0, 0, 0, 0, 0],
            a_param: 100_000,
            fee: 1_000_000,
            offpeg_fee_multiplier: 0,
        };
        let with_unit = StableSwapState {
            offpeg_fee_multiplier: FEE_DENOMINATOR,
            ..base
        };
        let dx = 1_000_000_000_000_000_000u128;
        let a = get_dy(&base, 0, 1, dx).unwrap();
        let b = get_dy(&with_unit, 0, 1, dx).unwrap();
        assert_eq!(
            a, b,
            "multiplier <= FEE_DENOMINATOR must equal the static-fee result"
        );
    }

    #[test]
    fn offpeg_multiplier_only_lowers_output_on_imbalanced_pool() {
        // On the imbalanced crvUSD/USDC.e snapshot, a >1e10 multiplier raises the effective fee,
        // so the output must be STRICTLY LOWER than the static-fee result (never higher) — the
        // safe, more-conservative direction for a revert-on-shortfall executor.
        let base = StableSwapState {
            n_coins: 2,
            balances: [
                75_156_140_434_690_947_953_802,
                43_034_902_322,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [1_000_000_000_000_000_000, 10u128.pow(30), 0, 0, 0, 0, 0, 0],
            a_param: 100_000,
            fee: 1_000_000,
            offpeg_fee_multiplier: 0,
        };
        let dyn_pool = StableSwapState {
            offpeg_fee_multiplier: 5 * FEE_DENOMINATOR,
            ..base
        };
        let dx = 1_000_000_000_000_000_000u128;
        let static_out = get_dy(&base, 0, 1, dx).unwrap();
        let dyn_out = get_dy(&dyn_pool, 0, 1, dx).unwrap();
        assert!(
            dyn_out <= static_out,
            "dynamic offpeg fee must not raise output: dyn={dyn_out} static={static_out}"
        );
        assert!(
            dyn_out < static_out,
            "imbalanced pool with 5× multiplier should lower output"
        );
    }

    #[test]
    fn dynamic_fee_at_balance_equals_base() {
        // Perfectly balanced midpoint (xpi == xpj) ⇒ the multiplier term cancels ⇒ effective fee
        // == base fee, regardless of how large the multiplier is.
        let x = U256::from(1_000_000_000_000_000_000_000u128);
        assert_eq!(dynamic_fee(x, x, 1_000_000, 5 * FEE_DENOMINATOR), 1_000_000);
    }

    // NG get_D split-form byte-verification (GOAL 2026-07-05). Deployed Curve StableSwap-NG
    // get_D rounds D_P ONCE at /N^N (split): `D_P = D_P*D/x` per coin, then `D_P /= N^N` — NOT
    // the classic combined `/(x*N)` per coin (verified verbatim vs
    // curvefi/stableswap-ng CurveStableSwapNGMath.vy @2abe778). The two floor-diverge on
    // unbalanced pools, so the kernel must use split to stay byte-exact (the Curve-leg class).
    fn compute_d_form(xp: &[U256], a_param: u64, split: bool) -> U256 {
        let n = U256::from(xp.len() as u64);
        let nn = U256::from((xp.len() as u64).pow(xp.len() as u32));
        let mut s = U256::ZERO;
        for &v in xp {
            s += v;
        }
        if s.is_zero() {
            return U256::ZERO;
        }
        let ann = U256::from(a_param) * n;
        let ap = U256::from(A_PRECISION);
        let mut d = s;
        for _ in 0..255 {
            let mut d_p = d;
            for &x in xp {
                d_p = if split { d_p * d / x } else { d_p * d / (x * n) };
            }
            if split {
                d_p /= nn;
            }
            let d_prev = d;
            let num = (ann * s / ap + d_p * n) * d;
            let den = (ann - ap) * d / ap + (n + U256::from(1u64)) * d_p;
            d = num / den;
            let diff = if d > d_prev { d - d_prev } else { d_prev - d };
            if diff <= U256::from(1u64) {
                return d;
            }
        }
        d
    }

    // Byte-exactness of `compute_d` vs the deployed NG SPLIT D_P form, searched over a broad
    // deterministic space (LCG) of n-coin states incl. heavy imbalance and extreme A. The wide
    // search (2026-07-05) found real split≠combined divergences on heavily-imbalanced pools —
    // so `compute_d` was switched to the split form to match the deployed contract to the wei
    // (near-balanced pools, incl. the from-chain crvUSD/USDC vector, were already identical). This
    // guards that the U256 kernel stays byte-identical to the split reference wherever it converges.
    #[test]
    fn compute_d_is_byte_exact_vs_ng_split_form_over_wide_search() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut checked = 0u32;
        let mut divergences = 0u32;
        for _ in 0..20_000 {
            let n = 2 + (next() % 3) as usize; // 2..=4 coins
            // A in [1, 1_000_000], stored as A*A_PRECISION.
            let a = ((next() % 1_000_000) + 1) * A_PRECISION;
            let xp: Vec<U256> = (0..n)
                .map(|_| {
                    // Wide magnitude span incl. heavy imbalance: 1e6 .. 1e30-ish.
                    let mag = 6 + (next() % 25) as u32; // 10^6 .. 10^30
                    let base = (next() % 900 + 100) as u128; // 100..999 mantissa
                    U256::from(base) * U256::from(10u128).pow(U256::from(mag))
                })
                .collect();
            let got = match compute_d(&xp, a) {
                Ok(d) => d,
                Err(_) => continue, // overflow-guarded crafted state; skip
            };
            let split = compute_d_form(&xp, a, true);
            let combined = compute_d_form(&xp, a, false);
            checked += 1;
            if split != combined {
                divergences += 1;
            }
            assert_eq!(
                got, split,
                "compute_d diverged from the NG split reference (n={n}, a={a}, xp={xp:?})"
            );
        }
        assert!(checked > 5_000, "search must exercise many convergent states: {checked}");
        // Informational: how often the two algebraic forms even differ pre-convergence.
        eprintln!(
            "compute_d split-form check: {checked} states, {divergences} split≠combined \
             (compute_d matched split in ALL)"
        );
    }

    #[test]
    fn get_dy_matches_onchain_crvusd_usdc_reference_vector() {
        // FROM-CHAIN reference vector — Arbitrum crvUSD/USDC pool 0xec09…d93d @ block 465686728:
        //   A_precise=100000, balances=[165305241295285343678573, 109232458359], rates=[1e18,1e30],
        //   fee=1e6, offpeg_fee_multiplier=5e10. On-chain get_dy(0,1,1000e18) == 999442901 (cast).
        // Pins the `Ann = A_precise * N_COINS` invariant byte-exact. The prior `A * n^n` returned
        // 999669502 (+2.27 bps over-quote) — this test fails on that regression.
        let state = StableSwapState {
            n_coins: 2,
            balances: [
                165_305_241_295_285_343_678_573,
                109_232_458_359,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [1_000_000_000_000_000_000, 10u128.pow(30), 0, 0, 0, 0, 0, 0],
            a_param: 100_000,
            fee: 1_000_000,
            offpeg_fee_multiplier: 50_000_000_000,
        };
        let dy = get_dy(&state, 0, 1, 1_000_000_000_000_000_000_000u128).unwrap();
        assert_eq!(
            dy, 999_442_901,
            "kernel get_dy must byte-match on-chain (Ann = A_precise·N_COINS)"
        );
    }

    #[test]
    fn d_for_balanced_pool() {
        // 2-coin pool with equal balances and A = 200.
        let state = StableSwapState {
            n_coins: 2,
            balances: [
                1_000_000_000_000_000_000_000,
                1_000_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            a_param: 200 * A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        let xp: Vec<U256> = (0..state.n_coins)
            .map(|k| scaled(state.balances[k], state.rates[k]).unwrap())
            .collect();
        let d = compute_d(&xp, state.a_param).unwrap();
        let total: U256 = xp.iter().copied().sum();
        // For a perfectly-balanced pool D ≈ total.
        let diff = if d > total { d - total } else { total - d };
        assert!(
            diff <= U256::from(10u64),
            "D should ≈ total for balanced pool, d={d}, total={total}"
        );
    }

    #[test]
    fn get_dy_small_swap() {
        // 2-coin pool, balanced, small swap from coin 0 to coin 1.
        let state = StableSwapState {
            n_coins: 2,
            balances: [
                1_000_000_000_000_000_000_000,
                1_000_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            a_param: 200 * A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        let dy = get_dy(&state, 0, 1, 1_000_000_000_000_000_000).unwrap();
        // Expect approximately 1 unit out, slightly less due to fee + curve.
        assert!(dy > 990_000_000_000_000_000, "got {dy}");
        assert!(dy < 1_000_000_000_000_000_000, "got {dy}");
    }

    #[test]
    fn apply_exchange_updates_balances_and_is_price_impacting() {
        let state = StableSwapState {
            n_coins: 2,
            balances: [
                1_000_000_000_000_000_000_000,
                1_000_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            a_param: 200 * A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        let dx = 10_000_000_000_000_000_000u128; // 10 units, 0→1
        let (dy, new_state) = apply_exchange(&state, 0, 1, dx).unwrap();
        // dy is exactly the kernel quote.
        assert_eq!(dy, get_dy(&state, 0, 1, dx).unwrap());
        // Balances move by (+dx, -dy).
        assert_eq!(new_state.balances[0], state.balances[0] + dx);
        assert_eq!(new_state.balances[1], state.balances[1] - dy);
        // Other fields untouched.
        assert_eq!(new_state.a_param, state.a_param);
        assert_eq!(new_state.fee, state.fee);
        // Price impact: after the trade coin0 is more abundant, so the SAME 0→1 swap now yields
        // strictly less (the predicted state correctly reflects the worse marginal rate).
        let dy2 = get_dy(&new_state, 0, 1, dx).unwrap();
        assert!(dy2 < dy, "post-trade rate must be worse: dy2={dy2} dy={dy}");
    }

    #[test]
    fn apply_exchange_rejects_bad_indices() {
        let state = StableSwapState {
            n_coins: 2,
            balances: [1_000, 1_000, 0, 0, 0, 0, 0, 0],
            rates: [
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            a_param: 200 * A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        assert!(apply_exchange(&state, 0, 0, 1).is_err());
        assert!(apply_exchange(&state, 0, 1, 0).is_err());
    }

    #[test]
    fn get_dy_rejects_same_index() {
        let state = StableSwapState {
            n_coins: 2,
            balances: [1, 1, 0, 0, 0, 0, 0, 0],
            rates: [1, 1, 0, 0, 0, 0, 0, 0],
            a_param: 200 * A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        assert!(matches!(
            get_dy(&state, 0, 0, 1),
            Err(AmmError::InvariantViolation)
        ));
    }

    /// Manual T2.1 bench (RELEASE only):
    /// `cargo test -p engine-amm --release curve::tests::curve_d_cache_speedup -- --ignored --nocapture`
    ///
    /// Simulates a sizing walk (one pool, many `dx`): baseline `get_dy` recomputes the Newton `D`
    /// each probe; `get_dy_cached` reuses a precomputed `D`. Ratio = the fraction of `get_dy` that
    /// is the size-independent `D` solve (the saving the state-cache would realize per probe).
    #[test]
    #[ignore]
    fn curve_d_cache_speedup() {
        use std::time::Instant;
        let state = StableSwapState {
            n_coins: 2,
            balances: [
                75_156_140_434_690_947_953_802,
                43_034_902_322,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [1_000_000_000_000_000_000, 10u128.pow(30), 0, 0, 0, 0, 0, 0],
            a_param: 100_000,
            fee: 1_000_000,
            offpeg_fee_multiplier: 0,
        };
        let d = precompute_d(&state).unwrap();
        let iters = 500_000u64;
        let base_dx = 1_000_000_000_000_000_000u128;
        let t0 = Instant::now();
        let mut acc = 0u128;
        for k in 0..iters {
            let dx = base_dx + (k as u128) * 1_000_000_000_000u128;
            acc ^= get_dy(&state, 0, 1, dx).unwrap_or(0);
        }
        let base = t0.elapsed();
        let t1 = Instant::now();
        let mut acc2 = 0u128;
        for k in 0..iters {
            let dx = base_dx + (k as u128) * 1_000_000_000_000u128;
            acc2 ^= get_dy_cached(&state, 0, 1, dx, d).unwrap_or(0);
        }
        let cached = t1.elapsed();
        assert_eq!(acc, acc2); // identical outputs
        println!(
            "curve get_dy {} probes: baseline {:?} ({:.0} ns/q) → cached-D {:?} ({:.0} ns/q) = {:.2}× \
             ({:.0}% of get_dy is the D solve)",
            iters,
            base,
            base.as_nanos() as f64 / iters as f64,
            cached,
            cached.as_nanos() as f64 / iters as f64,
            base.as_nanos() as f64 / cached.as_nanos() as f64,
            100.0 * (1.0 - cached.as_nanos() as f64 / base.as_nanos() as f64),
        );
    }

    #[test]
    fn cached_d_matches_get_dy_byte_exact() {
        // T2.1: get_dy_cached with a precomputed D must equal get_dy bit-for-bit, and the ZERO
        // sentinel must reproduce get_dy too. Exercised on the imbalanced crvUSD/USDC.e snapshot.
        let state = StableSwapState {
            n_coins: 2,
            balances: [
                75_156_140_434_690_947_953_802,
                43_034_902_322,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [1_000_000_000_000_000_000, 10u128.pow(30), 0, 0, 0, 0, 0, 0],
            a_param: 100_000,
            fee: 1_000_000,
            offpeg_fee_multiplier: 3 * FEE_DENOMINATOR, // exercise the dynamic-fee path too
        };
        let d = precompute_d(&state).unwrap();
        assert!(!d.is_zero());
        for &dx in &[
            1u128,
            1_000_000u128,
            1_000_000_000_000_000_000u128,
            500_000_000_000_000_000_000u128,
            10u128.pow(24),
        ] {
            let baseline = get_dy(&state, 0, 1, dx);
            assert_eq!(
                get_dy_cached(&state, 0, 1, dx, d),
                baseline,
                "cached D, dx={dx}"
            );
            assert_eq!(
                get_dy_cached(&state, 0, 1, dx, U256::ZERO),
                baseline,
                "ZERO sentinel, dx={dx}"
            );
        }
        // And the reverse direction.
        let d_is_same = precompute_d(&state).unwrap();
        assert_eq!(d, d_is_same); // deterministic
        let rev = get_dy(&state, 1, 0, 1_000_000u128);
        assert_eq!(get_dy_cached(&state, 1, 0, 1_000_000u128, d), rev);
    }

    #[test]
    fn precompute_d_errors_match_get_dy_errors() {
        // n_coins < 2 → both error (precompute_d can't be used to bypass validation).
        let bad = StableSwapState {
            n_coins: 1,
            balances: [1_000, 0, 0, 0, 0, 0, 0, 0],
            rates: [1_000_000_000_000_000_000, 0, 0, 0, 0, 0, 0, 0],
            a_param: 200 * A_PRECISION,
            fee: 0,
            offpeg_fee_multiplier: 0,
        };
        assert!(precompute_d(&bad).is_err());
        assert!(get_dy(&bad, 0, 1, 1).is_err());
    }

    #[test]
    fn get_dy_rejects_zero_input() {
        let state = StableSwapState {
            n_coins: 2,
            balances: [1, 1, 0, 0, 0, 0, 0, 0],
            rates: [1, 1, 0, 0, 0, 0, 0, 0],
            a_param: 200 * A_PRECISION,
            fee: 4_000_000,
            offpeg_fee_multiplier: 0,
        };
        assert!(matches!(get_dy(&state, 0, 1, 0), Err(AmmError::ZeroInput)));
    }
}

/// K1/K2: randomized differential + property tests for the Curve StableSwap Newton kernel.
///
/// Plan invariant #8. The kernel runs the `D`/`get_y` Newton iterations in `U256` with overflow
/// guards; this module checks them against an **independent `U512` re-derivation of the same
/// iteration** (so any `U256` intermediate truncation or mis-placed overflow guard surfaces as a
/// mismatch) plus the **model invariants** a correct StableSwap must satisfy (`0 < D ≤ Σ xp`,
/// monotonic output, no panic). Inputs are bounded by the kernel's own `1e36` normalized-balance
/// guard so the `U512` oracle never itself overflows.
#[cfg(test)]
mod curve_differential {
    use super::*;
    use proptest::prelude::*;
    use ruint::aliases::U512;

    fn u512(x: U256) -> U512 {
        U512::from(x)
    }

    /// Independent `U512` re-derivation of `compute_d` — identical iteration structure, but every
    /// intermediate is 512-bit so it cannot overflow within the kernel's `1e36` input bound. When
    /// the kernel returns `Ok`, it MUST equal this (cast back to 256-bit).
    fn compute_d_u512(xp: &[U256], a_param: u64) -> Option<U512> {
        let n = U512::from(xp.len() as u64);
        let mut s = U512::ZERO;
        for &v in xp {
            s += u512(v);
        }
        if s.is_zero() {
            return Some(U512::ZERO);
        }
        // Mirror the kernel's `Ann = A_precise * N_COINS` (see compute_d). This U512 oracle only
        // proves U256↔U512 precision-equivalence; chain-correctness is pinned by the from-chain
        // reference vector test. (Both previously used `n^n` — self-consistently wrong, which is
        // why the differential test passed while the kernel over-quoted vs the real contract.)
        let ann = U512::from(a_param) * n;
        let a_precision = U512::from(A_PRECISION);
        // Mirror the kernel's NG SPLIT D_P form (see compute_d): `D_P*D/x` per coin then `/N^N`.
        let n_pow_n = U512::from((xp.len() as u64).pow(xp.len() as u32));
        let mut d = s;
        for _ in 0..255 {
            let mut d_p = d;
            for &x in xp {
                d_p = d_p * d / u512(x);
            }
            d_p /= n_pow_n;
            let d_prev = d;
            let numerator = (ann * s / a_precision + d_p * n) * d;
            let denom = (ann - a_precision) * d / a_precision + (n + U512::from(1u64)) * d_p;
            if denom.is_zero() {
                return None;
            }
            d = numerator / denom;
            let diff = if d > d_prev { d - d_prev } else { d_prev - d };
            if diff <= U512::from(1u64) {
                return Some(d);
            }
        }
        None
    }

    /// A balanced or mildly-imbalanced N-coin state in 18-dec units (already rate-1 so xp == bal).
    fn arb_state() -> impl Strategy<Value = StableSwapState> {
        (
            2usize..=4usize,      // n_coins
            1u64..=1_000_000u64,  // A (raw, pre-A_PRECISION)
            0u64..=10_000_000u64, // fee (0..0.1%)
            prop::collection::vec(
                1_000_000_000_000_000_000u128..=1_000_000_000_000_000_000_000_000u128,
                4,
            ),
        )
            .prop_map(|(n, a, fee, bals)| {
                let mut balances = [0u128; MAX_COINS];
                let mut rates = [0u128; MAX_COINS];
                for k in 0..n {
                    balances[k] = bals[k];
                    rates[k] = 1_000_000_000_000_000_000; // rate = 1e18 ⇒ xp == balance
                }
                StableSwapState {
                    n_coins: n,
                    balances,
                    rates,
                    a_param: a * A_PRECISION,
                    fee,
                    offpeg_fee_multiplier: 0,
                }
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1500))]

        /// `compute_d` (U256) is byte-identical to the U512 re-derivation whenever the kernel
        /// returns Ok — i.e. the U256 fast path never truncates or mis-converges vs full precision.
        #[test]
        fn compute_d_matches_u512(state in arb_state()) {
            let xp: Vec<U256> = (0..state.n_coins)
                .map(|k| U256::from(state.balances[k]))
                .collect();
            if let Ok(d) = compute_d(&xp, state.a_param) {
                let dref = compute_d_u512(&xp, state.a_param);
                prop_assert_eq!(Some(u512(d)), dref, "U256 compute_d diverged from U512 oracle");
            }
        }

        /// Fundamental StableSwap bound: `0 < D ≤ Σ xp` (equality only at perfect balance; by AM-GM
        /// the invariant D never exceeds the constant-sum value). Catches a Newton overshoot.
        #[test]
        fn d_is_bounded_by_sum(state in arb_state()) {
            let xp: Vec<U256> = (0..state.n_coins)
                .map(|k| U256::from(state.balances[k]))
                .collect();
            if let Ok(d) = compute_d(&xp, state.a_param) {
                let sum: U256 = xp.iter().copied().fold(U256::ZERO, |a, b| a + b);
                prop_assert!(d > U256::ZERO);
                prop_assert!(d <= sum, "D={d} exceeded Σxp={sum}");
            }
        }

        /// `get_dy` never panics, and a larger input never produces a smaller output.
        #[test]
        fn get_dy_monotonic_and_total(
            state in arb_state(),
            dx in 1_000_000_000_000u128..=1_000_000_000_000_000_000_000u128,
            bump in 1u128..=1_000_000_000_000_000_000_000u128,
        ) {
            // Pick a valid (i, j) within n_coins.
            let i = 0usize;
            let j = 1usize;
            let o1 = get_dy(&state, i, j, dx);
            let o2 = get_dy(&state, i, j, dx.saturating_add(bump));
            if let (Ok(d1), Ok(d2)) = (o1, o2) {
                prop_assert!(d2 >= d1, "more dx ⇒ not less dy ({d1} → {d2})");
            }
        }

        /// T2.1 byte-exactness over random states: `get_dy_cached` with a precomputed `D` equals
        /// `get_dy` for every state/size the kernel accepts. This is the safety guard for the
        /// state-cached D wiring — a cached invariant can never change a quote.
        #[test]
        fn cached_d_is_byte_exact(
            state in arb_state(),
            dx in 1_000_000_000_000u128..=1_000_000_000_000_000_000_000u128,
        ) {
            if let (Ok(d), Ok(expected)) = (precompute_d(&state), get_dy(&state, 0, 1, dx)) {
                prop_assert_eq!(get_dy_cached(&state, 0, 1, dx, d).ok(), Some(expected));
            }
        }

        /// Balanced 2-coin, fee-free: a small swap returns slightly LESS than it puts in (slippage,
        /// minus the −1 Curve rounding), never more — a constant-product/stableswap sanity bound.
        #[test]
        fn balanced_swap_no_free_lunch(
            bal in 1_000_000_000_000_000_000_000u128..=1_000_000_000_000_000_000_000_000u128,
            a in 1u64..=5_000u64,
            dx in 1_000_000_000_000u128..=1_000_000_000_000_000_000u128,
        ) {
            let state = StableSwapState {
                n_coins: 2,
                balances: [bal, bal, 0, 0, 0, 0, 0, 0],
                rates: [1_000_000_000_000_000_000, 1_000_000_000_000_000_000, 0, 0, 0, 0, 0, 0],
                a_param: a * A_PRECISION,
                fee: 0,
                offpeg_fee_multiplier: 0,
            };
            if let Ok(dy) = get_dy(&state, 0, 1, dx) {
                prop_assert!(dy <= dx, "balanced fee-free swap returned more than input: dx={dx} dy={dy}");
            }
        }
    }
}
