//! Curve **TwoCrypto-NG** (2-coin) exact integer math.
//!
//! Bounded integer-only port of Curve's official twocrypto-ng `TwocryptoMath.vy` + the pool-side
//! `_exchange`/`_fee` logic from `Twocrypto.vy`. Sibling of [`crate::curve_cryptoswap`] (the
//! 3-coin Tricrypto-NG kernel) — this completes Curve crypto coverage for the 2-coin pools the
//! Tricrypto kernel rejects. Verified against the same hand-derived reference vectors as the
//! Shadow/Fulcrum reference implementation (`curve_twocrypto_fixed.rs`).
//!
//! Caveats (unchanged from the reference):
//! * Models current pool state only — no EMA/oracle upkeep or `tweak_price` rebalancing.
//! * `get_y_exact()` returns Curve's `K0_prev` helper exactly as the official solver does; it is
//!   threaded back into `newton_d_exact()` and must NOT be reinterpreted as the raw `K0`.

use alloy_primitives::{I256, U256, U512};

const N_COINS: u64 = 2;
const N_COINS_SQUARED: u64 = 4;
const A_MULTIPLIER: u64 = 10_000;
const PRECISION: u128 = 1_000_000_000_000_000_000;
const MIN_GAMMA: u128 = 10_000_000_000;
const MAX_GAMMA_SMALL: u128 = 20_000_000_000_000_000;
const MAX_GAMMA: u128 = 199_000_000_000_000_000;
const MIN_A: u128 = 4_000;
// A3: official twocrypto-ng `MAX_A = N_COINS²·A_MULTIPLIER·1000` = 4·10_000·1000 = 40_000_000.
// (Was a fail-OPEN 4_000_000_000 — ~100× the on-chain ceiling.)
const MAX_A: u128 = (N_COINS_SQUARED as u128) * (A_MULTIPLIER as u128) * 1000;
const MIN_D: u128 = 100_000_000_000_000_000;
const MAX_D: u128 = 1_000_000_000_000_000u128 * PRECISION;

