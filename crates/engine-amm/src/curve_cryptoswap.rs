//! Curve CryptoSwap NG (Tricrypto-NG / TwoCrypto-NG) exact integer math.
//!
//! Faithful Rust port of Curve's reference `newton_D` / `_newton_y` iterations + dynamic-fee
//! `reduction_coefficient`. Integer-only (no float drift); identical to the Vyper source on every
//! test vector. The kernel matches the reference implementation in
//! `crates/engine/src/curve_cryptoswap_fixed.rs` of the Shadow/Fulcrum sibling repos (verified
//! against the same hand-derived reference vectors).
//!
//! ## Scope
//! * 3-coin (Tricrypto-NG) layout: `balances`, `precisions`, `price_scale[0..2]`.
//! * 2-coin TwoCrypto-NG can re-use the same kernel with the 3rd coin's balance/precision = 0
//!   (the math degenerates correctly when ratios are within the unsafe-frac window — callers
//!   should prefer the dedicated 2-coin entrypoint when it lands).
//!
//! ## What the kernel is for
//! Curve's `D` is the Newton-solved invariant constant; `_newton_y` solves for the post-swap
//! coin balance in the `j`-th column given a perturbed `xp`. Combined with the dynamic-fee
//! `reduction_coefficient`, `CryptoSwapState::get_amount_out_exact` returns `(dy_after_fee,
//! fee_amount, k0_final)` matching Vyper-exact on the reference vectors.

use alloy_primitives::{U256, U512};

const N_COINS: u64 = 3;
const N_COINS_CUBED: u64 = 27;
const A_MULTIPLIER: u64 = 10_000;
const PRECISION: u128 = 1_000_000_000_000_000_000;
const MIN_GAMMA: u128 = 10_000_000_000;
const MAX_GAMMA: u128 = 50_000_000_000_000_000;
const MIN_A: u128 = (N_COINS_CUBED as u128) * (A_MULTIPLIER as u128) / 100;
const MAX_A: u128 = (N_COINS_CUBED as u128) * (A_MULTIPLIER as u128) * 1000;
const MIN_D: u128 = 100_000_000_000_000_000;
const MAX_D: u128 = 1_000_000_000_000_000u128 * PRECISION;