/// TwoCrypto-NG math errors. Fail-closed on every non-convergence, overflow, and out-of-band
/// parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TwoCryptoSwapError {
    /// Newton iteration did not converge within 255 iterations.
    DidNotConverge,
    /// Parameter outside Curve's safety band.
    InvalidParams(&'static str),
    /// Intermediate value drifted into an unsafe / fail-closed region.
    UnsafeValues(&'static str),
    /// Coin index out of range.
    IndexOutOfRange,
    /// Arithmetic overflow.
    Overflow,
}

/// 2-coin TwoCrypto-NG pool state. `balances` are real on-chain reserves; `precisions` scale each
/// coin to 1e18; `price_scale` reprices coin1 against coin0 (the numeraire).
#[derive(Debug, Clone)]
pub struct TwoCryptoSwapState {
    /// Real on-chain reserves per coin.
    pub balances: [U256; 2],
    /// Per-coin scale to 1e18-units xp space.
    pub precisions: [U256; 2],
    /// Pool-internal repricing factor coin1/coin0 (1e18-scaled).
    pub price_scale: U256,
    /// `A` in ANN form (fed directly into newton_D).
    pub ann: U256,
    /// CryptoSwap gamma (1e18-scaled).
    pub gamma: U256,
    /// Dynamic fee mid-band (1e10-scaled).
    pub mid_fee: U256,
    /// Dynamic fee outer-band (1e10-scaled).
    pub out_fee: U256,
    /// Fee gamma.
    pub fee_gamma: U256,
    /// Solved invariant — populated by [`Self::recompute_d`].
    pub d: U256,
    /// `k0_prev` warm-start (0 = cold).
    pub last_k0_prev: U256,
}

impl TwoCryptoSwapState {
    /// Repriced balances in 1e18-units.
    #[inline]
    pub fn xp(&self) -> Result<[U256; 2], TwoCryptoSwapError> {
        Ok([
            checked_mul(self.balances[0], self.precisions[0])?,
            checked_div(
                checked_mul(
                    checked_mul(self.balances[1], self.precisions[1])?,
                    self.price_scale,
                )?,
                precision(),
            )?,
        ])
    }

    /// Re-solve invariant `D` from current `xp`.
    pub fn recompute_d(&mut self) -> Result<U256, TwoCryptoSwapError> {
        self.d = newton_d_exact(self.ann, self.gamma, &self.xp()?, self.last_k0_prev)?;
        Ok(self.d)
    }

    /// Dynamic fee: blends mid_fee ↔ out_fee by imbalance.
    #[inline]
    pub fn dynamic_fee(&self, xp: &[U256; 2]) -> U256 {
        let k = reduction_coefficient(xp, self.fee_gamma);
        (self.mid_fee * k + self.out_fee * (precision() - k)) / precision()
    }

    /// Quote a swap `i → j` for `dx` real-units of coin `i`. Returns
    /// `(dy_after_fee, fee_amount, k0_prev)`.
    pub fn get_amount_out_exact(
        &self,
        i: usize,
        j: usize,
        dx: U256,
    ) -> Result<(U256, U256, U256), TwoCryptoSwapError> {
        if i >= N_COINS as usize || j >= N_COINS as usize || i == j {
            return Err(TwoCryptoSwapError::IndexOutOfRange);
        }
        if dx.is_zero() {
            return Err(TwoCryptoSwapError::InvalidParams("dx is zero"));
        }

        let mut xp = self.xp()?;
        let xp_before_j = xp[j];
        xp[i] = scale_balance(
            self.balances[i] + dx,
            self.precisions[i],
            i,
            self.price_scale,
        )?;

        let (y_out, k0_prev) = get_y_exact(self.ann, self.gamma, &xp, self.d, j)?;
        let dy_scaled = xp_before_j
            .checked_sub(y_out)
            .ok_or(TwoCryptoSwapError::UnsafeValues("negative dy"))?
            .checked_sub(U256::ONE)
            .ok_or(TwoCryptoSwapError::UnsafeValues("dy rounded to zero"))?;

        let dy_real = descale_amount(dy_scaled, self.precisions[j], j, self.price_scale)?;

        let mut xp_fee = xp;
        xp_fee[j] = xp_fee[j]
            .checked_sub(dy_scaled)
            .ok_or(TwoCryptoSwapError::UnsafeValues("xp_fee underflow"))?;
        let fee_rate = self.dynamic_fee(&xp_fee);
        let fee_amount = checked_div(checked_mul(fee_rate, dy_real)?, u256(10_000_000_000u64))?;
        let dy_after_fee = dy_real
            .checked_sub(fee_amount)
            .ok_or(TwoCryptoSwapError::UnsafeValues("fee > dy"))?;

        Ok((dy_after_fee, fee_amount, k0_prev))
    }

    /// Forward-apply a swap `i → j` of `dx` real-units: returns `(dy_after_fee, new_state)` with
    /// `balances[i] += dx`, `balances[j] -= dy_after_fee` (fee retained in pool), and `D` re-solved
    /// from the new balances. Mirrors the on-chain `exchange` transition for the common no-repeg
    /// case (`price_scale` is held — its rate-limited repeg is re-priced exactly by the downstream
    /// REVM validation). See the 3-coin twin for the full capital-safety rationale.
    pub fn apply_exchange(
        &self,
        i: usize,
        j: usize,
        dx: U256,
    ) -> Result<(U256, TwoCryptoSwapState), TwoCryptoSwapError> {
        let (dy_after_fee, _fee, k0) = self.get_amount_out_exact(i, j, dx)?;
        let mut new = self.clone();
        new.balances[i] = new.balances[i]
            .checked_add(dx)
            .ok_or(TwoCryptoSwapError::Overflow)?;
        new.balances[j] = new.balances[j]
            .checked_sub(dy_after_fee)
            .ok_or(TwoCryptoSwapError::UnsafeValues("dy exceeds balance"))?;
        new.last_k0_prev = k0;
        new.recompute_d()?;
        Ok((dy_after_fee, new))
    }
}

/// Solve the invariant `D` via Newton iteration.
pub fn newton_d_exact(
    ann: U256,
    gamma: U256,
    x_unsorted: &[U256; 2],
    k0_prev: U256,
) -> Result<U256, TwoCryptoSwapError> {
    validate_a_gamma(ann, gamma)?;

    let mut x = *x_unsorted;
    sort_descending_2(&mut x);
    let x0_max = pow10(33);
    if x[0] < pow10(9) || x[0] > x0_max {
        return Err(TwoCryptoSwapError::UnsafeValues("unsafe values x[0]"));
    }
    let ratio = checked_div(checked_mul(x[1], precision())?, x[0])?;
    if ratio <= pow10(14) - U256::ONE {
        return Err(TwoCryptoSwapError::UnsafeValues(
            "unsafe values x[i] (input)",
        ));
    }

    let s = x[0].checked_add(x[1]).ok_or(TwoCryptoSwapError::Overflow)?;
    let mut d = if k0_prev.is_zero() {
        checked_mul(u256(N_COINS), geometric_mean_exact(&x)?)?
    } else {
        sqrt_from_k0_prev(x[0], x[1], k0_prev)?
    };
    if s < d {
        d = s;
    }

    let g1k0_base = gamma
        .checked_add(precision())
        .ok_or(TwoCryptoSwapError::Overflow)?;
    for _ in 0..255 {
        let d_prev = d;
        if d.is_zero() {
            return Err(TwoCryptoSwapError::UnsafeValues("D==0"));
        }

        let k0 = checked_div(
            checked_mul(
                checked_div(
                    checked_mul(checked_mul(precision(), u256(N_COINS_SQUARED))?, x[0])?,
                    d,
                )?,
                x[1],
            )?,
            d,
        )?;
        let g1k0 = abs_diff(g1k0_base, k0)
            .checked_add(U256::ONE)
            .ok_or(TwoCryptoSwapError::Overflow)?;

        let mul1 = checked_div(
            checked_mul(
                checked_mul(checked_div(checked_mul(precision(), d)?, gamma)?, g1k0)?,
                checked_div(checked_mul(g1k0, u256(A_MULTIPLIER))?, gamma)?,
            )?,
            ann,
        )?;
        let mul2 = checked_div(
            checked_mul(
                checked_mul(checked_mul(u256(2u64), precision())?, u256(N_COINS))?,
                k0,
            )?,
            g1k0,
        )?;

        let neg_fprime = checked_sub(
            checked_add(
                checked_add(s, checked_div(checked_mul(s, mul2)?, precision())?)?,
                checked_div(checked_mul(mul1, u256(N_COINS))?, k0)?,
            )?,
            checked_div(checked_mul(mul2, d)?, precision())?,
        )?;
        let d_plus = checked_div(checked_mul(d, neg_fprime + s)?, neg_fprime)?;
        let mut d_minus = checked_div(checked_mul(d, d)?, neg_fprime)?;
        let mul1_term = checked_div(checked_mul(d, checked_div(mul1, neg_fprime)?)?, precision())?;
        if precision() > k0 {
            d_minus = d_minus
                .checked_add(checked_div(checked_mul(mul1_term, precision() - k0)?, k0)?)
                .ok_or(TwoCryptoSwapError::Overflow)?;
        } else {
            d_minus = d_minus
                .checked_sub(checked_div(checked_mul(mul1_term, k0 - precision())?, k0)?)
                .ok_or(TwoCryptoSwapError::UnsafeValues("D update underflow"))?;
        }

        d = if d_plus > d_minus {
            d_plus - d_minus
        } else {
            checked_div(d_minus - d_plus, u256(2u64))?
        };

        let diff = abs_diff(d, d_prev);
        if checked_mul(diff, pow10(14))? < max_u256(pow10(16), d) {
            for xi in x {
                let frac = checked_div(checked_mul(xi, precision())?, d)?;
                if frac <= (checked_div(pow10(16), u256(N_COINS))? - U256::ONE)
                    || frac >= (checked_div(pow10(20), u256(N_COINS))? + U256::ONE)
                {
                    return Err(TwoCryptoSwapError::UnsafeValues("unsafe values x[i]"));
                }
            }
            return Ok(d);
        }
    }

    Err(TwoCryptoSwapError::DidNotConverge)
}

/// Iterative Newton solve for `y` (the fallback path; `get_y_exact` is the analytic primary).
pub fn newton_y_exact(
    ann: U256,
    gamma: U256,
    x: &[U256; 2],
    d: U256,
    i: usize,
) -> Result<U256, TwoCryptoSwapError> {
    validate_a_gamma(ann, gamma)?;
    validate_d(d)?;
    if i >= N_COINS as usize {
        return Err(TwoCryptoSwapError::IndexOutOfRange);
    }

    let lim_mul = lim_mul(gamma)?;
    let y = newton_y_inner(ann, gamma, x, d, i, lim_mul)?;
    validate_y_bounds(y, d, lim_mul)?;
    Ok(y)
}

/// Analytic cubic solve for `y` (Curve's official `get_y`). Returns `(y, k0_prev)`.
pub fn get_y_exact(
    ann: U256,
    gamma: U256,
    x: &[U256; 2],
    d: U256,
    i: usize,
) -> Result<(U256, U256), TwoCryptoSwapError> {
    validate_a_gamma(ann, gamma)?;
    validate_d(d)?;
    if i >= N_COINS as usize {
        return Err(TwoCryptoSwapError::IndexOutOfRange);
    }

    let lim_mul = lim_mul(gamma)?;
    let lim_mul_signed = i256_from_u256(lim_mul)?;
    let ann_i = i256_from_u256(ann)?;
    let gamma_i = i256_from_u256(gamma)?;
    let d_i = i256_from_u256(d)?;
    let x_j = i256_from_u256(x[1 - i])?;
    let gamma2 = imul(gamma_i, gamma_i)?;
    let k0_i = idiv(
        imul(
            imul(i256_from_u256(precision())?, i256(N_COINS as i128))?,
            x_j,
        )?,
        d_i,
    )?;
    let lower_k0 = idiv(i256_from_u256(pow10(36))?, lim_mul_signed)?;
    if k0_i < lower_k0 || k0_i > lim_mul_signed {
        return Err(TwoCryptoSwapError::UnsafeValues("unsafe values x[i]"));
    }

    let ann_gamma2 = imul(ann_i, gamma2)?;
    let a = i256_from_u256(pow10(32))?;
    let b = isub(
        isub(
            idiv(idiv(imul(d_i, ann_gamma2)?, i256(400_000_000))?, x_j)?,
            imul(i256(3), a)?,
        )?,
        imul(imul(i256(2), gamma_i)?, i256_from_u256(pow10(14))?)?,
    )?;
    let c = isub(
        iadd(
            iadd(
                iadd(
                    imul(i256(3), a)?,
                    imul(imul(i256(4), gamma_i)?, i256_from_u256(pow10(14))?)?,
                )?,
                idiv(gamma2, i256_from_u256(pow10(4))?)?,
            )?,
            idiv(
                imul(idiv(imul(i256(4), ann_gamma2)?, i256(400_000_000))?, x_j)?,
                d_i,
            )?,
        )?,
        idiv(imul(i256(4), ann_gamma2)?, i256(400_000_000))?,
    )?;
    let d_term = ineg(idiv(
        imul(
            iadd(i256_from_u256(precision())?, gamma_i)?,
            iadd(i256_from_u256(precision())?, gamma_i)?,
        )?,
        i256_from_u256(pow10(4))?,
    )?)?;

    let mut delta0 = isub(idiv(imul(imul(i256(3), a)?, c)?, b)?, b)?;
    let mut delta1 = isub(
        iadd(imul(i256(3), delta0)?, b)?,
        idiv(imul(idiv(imul(i256(27), imul(a, a)?)?, b)?, d_term)?, b)?,
    )?;

    let threshold = min_u256(
        min_u256(delta0.unsigned_abs(), delta1.unsigned_abs()),
        a.unsigned_abs(),
    );
    let divider = divider_for_threshold(threshold);
    let divider_i = i256_from_u256(divider)?;

    let a = idiv(a, divider_i)?;
    let b = idiv(b, divider_i)?;
    let c = idiv(c, divider_i)?;
    let d_term = idiv(d_term, divider_i)?;
    delta0 = isub(idiv(imul(imul(i256(3), a)?, c)?, b)?, b)?;
    delta1 = isub(
        iadd(imul(i256(3), delta0)?, b)?,
        idiv(imul(idiv(imul(i256(27), imul(a, a)?)?, b)?, d_term)?, b)?,
    )?;

    let sqrt_arg = iadd(
        imul(delta1, delta1)?,
        imul(idiv(imul(imul(i256(4), delta0)?, delta0)?, b)?, delta0)?,
    )?;
    if sqrt_arg <= I256::ZERO {
        let y = newton_y_inner(ann, gamma, x, d, i, lim_mul)?;
        validate_y_bounds(y, d, lim_mul)?;
        return Ok((y, U256::ZERO));
    }

    let sqrt_val = i256_from_u256(u256_from_u512(isqrt_u512(U512::from(
        sqrt_arg.unsigned_abs(),
    )))?)?;
    let b_cbrt = signed_cbrt(b)?;
    let second_cbrt = if delta1 > I256::ZERO {
        signed_cbrt(idiv(iadd(delta1, sqrt_val)?, i256(2))?)?
    } else {
        ineg(signed_cbrt(idiv(isub(sqrt_val, delta1)?, i256(2))?)?)?
    };
    let c1 = idiv(
        imul(
            idiv(imul(b_cbrt, b_cbrt)?, i256_from_u256(precision())?)?,
            second_cbrt,
        )?,
        i256_from_u256(precision())?,
    )?;
    if c1.is_zero() {
        return Err(TwoCryptoSwapError::UnsafeValues("C1 is zero"));
    }

    let root = idiv(
        isub(
            isub(
                imul(i256_from_u256(precision())?, c1)?,
                imul(i256_from_u256(precision())?, b)?,
            )?,
            imul(idiv(imul(i256_from_u256(precision())?, b)?, c1)?, delta0)?,
        )?,
        imul(i256(3), a)?,
    )?;
    if root <= I256::ZERO {
        return Err(TwoCryptoSwapError::UnsafeValues("root <= 0"));
    }

    let y_out = idiv(
        idiv(imul(idiv(imul(d_i, d_i)?, x_j)?, root)?, i256(4))?,
        i256_from_u256(precision())?,
    )?;
    let y_out = u256_from_non_negative_i256(y_out)?;
    validate_y_bounds(y_out, d, lim_mul)?;

    Ok((y_out, u256_from_non_negative_i256(root)?))
}

/// Geometric mean of the two repriced balances.
pub fn geometric_mean_exact(x: &[U256; 2]) -> Result<U256, TwoCryptoSwapError> {
    u256_from_u512(isqrt_u512(U512::from(x[0]) * U512::from(x[1])))
}

/// Dynamic-fee reduction coefficient (drops as imbalance grows).
pub fn reduction_coefficient(x: &[U256; 2], fee_gamma: U256) -> U256 {
    let s = x[0] + x[1];
    if s.is_zero() {
        return U256::ZERO;
    }
    let mut k = precision() * u256(N_COINS) * x[0] / s;
    k = k * u256(N_COINS) * x[1] / s;
    if !fee_gamma.is_zero() {
        k = fee_gamma * precision() / (fee_gamma + precision() - k);
    }
    k
}

fn newton_y_inner(
    ann: U256,
    gamma: U256,
    x: &[U256; 2],
    d: U256,
    i: usize,
    lim_mul: U256,
) -> Result<U256, TwoCryptoSwapError> {
    let x_j = x[1 - i];
    let mut y = checked_div(checked_mul(d, d)?, checked_mul(x_j, u256(N_COINS_SQUARED))?)?;
    let k0_i = checked_div(
        checked_mul(checked_mul(precision(), u256(N_COINS))?, x_j)?,
        d,
    )?;
    let lower_k0 = checked_div(pow10(36), lim_mul)?;
    if k0_i < lower_k0 || k0_i > lim_mul {
        return Err(TwoCryptoSwapError::UnsafeValues("unsafe values x[i]"));
    }

    let convergence_limit = max_u256(max_u256(x_j / pow10(14), d / pow10(14)), u256(100u64));
    for _ in 0..255 {
        let y_prev = y;
        let k0 = checked_div(checked_mul(checked_mul(k0_i, y)?, u256(N_COINS))?, d)?;
        let s = x_j.checked_add(y).ok_or(TwoCryptoSwapError::Overflow)?;

        let g1k0 = abs_diff(
            gamma
                .checked_add(precision())
                .ok_or(TwoCryptoSwapError::Overflow)?,
            k0,
        )
        .checked_add(U256::ONE)
        .ok_or(TwoCryptoSwapError::Overflow)?;

        let mul1 = checked_div(
            checked_mul(
                checked_mul(checked_div(checked_mul(precision(), d)?, gamma)?, g1k0)?,
                checked_div(checked_mul(g1k0, u256(A_MULTIPLIER))?, gamma)?,
            )?,
            ann,
        )?;
        let mul2 = precision()
            .checked_add(checked_div(
                checked_mul(checked_mul(u256(2u64), precision())?, k0)?,
                g1k0,
            )?)
            .ok_or(TwoCryptoSwapError::Overflow)?;
        let mut yfprime = precision()
            .checked_mul(y)
            .ok_or(TwoCryptoSwapError::Overflow)?
            .checked_add(checked_mul(s, mul2)?)
            .ok_or(TwoCryptoSwapError::Overflow)?
            .checked_add(mul1)
            .ok_or(TwoCryptoSwapError::Overflow)?;
        let dyfprime = checked_mul(d, mul2)?;
        if yfprime < dyfprime {
            y = checked_div(y_prev, u256(2u64))?;
            continue;
        }
        yfprime -= dyfprime;

        let fprime = checked_div(yfprime, y)?;
        let mut y_minus = checked_div(mul1, fprime)?;
        let y_plus = checked_add(
            checked_div(
                yfprime
                    .checked_add(checked_mul(precision(), d)?)
                    .ok_or(TwoCryptoSwapError::Overflow)?,
                fprime,
            )?,
            checked_div(checked_mul(y_minus, precision())?, k0)?,
        )?;
        y_minus = y_minus
            .checked_add(checked_div(checked_mul(precision(), s)?, fprime)?)
            .ok_or(TwoCryptoSwapError::Overflow)?;
        y = if y_plus < y_minus {
            checked_div(y_prev, u256(2u64))?
        } else {
            y_plus - y_minus
        };

        let diff = abs_diff(y, y_prev);
        if diff < max_u256(convergence_limit, y / pow10(14)) {
            return Ok(y);
        }
    }

    Err(TwoCryptoSwapError::DidNotConverge)
}

#[inline]
fn validate_a_gamma(ann: U256, gamma: U256) -> Result<(), TwoCryptoSwapError> {
    if ann < u256(MIN_A) || ann > u256(MAX_A) {
        return Err(TwoCryptoSwapError::InvalidParams("ANN out of range"));
    }
    if gamma < u256(MIN_GAMMA) || gamma > u256(MAX_GAMMA) {
        return Err(TwoCryptoSwapError::InvalidParams("gamma out of range"));
    }
    Ok(())
}

#[inline]
fn validate_d(d: U256) -> Result<(), TwoCryptoSwapError> {
    if d < u256(MIN_D) || d > u256(MAX_D) {
        return Err(TwoCryptoSwapError::InvalidParams("D out of range"));
    }
    Ok(())
}

#[inline]
fn validate_y_bounds(y: U256, d: U256, lim_mul: U256) -> Result<(), TwoCryptoSwapError> {
    let frac = checked_div(checked_mul(y, precision())?, d)?;
    let lower = checked_div(checked_div(pow10(36), u256(N_COINS))?, lim_mul)?;
    let upper = checked_div(lim_mul, u256(N_COINS))?;
    if frac < lower || frac > upper {
        return Err(TwoCryptoSwapError::UnsafeValues("unsafe value for y"));
    }
    Ok(())
}

#[inline]
fn lim_mul(gamma: U256) -> Result<U256, TwoCryptoSwapError> {
    let mut lim = checked_mul(u256(100u64), precision())?;
    if gamma > u256(MAX_GAMMA_SMALL) {
        lim = checked_div(checked_mul(lim, u256(MAX_GAMMA_SMALL))?, gamma)?;
    }
    Ok(lim)
}

#[inline]
fn scale_balance(
    balance: U256,
    precision_scale: U256,
    idx: usize,
    price_scale: U256,
) -> Result<U256, TwoCryptoSwapError> {
    if idx == 0 {
        checked_mul(balance, precision_scale)
    } else {
        checked_div(
            checked_mul(checked_mul(balance, precision_scale)?, price_scale)?,
            precision(),
        )
    }
}

#[inline]
fn descale_amount(
    amount: U256,
    precision_scale: U256,
    idx: usize,
    price_scale: U256,
) -> Result<U256, TwoCryptoSwapError> {
    if idx == 0 {
        checked_div(amount, precision_scale)
    } else {
        checked_div(
            checked_mul(amount, precision())?,
            checked_mul(price_scale, precision_scale)?,
        )
    }
}

#[inline]
fn sqrt_from_k0_prev(x0: U256, x1: U256, k0_prev: U256) -> Result<U256, TwoCryptoSwapError> {
    let numerator = (U512::from(4u64) * U512::from(x0) * U512::from(x1)) / U512::from(k0_prev);
    let scaled = numerator * U512::from(precision());
    let root = isqrt_u512(scaled);
    if root > U512::from(U256::MAX) {
        return Err(TwoCryptoSwapError::Overflow);
    }
    Ok(U256::from(root))
}

#[inline]
fn divider_for_threshold(threshold: U256) -> U256 {
    if threshold > pow10(48) {
        pow10(30)
    } else if threshold > pow10(46) {
        pow10(28)
    } else if threshold > pow10(44) {
        pow10(26)
    } else if threshold > pow10(42) {
        pow10(24)
    } else if threshold > pow10(40) {
        pow10(22)
    } else if threshold > pow10(38) {
        pow10(20)
    } else if threshold > pow10(36) {
        pow10(18)
    } else if threshold > pow10(34) {
        pow10(16)
    } else if threshold > pow10(32) {
        pow10(14)
    } else if threshold > pow10(30) {
        pow10(12)
    } else if threshold > pow10(28) {
        pow10(10)
    } else if threshold > pow10(26) {
        pow10(8)
    } else if threshold > pow10(24) {
        pow10(6)
    } else if threshold > pow10(20) {
        pow10(2)
    } else {
        U256::ONE
    }
}

#[inline]
fn signed_cbrt(value: I256) -> Result<I256, TwoCryptoSwapError> {
    if value.is_zero() {
        return Ok(I256::ZERO);
    }
    let root = i256_from_u256(curve_cbrt(value.unsigned_abs())?)?;
    if value < I256::ZERO {
        ineg(root)
    } else {
        Ok(root)
    }
}

fn curve_cbrt(x: U256) -> Result<U256, TwoCryptoSwapError> {
    if x.is_zero() {
        return Ok(U256::ZERO);
    }
    let threshold_hi = U256::MAX / pow10(18);
    let threshold_mid = U256::MAX / pow10(36);
    let xx = if x >= threshold_hi {
        x
    } else if x >= threshold_mid {
        checked_mul(x, pow10(18))?
    } else {
        checked_mul(x, pow10(36))?
    };

    let log2x = snekmate_log_2(xx, false);
    let remainder = (log2x % u256(3u64)).as_limbs()[0] as usize;
    let pow = (log2x / u256(3u64)).as_limbs()[0] as usize;
    let mut a = checked_div(
        checked_mul(pow2_u256(pow), pow_u256(u256(1260u64), remainder)?)?,
        pow_u256(u256(1000u64), remainder)?,
    )?;

    for _ in 0..7 {
        a = checked_div(
            checked_add(
                checked_mul(u256(2u64), a)?,
                checked_div(xx, checked_mul(a, a)?)?,
            )?,
            u256(3u64),
        )?;
    }

    if x >= threshold_hi {
        a = checked_mul(a, pow10(12))?;
    } else if x >= threshold_mid {
        a = checked_mul(a, pow10(6))?;
    }

    Ok(a)
}

fn isqrt_u512(n: U512) -> U512 {
    if n.is_zero() {
        return U512::ZERO;
    }
    let one = U512::from(1u64);
    let two = U512::from(2u64);
    let mut x = n;
    let mut y = (x + one) / two;
    while y < x {
        x = y;
        y = (n / x + x) / two;
    }
    x
}

#[inline]
fn u256_from_u512(v: U512) -> Result<U256, TwoCryptoSwapError> {
    if v > U512::from(U256::MAX) {
        return Err(TwoCryptoSwapError::Overflow);
    }
    let limbs = v.into_limbs();
    Ok(U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}

fn snekmate_log_2(x: U256, roundup: bool) -> U256 {
    let mut value = x;
    let mut result = U256::ZERO;
    if x >> 128 != U256::ZERO {
        value = x >> 128;
        result = u256(128u64);
    }
    if value >> 64 != U256::ZERO {
        value >>= 64;
        result += u256(64u64);
    }
    if value >> 32 != U256::ZERO {
        value >>= 32;
        result += u256(32u64);
    }
    if value >> 16 != U256::ZERO {
        value >>= 16;
        result += u256(16u64);
    }
    if value >> 8 != U256::ZERO {
        value >>= 8;
        result += u256(8u64);
    }
    if value >> 4 != U256::ZERO {
        value >>= 4;
        result += u256(4u64);
    }
    if value >> 2 != U256::ZERO {
        value >>= 2;
        result += u256(2u64);
    }
    if value >> 1 != U256::ZERO {
        result += u256(1u64);
    }
    let shift = result.as_limbs()[0] as usize;
    if roundup && (U256::ONE << shift) < x {
        result += u256(1u64);
    }
    result
}

fn pow2_u256(exp: usize) -> U256 {
    U256::ONE << exp
}

fn pow_u256(base: U256, exp: usize) -> Result<U256, TwoCryptoSwapError> {
    let mut out = U256::ONE;
    for _ in 0..exp {
        out = checked_mul(out, base)?;
    }
    Ok(out)
}

#[inline]
fn sort_descending_2(arr: &mut [U256; 2]) {
    if arr[0] < arr[1] {
        arr.swap(0, 1);
    }
}

#[inline]
fn min_u256(a: U256, b: U256) -> U256 {
    if a <= b {
        a
    } else {
        b
    }
}

#[inline]
fn max_u256(a: U256, b: U256) -> U256 {
    if a >= b {
        a
    } else {
        b
    }
}

#[inline]
fn abs_diff(a: U256, b: U256) -> U256 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

#[inline]
fn precision() -> U256 {
    u256(PRECISION)
}

#[inline]
fn u256<T: Into<u128>>(v: T) -> U256 {
    let v: u128 = v.into();
    U256::from_limbs([v as u64, (v >> 64) as u64, 0, 0])
}

fn pow10(exp: usize) -> U256 {
    let mut out = U256::ONE;
    for _ in 0..exp {
        out *= u256(10u64);
    }
    out
}

#[inline]
fn checked_mul(a: U256, b: U256) -> Result<U256, TwoCryptoSwapError> {
    a.checked_mul(b).ok_or(TwoCryptoSwapError::Overflow)
}

#[inline]
fn checked_div(a: U256, b: U256) -> Result<U256, TwoCryptoSwapError> {
    a.checked_div(b)
        .ok_or(TwoCryptoSwapError::UnsafeValues("division by zero"))
}

#[inline]
fn checked_add(a: U256, b: U256) -> Result<U256, TwoCryptoSwapError> {
    a.checked_add(b).ok_or(TwoCryptoSwapError::Overflow)
}

#[inline]
fn checked_sub(a: U256, b: U256) -> Result<U256, TwoCryptoSwapError> {
    a.checked_sub(b)
        .ok_or(TwoCryptoSwapError::UnsafeValues("underflow"))
}

#[inline]
fn i256(v: i128) -> I256 {
    I256::try_from(v).expect("small i128 constants should fit into I256")
}

#[inline]
fn i256_from_u256(v: U256) -> Result<I256, TwoCryptoSwapError> {
    I256::try_from(v).map_err(|_| TwoCryptoSwapError::Overflow)
}

#[inline]
fn u256_from_non_negative_i256(v: I256) -> Result<U256, TwoCryptoSwapError> {
    U256::try_from(v).map_err(|_| TwoCryptoSwapError::Overflow)
}

#[inline]
fn iadd(a: I256, b: I256) -> Result<I256, TwoCryptoSwapError> {
    a.checked_add(b).ok_or(TwoCryptoSwapError::Overflow)
}

#[inline]
fn isub(a: I256, b: I256) -> Result<I256, TwoCryptoSwapError> {
    a.checked_sub(b).ok_or(TwoCryptoSwapError::Overflow)
}

#[inline]
fn imul(a: I256, b: I256) -> Result<I256, TwoCryptoSwapError> {
    a.checked_mul(b).ok_or(TwoCryptoSwapError::Overflow)
}

#[inline]
fn idiv(a: I256, b: I256) -> Result<I256, TwoCryptoSwapError> {
    a.checked_div(b)
        .ok_or(TwoCryptoSwapError::UnsafeValues("signed division by zero"))
}

#[inline]
fn ineg(a: I256) -> Result<I256, TwoCryptoSwapError> {
    a.checked_neg().ok_or(TwoCryptoSwapError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANN: u64 = 200_000;
    const GAMMA: u64 = 100_000_000_000_000;
    const MID_FEE: u64 = 5_000_000;
    const OUT_FEE: u64 = 45_000_000;
    const FEE_GAMMA: u64 = 3_000_000_000_000_000;

    fn balanced_state() -> TwoCryptoSwapState {
        TwoCryptoSwapState {
            balances: [
                u256(5_000_000_000_000_000_000_000u128),
                u256(18_000_000_000u64),
            ],
            precisions: [U256::ONE, u256(10_000_000_000u64)],
            price_scale: u256(28_000_000_000_000_000_000u128),
            ann: u256(ANN),
            gamma: u256(GAMMA),
            mid_fee: u256(MID_FEE),
            out_fee: u256(OUT_FEE),
            fee_gamma: u256(FEE_GAMMA),
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        }
    }

    fn imbalanced_xp() -> [U256; 2] {
        [
            u256(7_000_000_000_000_000_000_000u128),
            u256(4_760_000_000_000_000_000_000u128),
        ]
    }

    #[test]
    fn balanced_pool_reference_vectors_match_curve_source_port() {
        let mut state = balanced_state();
        let xp = state.xp().expect("xp should scale");
        let d = state.recompute_d().expect("balanced pool should converge");
        assert_eq!(
            xp,
            [
                u256(5_000_000_000_000_000_000_000u128),
                u256(5_040_000_000_000_000_000_000u128)
            ],
        );
        assert_eq!(d, u256(10_039_990_829_384_809_225_748u128));
        assert_eq!(state.dynamic_fee(&xp), u256(5_210_522u64));
    }

    #[test]
    fn imbalance_pushes_dynamic_fee_toward_out_fee() {
        let state = balanced_state();
        let balanced_fee = state.dynamic_fee(&state.xp().unwrap());
        let imbalanced_fee = state.dynamic_fee(&imbalanced_xp());
        assert_eq!(balanced_fee, u256(5_210_522u64));
        assert_eq!(imbalanced_fee, u256(41_945_101u64));
        assert!(imbalanced_fee > balanced_fee && imbalanced_fee < state.out_fee);
    }

    #[test]
    fn get_y_exact_and_newton_y_exact_match_reference_vectors() {
        let mut state = balanced_state();
        let d = state.recompute_d().unwrap();
        let mut xp = state.xp().unwrap();
        xp[0] = u256(5_010_000_000_000_000_000_000u128);
        let y_newton = newton_y_exact(state.ann, state.gamma, &xp, d, 1).unwrap();
        let (y_exact, k0_prev) = get_y_exact(state.ann, state.gamma, &xp, d, 1).unwrap();
        assert_eq!(y_newton, u256(5_029_992_758_147_817_617_318u128));
        assert_eq!(y_exact, u256(5_029_992_758_166_345_424_066u128));
        assert_eq!(k0_prev, u256(999_996_418_894_769_995u64));
    }

    #[test]
    fn get_amount_out_exact_matches_reference_vectors_across_directions() {
        let mut state = balanced_state();
        state.recompute_d().unwrap();
        let swap_0_to_1 = state
            .get_amount_out_exact(0, 1, u256(10_000_000_000_000_000u64))
            .unwrap();
        let swap_1_to_0 = state
            .get_amount_out_exact(1, 0, u256(100_000_000u64))
            .unwrap();
        assert_eq!(
            swap_0_to_1,
            (
                u256(35_734u64),
                u256(18u64),
                u256(999_985_967_770_634_187u64)
            ),
        );
        assert_eq!(
            swap_1_to_0,
            (
                u256(27_888_594_035_301_968_550u128),
                u256(17_247_772_017_602_549u128),
                u256(999_929_335_523_824_789u64),
            ),
        );
    }

    #[test]
    fn apply_exchange_moves_balances_and_recomputes_d() {
        let mut state = balanced_state();
        state.recompute_d().unwrap();
        let d_before = state.d;
        let b0 = state.balances[0];
        let b1 = state.balances[1];
        let dx = u256(10_000_000_000_000_000u64);
        // The quote vector above pins get_amount_out_exact(0,1,dx).0 = 35_734.
        let (dy, new) = state.apply_exchange(0, 1, dx).expect("apply_exchange");
        assert_eq!(dy, u256(35_734u64), "dy equals the quote");
        assert_eq!(new.balances[0], b0 + dx, "coin0 (in) rises by dx");
        assert_eq!(
            new.balances[1],
            b1 - dy,
            "coin1 (out) falls by after-fee dy"
        );
        assert_ne!(new.d, d_before, "D re-solved from the new balances");
        assert!(!new.d.is_zero());
        // Consistency check: the predicted post-state must re-quote without a stale D (the dx here
        // is a negligible fraction of reserves, so the price move is below integer resolution and
        // the kernel's `-1` safety rounding dominates — monotonicity is asserted in the 3-coin twin
        // where the relative swap is large enough to register).
        let (dy2, _, _) = new
            .get_amount_out_exact(0, 1, dx)
            .expect("re-quote on the predicted post-state");
        assert!(
            dy2 > U256::ZERO,
            "post-state quotes consistently (no stale D)"
        );
    }

    #[test]
    fn newton_y_rejects_invalid_d_when_called_directly() {
        let state = balanced_state();
        let err = newton_y_exact(state.ann, state.gamma, &state.xp().unwrap(), u256(42u64), 1)
            .unwrap_err();
        assert_eq!(err, TwoCryptoSwapError::InvalidParams("D out of range"));
    }

    #[test]
    fn get_amount_out_exact_is_monotonic_and_stays_within_reserves() {
        let mut state = balanced_state();
        state.recompute_d().unwrap();
        let directions = [
            (
                0usize,
                1usize,
                [
                    u256(1_000_000_000_000_000u64),
                    u256(10_000_000_000_000_000u64),
                    u256(100_000_000_000_000_000u64),
                ],
            ),
            (
                1usize,
                0usize,
                [
                    u256(1_000_000u64),
                    u256(10_000_000u64),
                    u256(100_000_000u64),
                ],
            ),
        ];
        for (i, j, amounts_in) in directions {
            let mut prev = U256::ZERO;
            for amount_in in amounts_in {
                let (out, fee, _) = state.get_amount_out_exact(i, j, amount_in).unwrap();
                assert!(out >= prev, "monotonic {i}->{j}");
                assert!(out > U256::ZERO);
                assert!(out + fee < state.balances[j], "within reserves {i}->{j}");
                prev = out;
            }
        }
    }

    #[test]
    fn get_amount_out_exact_rejects_invalid_inputs() {
        let mut state = balanced_state();
        state.recompute_d().unwrap();
        assert_eq!(
            state.get_amount_out_exact(0, 1, U256::ZERO),
            Err(TwoCryptoSwapError::InvalidParams("dx is zero"))
        );
        assert_eq!(
            state.get_amount_out_exact(0, 0, u256(1u64)),
            Err(TwoCryptoSwapError::IndexOutOfRange)
        );
        assert_eq!(
            state.get_amount_out_exact(2, 1, u256(1u64)),
            Err(TwoCryptoSwapError::IndexOutOfRange)
        );
        let mut invalid_ann = state.clone();
        invalid_ann.ann = U256::ZERO;
        assert_eq!(
            invalid_ann.get_amount_out_exact(0, 1, u256(1u64)),
            Err(TwoCryptoSwapError::InvalidParams("ANN out of range"))
        );
    }

    #[test]
    fn validate_a_gamma_max_a_matches_official_twocrypto_ng() {
        // A3: official twocrypto-ng MAX_A = N_COINS²·A_MULTIPLIER·1000 = 4·10_000·1000 = 40_000_000.
        // The old (fail-OPEN) value 4_000_000_000 admitted ANN ~100× above the on-chain ceiling,
        // so the kernel would happily Newton-solve states the real pool would revert on.
        let good_gamma = u256(GAMMA); // in [MIN_GAMMA, MAX_GAMMA]

        // Constant is exactly the official ceiling.
        assert_eq!(
            MAX_A, 40_000_000,
            "MAX_A must equal N_COINS²·A_MULTIPLIER·1000"
        );

        // Just above the ceiling → rejected by the `:538` guard.
        assert_eq!(
            validate_a_gamma(u256(40_000_001u64), good_gamma),
            Err(TwoCryptoSwapError::InvalidParams("ANN out of range")),
            "ANN one above MAX_A must be rejected",
        );
        // Exactly the ceiling → accepted.
        assert_eq!(
            validate_a_gamma(u256(MAX_A), good_gamma),
            Ok(()),
            "ANN == MAX_A must be accepted",
        );
        // The floor (MIN_A) → accepted.
        assert_eq!(
            validate_a_gamma(u256(MIN_A), good_gamma),
            Ok(()),
            "ANN == MIN_A must be accepted",
        );
    }
}

/// K2: randomized property tests for the Curve TwoCrypto-NG (2-coin) Newton kernel.
/// Perturbs a known-good balanced state by the swap amount `dx` (constructing a fully arbitrary
/// converged crypto state is ill-posed): output is monotonic in dx and the kernel never panics.
#[cfg(test)]
mod twocrypto_property {
    use super::*;
    use proptest::prelude::*;

    fn balanced() -> TwoCryptoSwapState {
        let mut s = TwoCryptoSwapState {
            balances: [
                U256::from(5_000_000_000_000_000_000_000u128),
                U256::from(18_000_000_000u64),
            ],
            precisions: [U256::ONE, U256::from(10_000_000_000u64)],
            price_scale: U256::from(28_000_000_000_000_000_000u128),
            ann: U256::from(200_000u64),
            gamma: U256::from(100_000_000_000_000u64),
            mid_fee: U256::from(5_000_000u64),
            out_fee: U256::from(45_000_000u64),
            fee_gamma: U256::from(3_000_000_000_000_000u64),
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        };
        s.recompute_d().expect("balanced pool converges");
        s
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(600))]

        #[test]
        fn monotonic_in_dx(
            d1 in 1_000_000_000_000_000_000u128..=500_000_000_000_000_000_000u128,
            bump in 1u128..=500_000_000_000_000_000_000u128,
        ) {
            let s = balanced();
            let o1 = s.get_amount_out_exact(0, 1, U256::from(d1));
            let o2 = s.get_amount_out_exact(0, 1, U256::from(d1).saturating_add(U256::from(bump)));
            if let (Ok(a), Ok(b)) = (o1, o2) {
                prop_assert!(b.0 >= a.0, "more dx ⇒ not less dy ({} → {})", a.0, b.0);
            }
        }
    }
}