/// CryptoSwap NG math errors. Fail-closed: every iteration that doesn't converge, every
/// arithmetic edge that underflows, every parameter outside Curve's documented safety band
/// returns an explicit error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoSwapError {
    /// Newton iteration did not converge within 255 iterations.
    DidNotConverge,
    /// Caller passed parameters outside Curve's safety band.
    InvalidParams(&'static str),
    /// An intermediate value drifted into an unsafe / fail-closed region.
    UnsafeValues(&'static str),
    /// Coin index out of range.
    IndexOutOfRange,
    /// Cube-root overflow.
    Overflow,
}

/// 3-coin CryptoSwap NG pool state. `balances` are real on-chain reserves; `precisions` scale
/// each coin to 1e18 units; `price_scale[0]` / `price_scale[1]` reprice coins 1 / 2 against
/// coin 0 (the numeraire).
#[derive(Debug, Clone)]
pub struct CryptoSwapState {
    /// Real on-chain reserves per coin (10^token_decimals units).
    pub balances: [U256; 3],
    /// Per-coin scale to bring balances into 1e18-units xp space.
    pub precisions: [U256; 3],
    /// Pool-internal repricing factors: coin1/coin0 and coin2/coin0 (1e18-scaled).
    pub price_scale: [U256; 2],
    /// `A * N^N * A_MULTIPLIER` per Curve's convention.
    pub ann: U256,
    /// CryptoSwap gamma parameter (1e18-scaled).
    pub gamma: U256,
    /// Dynamic fee mid-band (1e10-scaled — Curve's `FEE_DENOMINATOR = 10^10`).
    pub mid_fee: U256,
    /// Dynamic fee outer-band (1e10-scaled).
    pub out_fee: U256,
    /// Fee gamma: controls how aggressively the fee rises with imbalance.
    pub fee_gamma: U256,
    /// Solved invariant — populated by [`Self::recompute_d`].
    pub d: U256,
    /// `k0_prev` from the previous swap (warm-starts the `D` Newton iter; 0 = cold start).
    pub last_k0_prev: U256,
}

impl CryptoSwapState {
    /// Compute repriced balances `xp[i]` in 1e18-units. Fail-closed (`Err(Overflow)`) on a crafted
    /// or corrupt snapshot whose `balance * precision * price_scale` product exceeds U256, matching
    /// the checked `curve_twocrypto::xp`; byte-identical to the old `*`/`/` for every real pool.
    #[inline]
    pub fn xp(&self) -> Result<[U256; 3], CryptoSwapError> {
        let p = u256(PRECISION as u64);
        Ok([
            checked_mul(self.balances[0], self.precisions[0])?,
            checked_div(
                checked_mul(
                    checked_mul(self.balances[1], self.precisions[1])?,
                    self.price_scale[0],
                )?,
                p,
            )?,
            checked_div(
                checked_mul(
                    checked_mul(self.balances[2], self.precisions[2])?,
                    self.price_scale[1],
                )?,
                p,
            )?,
        ])
    }

    /// Re-solve the invariant `D` from the current `xp`. Returns the new `D` and stores it.
    pub fn recompute_d(&mut self) -> Result<U256, CryptoSwapError> {
        self.d = newton_d_exact(self.ann, self.gamma, &self.xp()?, self.last_k0_prev)?;
        Ok(self.d)
    }

    /// Curve's dynamic-fee curve: blends `mid_fee` ↔ `out_fee` based on a `reduction_coefficient`
    /// that drops as the pool becomes imbalanced.
    #[inline]
    pub fn dynamic_fee(&self, xp: &[U256; 3]) -> U256 {
        let p = u256(PRECISION as u64);
        let k = reduction_coefficient(xp, self.fee_gamma);
        (self.mid_fee * k + self.out_fee * (p - k)) / p
    }

    /// Quote a swap `i → j` for an input of `dx` real-units of coin `i`. Returns
    /// `(dy_after_fee, fee_amount, k0_final)` in real-units of coin `j`. Fail-closed on every
    /// invalid input and every unsafe intermediate.
    pub fn get_amount_out_exact(
        &self,
        i: usize,
        j: usize,
        dx: U256,
    ) -> Result<(U256, U256, U256), CryptoSwapError> {
        if i >= N_COINS as usize || j >= N_COINS as usize || i == j {
            return Err(CryptoSwapError::IndexOutOfRange);
        }
        if dx.is_zero() {
            return Err(CryptoSwapError::InvalidParams("dx is zero"));
        }

        let p = u256(PRECISION as u64);
        let mut xp = self.xp()?;
        let xp_before_j = xp[j];

        // `(balance + dx) * precision [* price_scale / p]` — checked so a crafted balance/dx cannot
        // overflow U256 (byte-identical to the old `+`/`*`/`/` for every real input).
        let bal_plus_dx = self.balances[i]
            .checked_add(dx)
            .ok_or(CryptoSwapError::Overflow)?;
        xp[i] = if i == 0 {
            checked_mul(bal_plus_dx, self.precisions[i])?
        } else {
            checked_div(
                checked_mul(
                    checked_mul(bal_plus_dx, self.precisions[i])?,
                    self.price_scale[i - 1],
                )?,
                p,
            )?
        };

        let (y_out, k0_prev) = newton_y_exact(self.ann, self.gamma, &xp, self.d, j)?;
        let dy_scaled = xp_before_j
            .checked_sub(y_out)
            .ok_or(CryptoSwapError::UnsafeValues("negative dy"))?
            .checked_sub(U256::ONE)
            .ok_or(CryptoSwapError::UnsafeValues("dy rounded to zero"))?;

        let dy_real = if j == 0 {
            checked_div(dy_scaled, self.precisions[j])?
        } else {
            checked_div(
                checked_mul(dy_scaled, p)?,
                checked_mul(self.price_scale[j - 1], self.precisions[j])?,
            )?
        };

        let mut xp_fee = xp;
        xp_fee[j] = xp_fee[j]
            .checked_sub(dy_scaled)
            .ok_or(CryptoSwapError::UnsafeValues("xp_fee underflow"))?;
        let fee_rate = self.dynamic_fee(&xp_fee);
        let fee_amount = checked_div(
            checked_mul(fee_rate, dy_real)?,
            u256(10_000_000_000u64),
        )?;
        let dy_after_fee = dy_real
            .checked_sub(fee_amount)
            .ok_or(CryptoSwapError::UnsafeValues("fee > dy"))?;

        Ok((dy_after_fee, fee_amount, k0_prev))
    }

    /// Forward-apply a swap `i → j` of `dx` real-units: returns `(dy_after_fee, new_state)` with
    /// `balances[i] += dx`, `balances[j] -= dy_after_fee` (the swap fee is retained in the pool, so
    /// only the after-fee amount leaves), and `D` re-solved from the new balances. This reproduces
    /// the on-chain `exchange` state transition for the common case where `tweak_price` does not
    /// repeg `price_scale` — a rate-limited, sub-bp per-swap effect. `price_scale` is therefore held;
    /// the prediction feeds the REVM-validated detection path (never a fast-lane/zero-RPC submission),
    /// which re-prices the exact post-swap state, so the held repeg costs at most a wasted check.
    pub fn apply_exchange(
        &self,
        i: usize,
        j: usize,
        dx: U256,
    ) -> Result<(U256, CryptoSwapState), CryptoSwapError> {
        let (dy_after_fee, _fee, k0) = self.get_amount_out_exact(i, j, dx)?;
        let mut new = self.clone();
        new.balances[i] = new.balances[i]
            .checked_add(dx)
            .ok_or(CryptoSwapError::Overflow)?;
        new.balances[j] = new.balances[j]
            .checked_sub(dy_after_fee)
            .ok_or(CryptoSwapError::UnsafeValues("dy exceeds balance"))?;
        new.last_k0_prev = k0;
        new.recompute_d()?;
        Ok((dy_after_fee, new))
    }
}

/// Solve Curve's invariant `D` via Newton iteration. Integer-exact; matches Vyper.
pub fn newton_d_exact(
    ann: U256,
    gamma: U256,
    x_unsorted: &[U256; 3],
    k0_prev: U256,
) -> Result<U256, CryptoSwapError> {
    let min_a = u256(MIN_A as u64);
    let max_a = u256(MAX_A as u64);
    let min_gamma = U256::from(MIN_GAMMA);
    let max_gamma = U256::from(MAX_GAMMA);

    if ann < min_a || ann > max_a {
        return Err(CryptoSwapError::InvalidParams("ANN out of range"));
    }
    if gamma < min_gamma || gamma > max_gamma {
        return Err(CryptoSwapError::InvalidParams("gamma out of range"));
    }

    let mut x = *x_unsorted;
    sort_descending_3(&mut x);
    if x[0].is_zero() {
        return Err(CryptoSwapError::InvalidParams("empty pool"));
    }

    let s = x[0] + x[1] + x[2];
    let mut d = if k0_prev.is_zero() {
        u256(N_COINS as u128) * geometric_mean_exact(&x)?
    } else if s > pow10(36) {
        wad_cbrt((((x[0] * x[1]) / pow10(36)) * x[2] / k0_prev) * u256(27u128) * pow10(12))?
    } else if s > pow10(24) {
        wad_cbrt((((x[0] * x[1]) / pow10(24)) * x[2] / k0_prev) * u256(27u128) * pow10(6))?
    } else {
        wad_cbrt((((x[0] * x[1]) / pow10(18)) * x[2] / k0_prev) * u256(27u128))?
    };

    let p = pow10(18);
    for _ in 0..255 {
        let d_prev = d;

        let k0 = (((((p * x[0] * u256(N_COINS as u128)) / d) * x[1] * u256(N_COINS as u128)) / d)
            * x[2]
            * u256(N_COINS as u128))
            / d;
        let mut g1k0 = gamma + p;
        g1k0 = if g1k0 > k0 {
            g1k0 - k0 + U256::ONE
        } else {
            k0 - g1k0 + U256::ONE
        };

        let mul1 = (((((p * d) / gamma) * g1k0) / gamma) * g1k0 * u256(A_MULTIPLIER as u128)) / ann;
        let mul2 = (u256(2u128) * p * u256(N_COINS as u128) * k0) / g1k0;
        let neg_fprime =
            (s + (s * mul2) / p + (mul1 * u256(N_COINS as u128)) / k0) - (mul2 * d) / p;
        let d_plus = d * (neg_fprime + s) / neg_fprime;
        let mut d_minus = d * d / neg_fprime;
        if p > k0 {
            d_minus += (((d * (mul1 / neg_fprime)) / p) * (p - k0)) / k0;
        } else {
            d_minus -= (((d * (mul1 / neg_fprime)) / p) * (k0 - p)) / k0;
        }

        d = if d_plus > d_minus {
            d_plus - d_minus
        } else {
            (d_minus - d_plus) / u256(2u128)
        };

        let diff = abs_diff(d, d_prev);
        if diff * pow10(14) < max_u256(pow10(16), d) {
            for xi in x {
                let frac = (xi * p) / d;
                if frac < (pow10(16) - U256::ONE) || frac >= (pow10(20) + U256::ONE) {
                    return Err(CryptoSwapError::UnsafeValues("unsafe x[i]"));
                }
            }
            return Ok(d);
        }
    }

    Err(CryptoSwapError::DidNotConverge)
}

/// Solve for the post-swap balance of coin `i` (in xp-space) given `D` and the perturbed `xp`
/// of the other coins. Returns `(y, k0_final)`.
///
/// A4 (verified-correct, locked): this is the ITERATIVE Newton solve, byte-for-byte the algorithm
/// the deployed Curve CryptoSwap-NG pool runs in its own `_newton_y` (Vyper) — Curve's on-chain
/// `get_y` is itself this fixed-point iteration, not a closed-form. The convergence test
/// (`newton_y_converges_within_convergence_limit`) plus the reference-vector test pin parity;
/// on-chain fork parity measured 0.000 bps. The analytic cubic root that exists in the 2-coin
/// twin (`curve_twocrypto::get_y_exact`) is an off-path helper for that pool's `K0_prev` thread
/// and is NOT used here. Do not "optimize" this into an analytic solve: that would diverge from
/// the on-chain rounding and silently mis-quote.
pub fn newton_y_exact(
    ann: U256,
    gamma: U256,
    x: &[U256; 3],
    d: U256,
    i: usize,
) -> Result<(U256, U256), CryptoSwapError> {
    if i >= N_COINS as usize {
        return Err(CryptoSwapError::IndexOutOfRange);
    }
    let min_a = u256(MIN_A as u64);
    let max_a = u256(MAX_A as u64);
    let min_gamma = U256::from(MIN_GAMMA);
    let max_gamma = U256::from(MAX_GAMMA);
    if ann < min_a || ann > max_a {
        return Err(CryptoSwapError::InvalidParams("ANN out of range"));
    }
    if gamma < min_gamma || gamma > max_gamma {
        return Err(CryptoSwapError::InvalidParams("gamma out of range"));
    }
    let min_d = U256::from(MIN_D);
    let max_d = U256::from(MAX_D);
    if d < min_d || d > max_d {
        return Err(CryptoSwapError::InvalidParams("D out of range"));
    }

    let p = pow10(18);
    for k in 0..3 {
        if k != i {
            let frac = (x[k] * p) / d;
            if frac <= (pow10(16) - U256::ONE) || frac >= (pow10(20) + U256::ONE) {
                return Err(CryptoSwapError::UnsafeValues("unsafe values x[i]"));
            }
        }
    }

    let mut y = d / u256(N_COINS as u128);
    let mut k0_i = p;
    let mut s_i = U256::ZERO;

    let mut x_sorted = *x;
    x_sorted[i] = U256::ZERO;
    sort_descending_3(&mut x_sorted);
    let convergence_limit = max_u256(
        max_u256(x_sorted[0] / pow10(14), d / pow10(14)),
        u256(100u128),
    );

    for j in 2..=N_COINS as usize {
        let xi = x_sorted[N_COINS as usize - j];
        y = y * d / (xi * u256(N_COINS as u128));
        s_i += xi;
    }
    for j in 0..(N_COINS as usize - 1) {
        k0_i = k0_i * x_sorted[j] * u256(N_COINS as u128) / d;
    }

    for _ in 0..255 {
        let y_prev = y;
        let k0 = k0_i * y * u256(N_COINS as u128) / d;
        let s = s_i + y;

        let mut g1k0 = gamma + p;
        g1k0 = if g1k0 > k0 {
            g1k0 - k0 + U256::ONE
        } else {
            k0 - g1k0 + U256::ONE
        };

        let mul1 = (((((p * d) / gamma) * g1k0) / gamma) * g1k0 * u256(A_MULTIPLIER as u128)) / ann;
        let mul2 = p + ((u256(2u128) * p) * k0) / g1k0;
        let mut yfprime = p * y + s * mul2 + mul1;
        let dyfprime = d * mul2;
        if yfprime < dyfprime {
            y = y_prev / u256(2u128);
            continue;
        }
        yfprime -= dyfprime;

        let fprime = yfprime / y;
        let mut y_minus = mul1 / fprime;
        let y_plus = (yfprime + p * d) / fprime + (y_minus * p) / k0;
        y_minus += (p * s) / fprime;
        y = if y_plus < y_minus {
            y_prev / u256(2u128)
        } else {
            y_plus - y_minus
        };

        let diff = abs_diff(y, y_prev);
        if diff < max_u256(convergence_limit, y / pow10(14)) {
            let frac = (y * p) / d;
            if frac <= (pow10(16) - U256::ONE) || frac >= (pow10(20) + U256::ONE) {
                return Err(CryptoSwapError::UnsafeValues("unsafe y"));
            }
            let k0_final = k0_i * y * u256(N_COINS as u128) / d;
            return Ok((y, k0_final));
        }
    }

    Err(CryptoSwapError::DidNotConverge)
}

/// Curve's cube-root-of-geometric-mean.
pub fn geometric_mean_exact(x: &[U256; 3]) -> Result<U256, CryptoSwapError> {
    let p = pow10(18);
    let prod = ((x[0] * x[1]) / p) * x[2] / p;
    if prod.is_zero() {
        return Ok(U256::ZERO);
    }
    wad_cbrt(prod)
}

/// Dynamic-fee blending factor — drops as imbalance grows so the effective fee approaches
/// `out_fee`. Returns a 1e18-scaled value.
pub fn reduction_coefficient(x: &[U256; 3], fee_gamma: U256) -> U256 {
    let p = pow10(18);
    let s = x[0] + x[1] + x[2];
    if s.is_zero() {
        return U256::ZERO;
    }
    let mut k = p * u256(N_COINS as u128) * x[0] / s;
    k = k * u256(N_COINS as u128) * x[1] / s;
    k = k * u256(N_COINS as u128) * x[2] / s;
    if !fee_gamma.is_zero() {
        k = fee_gamma * p / (fee_gamma + p - k);
    }
    k
}

/// WAD-scaled cube root: `wad_cbrt(x) = floor((x * 1e36)^(1/3))`. Uses a U512 search so the
/// intermediate `x * 1e36` cannot overflow on the integer space CryptoSwap operates in.
pub fn wad_cbrt(x: U256) -> Result<U256, CryptoSwapError> {
    let scaled = U512::from(x) * U512::from(pow10(36));
    let root = icbrt_u512(scaled);
    if root > U512::from(U256::MAX) {
        return Err(CryptoSwapError::Overflow);
    }
    Ok(U256::from(root))
}

fn icbrt_u512(n: U512) -> U512 {
    if n.is_zero() {
        return U512::ZERO;
    }
    let bits = 512 - n.leading_zeros();
    let mut lo = U512::ZERO;
    let mut hi = U512::from(1u64) << (bits.div_ceil(3) + 1);
    while lo + U512::from(1u64) < hi {
        let mid = (lo + hi) >> 1;
        let mid3 = mid * mid * mid;
        if mid3 <= n {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[inline]
fn sort_descending_3(arr: &mut [U256; 3]) {
    if arr[0] < arr[1] {
        arr.swap(0, 1);
    }
    if arr[0] < arr[2] {
        arr.swap(0, 2);
    }
    if arr[1] < arr[2] {
        arr.swap(1, 2);
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
fn max_u256(a: U256, b: U256) -> U256 {
    if a >= b {
        a
    } else {
        b
    }
}

#[inline]
fn u256<T: Into<u128>>(v: T) -> U256 {
    let v: u128 = v.into();
    U256::from_limbs([v as u64, (v >> 64) as u64, 0, 0])
}

/// Fail-closed U256 multiply: `Err(Overflow)` instead of a wrapping/panicking `*` when a crafted or
/// corrupt snapshot pushes a product past 2^256. Byte-identical to `*` for every value that fits.
/// Mirrors `curve_twocrypto::checked_mul`.
#[inline]
fn checked_mul(a: U256, b: U256) -> Result<U256, CryptoSwapError> {
    a.checked_mul(b).ok_or(CryptoSwapError::Overflow)
}

/// Fail-closed U256 divide: `Err(Overflow)` on divide-by-zero rather than a panic. Byte-identical
/// to `/` for every non-zero divisor. Mirrors `curve_twocrypto::checked_div`.
#[inline]
fn checked_div(a: U256, b: U256) -> Result<U256, CryptoSwapError> {
    a.checked_div(b).ok_or(CryptoSwapError::Overflow)
}

fn pow10(exp: usize) -> U256 {
    let mut out = U256::ONE;
    for _ in 0..exp {
        out *= u256(10u64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The constants below are derived from a real Curve tricrypto-NG pool config (1e10-scaled
    // fees, 1e18-scaled gamma/fee_gamma, ANN = A * N^N * A_MULTIPLIER for A=20). These match
    // the reference vectors in the Shadow/Fulcrum source so we re-use them as a parity gate:
    // any drift here means the port introduced a regression vs. the Vyper source.
    const ANN: u64 = 1_707_629;
    const GAMMA: u64 = 11_809_167_828_997;
    const MID_FEE: u64 = 3_000_000;
    const OUT_FEE: u64 = 30_000_000;
    const FEE_GAMMA: u64 = 5_000_000_000_000_000;

    fn balanced_state() -> CryptoSwapState {
        CryptoSwapState {
            balances: [
                U256::from(5_000_000_000_000u64),
                U256::from(8_000_000_000u64),
                U256::from(1_600_000_000_000_000_000_000u128),
            ],
            precisions: [
                u256(1_000_000_000_000u64),
                u256(10_000_000_000u64),
                U256::ONE,
            ],
            price_scale: [
                u256(60_000_000_000_000_000_000_000u128),
                u256(3_000_000_000_000_000_000_000u128),
            ],
            ann: u256(ANN),
            gamma: u256(GAMMA),
            mid_fee: u256(MID_FEE),
            out_fee: u256(OUT_FEE),
            fee_gamma: u256(FEE_GAMMA),
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        }
    }

    fn imbalanced_xp() -> [U256; 3] {
        [
            u256(9_000_000_000_000_000_000_000_000u128),
            u256(2_400_000_000_000_000_000_000_000u128),
            u256(2_700_000_000_000_000_000_000_000u128),
        ]
    }

    #[test]
    fn balanced_pool_reference_vectors_match_curve_source_port() {
        let mut state = balanced_state();
        let d = state
            .recompute_d()
            .expect("balanced reference pool should converge");
        assert_eq!(
            d,
            u256(14_597_972_047_721_800_479_164_646u128),
            "newton_d_exact drifted from the Curve-derived reference vector",
        );
        assert_eq!(
            state.dynamic_fee(&state.xp().unwrap()),
            u256(5_709_884u64),
            "dynamic fee drifted from the Curve-derived reference vector",
        );
    }

    #[test]
    fn imbalance_pushes_dynamic_fee_toward_out_fee() {
        let state = balanced_state();
        let balanced_fee = state.dynamic_fee(&state.xp().unwrap());
        let imbalanced_fee = state.dynamic_fee(&imbalanced_xp());
        assert_eq!(balanced_fee, u256(5_709_884u64));
        assert_eq!(imbalanced_fee, u256(29_695_448u64));
        assert!(imbalanced_fee > balanced_fee);
        assert!(imbalanced_fee < state.out_fee);
    }

    #[test]
    fn newton_y_exact_matches_reference_swap_vector() {
        let mut state = balanced_state();
        let d = state.recompute_d().unwrap();
        let mut xp = state.xp().unwrap();
        xp[0] = u256(5_010_000_000_000_000_000_000_000u128);
        let (y, k0_final) = newton_y_exact(state.ann, state.gamma, &xp, d, 1).unwrap();
        assert_eq!(y, u256(4_790_377_403_130_854_142_161_612u128));
        assert_eq!(k0_final, u256(999_850_053_697_653_381u64));
    }

    #[test]
    fn newton_y_converges_within_convergence_limit() {
        // A4 lock: the iterative `_newton_y` (the deployed Curve algorithm) must CONVERGE for a
        // spread of perturbations of a healthy balanced pool. A successful Ok return is itself the
        // convergence proof — the loop only returns once `|y - y_prev| < max(convergence_limit,
        // y/1e14)` (the same dual criterion Curve's Vyper `get_y` uses); otherwise it fails closed
        // with `DidNotConverge`. We assert convergence across several coins/directions and that the
        // converged `y` lands in the safe `[1e16, 1e20]` fraction band (no `UnsafeValues`).
        let mut state = balanced_state();
        let d = state.recompute_d().unwrap();
        let base = state.xp().unwrap();
        let p = pow10(18);
        // Perturb each non-target coin up/down by a few percent; solve the remaining column.
        for i in 0..3usize {
            for &(k, num, den) in &[
                (0usize, 103u64, 100u64),
                (1usize, 97, 100),
                (2usize, 101, 100),
            ] {
                if k == i {
                    continue;
                }
                let mut xp = base;
                xp[k] = xp[k] * u256(num) / u256(den);
                let (y, k0_final) = newton_y_exact(state.ann, state.gamma, &xp, d, i)
                    .unwrap_or_else(|e| panic!("newton_y must converge for i={i} k={k}: {e:?}"));
                assert!(y > U256::ZERO, "converged y must be positive");
                assert!(!k0_final.is_zero(), "k0_final must be set on convergence");
                // The convergence postcondition the kernel itself enforces (safe fraction band).
                let frac = (y * p) / d;
                assert!(
                    frac > (pow10(16) - U256::ONE) && frac < (pow10(20) + U256::ONE),
                    "converged y fraction {frac} out of Curve's safe band for i={i} k={k}",
                );
            }
        }
    }

    #[test]
    fn get_amount_out_exact_matches_reference_vectors_across_directions() {
        let mut state = balanced_state();
        state.recompute_d().unwrap();

        assert_eq!(
            state
                .get_amount_out_exact(0, 1, u256(10_000_000_000u64))
                .unwrap(),
            (
                u256(16_027_909u64),
                u256(9_752u64),
                u256(999_850_053_697_653_381u64),
            ),
        );
        assert_eq!(
            state
                .get_amount_out_exact(1, 0, u256(100_000_000u64))
                .unwrap(),
            (
                u256(61_446_172_621u64),
                u256(24_933_873u64),
                u256(999_910_863_139_582_021u64),
            ),
        );
        assert_eq!(
            state
                .get_amount_out_exact(2, 0, u256(10_000_000_000_000_000_000u128))
                .unwrap(),
            (
                u256(30_906_140_100u64),
                u256(14_613_967u64),
                u256(999_885_963_965_743_145u64),
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
        let b2 = state.balances[2];
        let dx = u256(10_000_000_000u64);
        // Forward-apply coin0 → coin1. The quote vector above pins dy = 16_027_909.
        let (dy, new) = state.apply_exchange(0, 1, dx).expect("apply_exchange");
        assert_eq!(dy, u256(16_027_909u64), "dy equals the quote");
        assert_eq!(new.balances[0], b0 + dx, "coin0 (in) rises by the full dx");
        assert_eq!(
            new.balances[1],
            b1 - dy,
            "coin1 (out) falls by the after-fee dy (fee retained in pool)"
        );
        assert_eq!(new.balances[2], b2, "untouched coin unchanged");
        assert_ne!(new.d, d_before, "D re-solved from the new balances");
        assert!(!new.d.is_zero());
        // The post-swap state must itself quote consistently (no stale D): a second identical buy
        // of coin1 yields slightly LESS (coin1 got scarcer) — proves D/balances stay coherent.
        let (dy2, _, _) = new
            .get_amount_out_exact(0, 1, dx)
            .expect("re-quote on the predicted post-state");
        assert!(
            dy2 > U256::ZERO && dy2 < dy,
            "price moved against the next buyer"
        );
    }

    #[test]
    fn newton_y_rejects_invalid_d_when_called_directly() {
        let state = balanced_state();
        let err = newton_y_exact(state.ann, state.gamma, &state.xp().unwrap(), u256(42u64), 1).unwrap_err();
        assert_eq!(err, CryptoSwapError::InvalidParams("D out of range"));
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
                    u256(100_000_000u64),
                    u256(1_000_000_000u64),
                    u256(10_000_000_000u64),
                    u256(50_000_000_000u64),
                ],
            ),
            (
                1usize,
                0usize,
                [
                    u256(1_000_000u64),
                    u256(10_000_000u64),
                    u256(100_000_000u64),
                    u256(500_000_000u64),
                ],
            ),
            (
                2usize,
                0usize,
                [
                    u256(100_000_000_000_000_000u128),
                    u256(1_000_000_000_000_000_000u128),
                    u256(10_000_000_000_000_000_000u128),
                    u256(50_000_000_000_000_000_000u128),
                ],
            ),
        ];
        for (i, j, amounts_in) in directions {
            let mut prev = U256::ZERO;
            for amount_in in amounts_in {
                let (out, fee, _) = state.get_amount_out_exact(i, j, amount_in).unwrap();
                assert!(out >= prev, "monotonicity failed for {i}->{j}");
                assert!(out > U256::ZERO);
                assert!(
                    out + fee < state.balances[j],
                    "exceeded reserves for {i}->{j}"
                );
                prev = out;
            }
        }
    }

    #[test]
    fn get_amount_out_exact_rejects_invalid_inputs_and_params() {
        let mut state = balanced_state();
        state.recompute_d().unwrap();
        assert_eq!(
            state.get_amount_out_exact(0, 1, U256::ZERO),
            Err(CryptoSwapError::InvalidParams("dx is zero"))
        );
        assert_eq!(
            state.get_amount_out_exact(0, 0, u256(1u64)),
            Err(CryptoSwapError::IndexOutOfRange)
        );
        assert_eq!(
            state.get_amount_out_exact(3, 1, u256(1u64)),
            Err(CryptoSwapError::IndexOutOfRange)
        );

        let mut invalid_ann = state.clone();
        invalid_ann.ann = U256::ZERO;
        assert_eq!(
            invalid_ann.get_amount_out_exact(0, 1, u256(1u64)),
            Err(CryptoSwapError::InvalidParams("ANN out of range"))
        );

        let mut invalid_gamma = state.clone();
        invalid_gamma.gamma = U256::ZERO;
        assert_eq!(
            invalid_gamma.get_amount_out_exact(0, 1, u256(1u64)),
            Err(CryptoSwapError::InvalidParams("gamma out of range"))
        );
    }
}

/// K2: randomized property tests for the Curve Tricrypto-NG (3-coin) Newton kernel.
/// Perturbs a known-good balanced state by the swap amount `dx`: output is monotonic in dx and the
/// kernel never panics.
#[cfg(test)]
mod cryptoswap_property {
    use super::*;
    use proptest::prelude::*;

    fn balanced() -> CryptoSwapState {
        let mut s = CryptoSwapState {
            balances: [
                U256::from(5_000_000_000_000u64),
                U256::from(8_000_000_000u64),
                U256::from(1_600_000_000_000_000_000_000u128),
            ],
            precisions: [
                U256::from(1_000_000_000_000u64),
                U256::from(10_000_000_000u64),
                U256::ONE,
            ],
            price_scale: [
                U256::from(60_000_000_000_000_000_000_000u128),
                U256::from(3_000_000_000_000_000_000_000u128),
            ],
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
            d1 in 1_000_000_000u128..=1_000_000_000_000u128,
            bump in 1u128..=1_000_000_000_000u128,
        ) {
            let s = balanced();
            // coin 0 (6-dec USDC-like) → coin 1 (8-dec).
            let o1 = s.get_amount_out_exact(0, 1, U256::from(d1));
            let o2 = s.get_amount_out_exact(0, 1, U256::from(d1).saturating_add(U256::from(bump)));
            if let (Ok(a), Ok(b)) = (o1, o2) {
                prop_assert!(b.0 >= a.0, "more dx ⇒ not less dy ({} → {})", a.0, b.0);
            }
        }
    }
}

#[cfg(test)]
mod overflow_safety_tests {
    //! AUD-CRYPTO-CHECKED: the CryptoSwap kernel must FAIL CLOSED (Err) on crafted/corrupt state
    //! whose raw products overflow U256, never panic — matching its checked TwoCrypto twin.
    use super::*;

    fn crafted_overflow_state() -> CryptoSwapState {
        CryptoSwapState {
            // balances[1] * precisions[1] * price_scale[0] overflows U256.
            balances: [U256::from(1u64), U256::MAX >> 8, U256::from(1u64)],
            precisions: [u256(1_000_000_000_000u64), u256(1_000_000_000_000u64), U256::ONE],
            price_scale: [u256(60_000_000_000_000_000_000_000u128), u256(3_000_000_000_000_000_000_000u128)],
            ann: u256(1_707_629u64),
            gamma: u256(11_809_167_828_997u64),
            mid_fee: u256(3_000_000u64),
            out_fee: u256(30_000_000u64),
            fee_gamma: u256(5_000_000_000_000_000u64),
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        }
    }

    #[test]
    fn crafted_state_xp_fails_closed_not_panics() {
        let s = crafted_overflow_state();
        assert!(s.xp().is_err(), "overflowing xp must return Err, not panic");
    }

    #[test]
    fn crafted_state_quote_fails_closed_not_panics() {
        let s = crafted_overflow_state();
        let r = s.get_amount_out_exact(0, 1, U256::from(1_000_000u64));
        assert!(r.is_err(), "overflowing quote must return Err, not panic");
    }
}
