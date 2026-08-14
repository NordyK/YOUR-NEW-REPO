//! DODO V2 Proactive Market Maker (PMM) math kernel.
//!
//! DODO V2 implements a PMM with state variables `(i, K, B, Q, B0, Q0, R)`:
//!   * `i` — oracle price (Q per 1 B, in 1e18 fixed point)
//!   * `K` — curve concavity (0 = constant product, 1e18 = stable curve)
//!   * `B`, `Q` — current base / quote reserves
//!   * `B0`, `Q0` — target (equilibrium) reserves
//!   * `R` — current state: `One` / `AboveOne` (base abundant) / `BelowOne` (quote abundant)
//!
//! Ported from the Fulcrum codebase. Pure math kernel — RPC bindings and
//! discovery live in separate modules. All math operates on
//! `alloy_primitives` `U256` / `U512`.

use alloy_primitives::{Address, U256, U512};

use crate::error::{AmmError, AmmResult};

/// 1e18, the fixed-point precision used by DODO contracts.
pub const D18: u128 = 1_000_000_000_000_000_000;

#[inline]
fn one_d18() -> U256 {
    U256::from(D18)
}

#[inline]
fn one_d36_u512() -> U512 {
    U512::from(one_d18()) * U512::from(one_d18())
}

/// Function selectors for direct pool calls.
pub mod selectors {
    /// `sellBase(address)` — selector `0xbd6015b4`.
    pub const SELL_BASE: [u8; 4] = [0xbd, 0x60, 0x15, 0xb4];
    /// `sellQuote(address)` — selector `0xdd93f59a`.
    pub const SELL_QUOTE: [u8; 4] = [0xdd, 0x93, 0xf5, 0x9a];
}

/// DODO V2 pool class — different PMM variants have slightly different invariants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DodoPoolClass {
    /// DODO Vending Machine (price-curve only, no targets recorded).
    Dvm,
    /// DODO Private Pool (admin-managed PMM with explicit targets).
    Dpp,
    /// DODO Stable Pool (PMM tuned for stables — high K).
    Dsp,
}

/// PMM `R` state — which side of the curve we're on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DodoRState {
    /// Balanced — reserves equal targets.
    One = 0,
    /// Base abundant (B > B0, Q < Q0).
    AboveOne = 1,
    /// Quote abundant (B < B0, Q > Q0).
    BelowOne = 2,
}

impl DodoRState {
    /// Decode an on-chain `R` value into the typed enum.
    pub fn from_u256(value: U256) -> AmmResult<Self> {
        let v: u8 = value.try_into().map_err(|_| AmmError::InvariantViolation)?;
        match v {
            0 => Ok(Self::One),
            1 => Ok(Self::AboveOne),
            2 => Ok(Self::BelowOne),
            _ => Err(AmmError::InvariantViolation),
        }
    }
}

/// Direction of a DODO swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapDirection {
    /// Pay base, receive quote (base → quote).
    BaseToQuote,
    /// Pay quote, receive base (quote → base).
    QuoteToBase,
}

/// Internal PMM state used during quote/swap math.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PmmState {
    i: U256,
    k: U256,
    b: U256,
    q: U256,
    b0: U256,
    q0: U256,
    r: DodoRState,
}

/// Full on-chain DODO V2 pool state snapshot — what the engine caches per pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DodoState {
    /// Pool contract address.
    pub address: Address,
    /// PMM variant (DVM / DPP / DSP).
    pub pool_class: DodoPoolClass,
    /// Base token (token0).
    pub base_token: Address,
    /// Quote token (token1).
    pub quote_token: Address,
    /// Oracle price `i` (quote per base, 1e18 precision).
    pub i: U256,
    /// Curve concavity `K` (1e18 precision).
    pub k: U256,
    /// Base reserve.
    pub base_reserve: U256,
    /// Quote reserve.
    pub quote_reserve: U256,
    /// Base target (equilibrium reserve).
    pub base_target: U256,
    /// Quote target (equilibrium reserve).
    pub quote_target: U256,
    /// Current PMM R state.
    pub r_state: DodoRState,
    /// LP fee rate, 1e18 precision (e.g. 3e15 = 0.30%).
    pub lp_fee_rate_d18: U256,
    /// Maintainer fee rate, 1e18 precision.
    pub mt_fee_rate_d18: U256,
    /// Total fee in parts-per-million, conservative ceiling of `lp + mt` for the
    /// engine's marginal-rate filter (no precision loss because PPM is 1e6).
    pub fee_ppm: u32,
    /// Block number this snapshot is from.
    pub block_number: u64,
    /// Log index within the block (0 for view calls).
    pub log_index: u32,
}

/// Result of a DODO V2 quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DodoQuote {
    /// Input amount in native token units.
    pub amount_in_native: U256,
    /// Output amount before fees.
    pub gross_amount_out_native: U256,
    /// Output amount after LP + maintainer fees.
    pub amount_out_native: U256,
    /// LP fee component (deducted from gross output).
    pub lp_fee_native: U256,
    /// Maintainer fee component.
    pub mt_fee_native: U256,
    /// New `R` state after the swap.
    pub new_r_state: DodoRState,
    /// Updated base target after the swap.
    pub next_base_target: U256,
    /// Updated quote target after the swap.
    pub next_quote_target: U256,
}

// ============================================================================
// Helpers — U512 ↔ U256 conversion + fixed-point math
// ============================================================================

fn u512_to_u256(value: U512) -> AmmResult<U256> {
    let bytes = value.to_be_bytes::<64>();
    if bytes[..32].iter().any(|byte| *byte != 0) {
        return Err(AmmError::Overflow);
    }
    Ok(U256::from_be_slice(&bytes[32..]))
}

fn sqrt_u512(x: U512) -> U512 {
    if x <= U512::from(1u8) {
        return x;
    }
    let mut z = x;
    let mut y = (x >> 1) + U512::from(1u8);
    while y < z {
        z = y;
        y = ((x / y) + y) >> 1;
    }
    z
}

#[inline]
fn mul_floor(target: U256, rate_d18: U256) -> AmmResult<U256> {
    mul_div_floor(target, rate_d18, one_d18())
}

#[inline]
fn div_floor(target: U256, denominator: U256) -> AmmResult<U256> {
    if denominator.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    u512_to_u256(U512::from(target) * U512::from(one_d18()) / U512::from(denominator))
}

#[inline]
fn div_ceil(target: U256, denominator: U256) -> AmmResult<U256> {
    if denominator.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    let numerator = U512::from(target) * U512::from(one_d18());
    let denom = U512::from(denominator);
    let out = if numerator % denom == U512::ZERO {
        numerator / denom
    } else {
        numerator / denom + U512::from(1u8)
    };
    u512_to_u256(out)
}

#[inline]
fn reciprocal_floor(target: U256) -> AmmResult<U256> {
    if target.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    u512_to_u256(one_d36_u512() / U512::from(target))
}

#[inline]
fn mul_div_floor(a: U256, b: U256, denominator: U256) -> AmmResult<U256> {
    if denominator.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    u512_to_u256(U512::from(a) * U512::from(b) / U512::from(denominator))
}

#[inline]
fn mul_div_ceil(a: U256, b: U256, denominator: U256) -> AmmResult<U256> {
    if denominator.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    let numerator = U512::from(a) * U512::from(b);
    let denom = U512::from(denominator);
    let out = if numerator % denom == U512::ZERO {
        numerator / denom
    } else {
        numerator / denom + U512::from(1u8)
    };
    u512_to_u256(out)
}

/// Convert a d18 fee RATE (e.g. `lp_fee_rate_d18 + mt_fee_rate_d18`) to parts-per-million, ceil-rounded.
/// This is THE canonical `fee_ppm` derivation every normal Dodo producer maintains (see [`DodoState::apply_swap`]
/// and [`dodo_state_from_view`]); WI-06 T3 fix round 9 (N4) reuses it so a Shadow-origin merge can DERIVE
/// `fee_ppm` from the two CAS-compared rates rather than trust an assigned-but-uncompared batch value.
pub fn fee_rate_to_ppm(rate_d18: U256) -> AmmResult<u32> {
    let ppm = mul_div_ceil(rate_d18, U256::from(1_000_000u64), one_d18())?;
    ppm.try_into().map_err(|_| AmmError::InvariantViolation)
}

// ============================================================================
// PMM math — solve_quadratic_for_target, general_integrate, solve_quadratic_for_trade
// ============================================================================

fn adjusted_target(state: &mut PmmState) -> AmmResult<()> {
    match state.r {
        DodoRState::BelowOne => {
            state.q0 = solve_quadratic_for_target(
                state.q,
                state.b.checked_sub(state.b0).ok_or(AmmError::Underflow)?,
                state.i,
                state.k,
            )?;
        }
        DodoRState::AboveOne => {
            state.b0 = solve_quadratic_for_target(
                state.b,
                state.q.checked_sub(state.q0).ok_or(AmmError::Underflow)?,
                reciprocal_floor(state.i)?,
                state.k,
            )?;
        }
        DodoRState::One => {}
    }
    Ok(())
}

fn general_integrate(v0: U256, v1: U256, v2: U256, i: U256, k: U256) -> AmmResult<U256> {
    if v0.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    let delta = v1.checked_sub(v2).ok_or(AmmError::Underflow)?;
    let fair_amount = U512::from(i) * U512::from(delta);
    if k.is_zero() {
        return u512_to_u256(fair_amount / U512::from(one_d18()));
    }
    let v0_sq_div_v1 = u512_to_u256(U512::from(v0) * U512::from(v0) / U512::from(v1))?;
    let v0v0v1v2 = div_floor(v0_sq_div_v1, v2)?;
    let penalty = mul_floor(k, v0v0v1v2)?;
    let multiplier = U256::from(D18)
        .checked_sub(k)
        .ok_or(AmmError::Underflow)?
        .checked_add(penalty)
        .ok_or(AmmError::Overflow)?;
    u512_to_u256(U512::from(multiplier) * fair_amount / one_d36_u512())
}

fn solve_quadratic_for_target(v1: U256, delta: U256, i: U256, k: U256) -> AmmResult<U256> {
    if k.is_zero() {
        return v1
            .checked_add(mul_floor(i, delta)?)
            .ok_or(AmmError::Overflow);
    }
    if v1.is_zero() {
        return Ok(U256::ZERO);
    }
    let four = U256::from(4u8);
    let ki = u512_to_u256(U512::from(four) * U512::from(k) * U512::from(i))?;
    let sqrt_term = if ki.is_zero() || delta.is_zero() {
        one_d36_u512()
    } else {
        (U512::from(ki) * U512::from(delta) / U512::from(v1))
            + U512::from(U256::from(10u128.pow(36)))
    };
    let sqrt = u512_to_u256(sqrt_u512(sqrt_term))?;
    let premium = div_floor(
        sqrt.checked_sub(U256::from(D18))
            .ok_or(AmmError::Underflow)?,
        k.checked_mul(U256::from(2u8)).ok_or(AmmError::Overflow)?,
    )?
    .checked_add(U256::from(D18))
    .ok_or(AmmError::Overflow)?;
    mul_floor(v1, premium)
}

fn solve_quadratic_for_trade(v0: U256, v1: U256, delta: U256, i: U256, k: U256) -> AmmResult<U256> {
    if v0.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    if delta.is_zero() {
        return Ok(U256::ZERO);
    }
    if k.is_zero() {
        return Ok(mul_floor(i, delta)?.min(v1));
    }
    if k == U256::from(D18) {
        let temp = if i.is_zero() || delta.is_zero() {
            U256::ZERO
        } else {
            u512_to_u256(
                U512::from(i) * U512::from(delta) * U512::from(v1)
                    / U512::from(v0)
                    / U512::from(v0),
            )?
        };
        return u512_to_u256(
            U512::from(v1) * U512::from(temp) / U512::from(temp + U256::from(D18)),
        );
    }

    let part2 = u512_to_u256(
        ((U512::from(k) * U512::from(v0)) / U512::from(v1)) * U512::from(v0)
            + U512::from(i) * U512::from(delta),
    )?;
    let mut b_abs =
        U512::from(U256::from(D18).checked_sub(k).ok_or(AmmError::Underflow)?) * U512::from(v1);
    let b_sig;
    let part2_u512 = U512::from(part2);
    if b_abs >= part2_u512 {
        b_abs -= part2_u512;
        b_sig = false;
    } else {
        b_abs = part2_u512 - b_abs;
        b_sig = true;
    }
    let b_abs = u512_to_u256(b_abs / U512::from(U256::from(D18)))?;

    let four_one_minus_k = U256::from(4u8)
        .checked_mul(U256::from(D18).checked_sub(k).ok_or(AmmError::Underflow)?)
        .ok_or(AmmError::Overflow)?;
    let k_v0 = mul_floor(k, v0)?;
    let square_root_term = mul_floor(
        four_one_minus_k,
        u512_to_u256(U512::from(k_v0) * U512::from(v0))?,
    )?;
    let square_root = u512_to_u256(sqrt_u512(
        U512::from(b_abs) * U512::from(b_abs) + U512::from(square_root_term),
    ))?;

    let denominator = U256::from(D18)
        .checked_sub(k)
        .ok_or(AmmError::Underflow)?
        .checked_mul(U256::from(2u8))
        .ok_or(AmmError::Overflow)?;
    let numerator = if b_sig {
        square_root.checked_sub(b_abs).ok_or(AmmError::Underflow)?
    } else {
        square_root.checked_add(b_abs).ok_or(AmmError::Overflow)?
    };
    let v2 = div_ceil(numerator, denominator)?;
    if v2 > v1 {
        Ok(U256::ZERO)
    } else {
        v1.checked_sub(v2).ok_or(AmmError::Underflow)
    }
}

fn sell_base_token(state: &PmmState, pay_base_amount: U256) -> AmmResult<(U256, DodoRState)> {
    match state.r {
        DodoRState::One => Ok((
            solve_quadratic_for_trade(state.q0, state.q0, pay_base_amount, state.i, state.k)?,
            DodoRState::BelowOne,
        )),
        DodoRState::AboveOne => {
            let back_to_one_pay_base = state.b0.checked_sub(state.b).ok_or(AmmError::Underflow)?;
            let back_to_one_receive_quote =
                state.q.checked_sub(state.q0).ok_or(AmmError::Underflow)?;
            if pay_base_amount < back_to_one_pay_base {
                let receive = general_integrate(
                    state.b0,
                    state
                        .b
                        .checked_add(pay_base_amount)
                        .ok_or(AmmError::Overflow)?,
                    state.b,
                    state.i,
                    state.k,
                )?;
                Ok((receive.min(back_to_one_receive_quote), DodoRState::AboveOne))
            } else if pay_base_amount == back_to_one_pay_base {
                Ok((back_to_one_receive_quote, DodoRState::One))
            } else {
                let extra = solve_quadratic_for_trade(
                    state.q0,
                    state.q0,
                    pay_base_amount
                        .checked_sub(back_to_one_pay_base)
                        .ok_or(AmmError::Underflow)?,
                    state.i,
                    state.k,
                )?;
                Ok((
                    back_to_one_receive_quote
                        .checked_add(extra)
                        .ok_or(AmmError::Overflow)?,
                    DodoRState::BelowOne,
                ))
            }
        }
        DodoRState::BelowOne => Ok((
            solve_quadratic_for_trade(state.q0, state.q, pay_base_amount, state.i, state.k)?,
            DodoRState::BelowOne,
        )),
    }
}

fn sell_quote_token(state: &PmmState, pay_quote_amount: U256) -> AmmResult<(U256, DodoRState)> {
    let reciprocal_i = reciprocal_floor(state.i)?;
    match state.r {
        DodoRState::One => Ok((
            solve_quadratic_for_trade(state.b0, state.b0, pay_quote_amount, reciprocal_i, state.k)?,
            DodoRState::AboveOne,
        )),
        DodoRState::AboveOne => Ok((
            solve_quadratic_for_trade(state.b0, state.b, pay_quote_amount, reciprocal_i, state.k)?,
            DodoRState::AboveOne,
        )),
        DodoRState::BelowOne => {
            let back_to_one_pay_quote = state.q0.checked_sub(state.q).ok_or(AmmError::Underflow)?;
            let back_to_one_receive_base =
                state.b.checked_sub(state.b0).ok_or(AmmError::Underflow)?;
            if pay_quote_amount < back_to_one_pay_quote {
                let receive = general_integrate(
                    state.q0,
                    state
                        .q
                        .checked_add(pay_quote_amount)
                        .ok_or(AmmError::Overflow)?,
                    state.q,
                    reciprocal_i,
                    state.k,
                )?;
                Ok((receive.min(back_to_one_receive_base), DodoRState::BelowOne))
            } else if pay_quote_amount == back_to_one_pay_quote {
                Ok((back_to_one_receive_base, DodoRState::One))
            } else {
                let extra = solve_quadratic_for_trade(
                    state.b0,
                    state.b0,
                    pay_quote_amount
                        .checked_sub(back_to_one_pay_quote)
                        .ok_or(AmmError::Underflow)?,
                    reciprocal_i,
                    state.k,
                )?;
                Ok((
                    back_to_one_receive_base
                        .checked_add(extra)
                        .ok_or(AmmError::Overflow)?,
                    DodoRState::AboveOne,
                ))
            }
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

impl DodoState {
    /// Build the in-memory PMM state used by the math kernel.
    ///
    /// All three DODO classes (DVM/DPP/DSP) expose `getPMMStateForCall()`, which returns
    /// already-resolved `(i,K,B,Q,B0,Q0,R)` — the same `adjustedTarget`-resolved targets the
    /// pool itself prices against. We therefore seed the PMM state from those snapshot targets
    /// and `R` for EVERY class and let [`adjusted_target`] idempotently re-confirm them.
    ///
    /// Previously the DVM arm discarded the snapshot `B0/Q0/R`, forcing `R=AboveOne` with zero
    /// targets and re-deriving from scratch. That only matches on-chain when the pool happens to
    /// sit in `AboveOne` with `Q0≈0`; for a pool in `BelowOne`/`One` it skewed the quote by
    /// ~0.25% (verified against on-chain `querySellBase`/`querySellQuote`), which surfaced as a
    /// phantom ~0.13% gross on every DODO-leg route. Trusting the snapshot targets matches the
    /// chain to the wei for all classes (a true DVM still returns `B0=0,Q0=0,R=AboveOne` here, so
    /// it reduces to the old behaviour).
    fn effective_pmm_state(&self) -> AmmResult<PmmState> {
        let mut state = PmmState {
            i: self.i,
            k: self.k,
            b: self.base_reserve,
            q: self.quote_reserve,
            b0: self.base_target,
            q0: self.quote_target,
            r: self.r_state,
        };
        adjusted_target(&mut state)?;
        Ok(state)
    }

    /// Detect the swap direction for `(token_in, token_out)`. Returns `None` if the
    /// pair is unsupported by this pool.
    #[inline]
    pub fn direction_for_tokens(
        &self,
        token_in: Address,
        token_out: Address,
    ) -> Option<SwapDirection> {
        if token_in == self.base_token && token_out == self.quote_token {
            Some(SwapDirection::BaseToQuote)
        } else if token_in == self.quote_token && token_out == self.base_token {
            Some(SwapDirection::QuoteToBase)
        } else {
            None
        }
    }

    /// Conservative fee bound for the marginal-rate pre-filter.
    #[inline]
    pub fn conservative_fee_ppm(&self) -> u32 {
        self.fee_ppm
    }

    /// Quote an exact-input swap for `(token_in, token_out)`.
    pub fn quote_for_tokens_exact_in(
        &self,
        token_in: Address,
        token_out: Address,
        amount_in_native: U256,
    ) -> AmmResult<DodoQuote> {
        let direction = self
            .direction_for_tokens(token_in, token_out)
            .ok_or(AmmError::InvariantViolation)?;
        self.quote_exact_in_local(amount_in_native, direction)
    }

    /// Quote an exact-input swap with a pre-resolved direction.
    pub fn quote_exact_in_local(
        &self,
        amount_in_native: U256,
        direction: SwapDirection,
    ) -> AmmResult<DodoQuote> {
        if amount_in_native.is_zero() {
            return Err(AmmError::ZeroInput);
        }
        let state = self.effective_pmm_state()?;
        let (gross_amount_out_native, new_r_state) = match direction {
            SwapDirection::BaseToQuote => sell_base_token(&state, amount_in_native)?,
            SwapDirection::QuoteToBase => sell_quote_token(&state, amount_in_native)?,
        };
        if gross_amount_out_native.is_zero() {
            return Err(AmmError::ZeroLiquidity);
        }
        let lp_fee_native = mul_floor(gross_amount_out_native, self.lp_fee_rate_d18)?;
        let mt_fee_native = mul_floor(gross_amount_out_native, self.mt_fee_rate_d18)?;
        let amount_out_native = gross_amount_out_native
            .checked_sub(lp_fee_native)
            .ok_or(AmmError::Underflow)?
            .checked_sub(mt_fee_native)
            .ok_or(AmmError::Underflow)?;
        let (next_base_target, next_quote_target) =
            self.next_targets_after_trade(&state, direction, new_r_state)?;

        Ok(DodoQuote {
            amount_in_native,
            gross_amount_out_native,
            amount_out_native,
            lp_fee_native,
            mt_fee_native,
            new_r_state,
            next_base_target,
            next_quote_target,
        })
    }

    /// Predict the post-trade state of a swap for the feed-driven state predictor
    /// (TAIL_LATENCY_PLAN C2). Returns the net output and a new `DodoState` with the swapped
    /// reserves and the post-trade PMM fields (`r_state`, base/quote targets) that
    /// [`quote_exact_in_local`] computes — closing the i/K/R 30 s staleness the audit flagged.
    /// The oracle price `i` and concavity `K` are left unchanged (they update via the price oracle,
    /// not the swap). `quote_reserve`/`base_reserve` move by `(+amount_in, -(amount_out_native +
    /// mt_fee_native))` — BYTE-EXACT: the maintainer fee also leaves the pool on-chain
    /// (`_transferQuoteOut(_MAINTAINER_, mtFee)`), so it is now subtracted too (only the LP fee is
    /// retained). This closes the few-bps same-block drift that flipped a thin-margin V4>DODO
    /// candidate to an on-chain Loss (Gate9 route268).
    pub fn apply_swap(
        &self,
        amount_in_native: U256,
        direction: SwapDirection,
    ) -> AmmResult<(U256, DodoState)> {
        let q = self.quote_exact_in_local(amount_in_native, direction)?;
        let mut new = self.clone();
        // Tokens LEAVING the pool on the output side = net_to_user + maintainer fee. On-chain DODO
        // transfers the maintainer fee out (`_transferQuoteOut(_MAINTAINER_, mtFee)`), so the cached
        // reserve drops by (net + mtFee); only the LP fee is retained in the reserve. Omitting mtFee
        // here drifts the same-block forward-apply by a few bps, which can flip a thin-margin DODO
        // candidate from predicted-profit to on-chain Loss (Gate9 route268, UniswapV4>DodoV2).
        let out_plus_mt = q
            .amount_out_native
            .checked_add(q.mt_fee_native)
            .ok_or(AmmError::Overflow)?;
        match direction {
            SwapDirection::BaseToQuote => {
                new.base_reserve = self
                    .base_reserve
                    .checked_add(amount_in_native)
                    .ok_or(AmmError::Overflow)?;
                new.quote_reserve = self
                    .quote_reserve
                    .checked_sub(out_plus_mt)
                    .ok_or(AmmError::Underflow)?;
            }
            SwapDirection::QuoteToBase => {
                new.quote_reserve = self
                    .quote_reserve
                    .checked_add(amount_in_native)
                    .ok_or(AmmError::Overflow)?;
                new.base_reserve = self
                    .base_reserve
                    .checked_sub(out_plus_mt)
                    .ok_or(AmmError::Underflow)?;
            }
        }
        new.r_state = q.new_r_state;
        new.base_target = q.next_base_target;
        new.quote_target = q.next_quote_target;
        Ok((q.amount_out_native, new))
    }

    fn next_targets_after_trade(
        &self,
        state: &PmmState,
        direction: SwapDirection,
        new_r_state: DodoRState,
    ) -> AmmResult<(U256, U256)> {
        // Class-agnostic: the resolved targets carried in `state` (seeded from
        // `getPMMStateForCall`) are what the pool prices against, for DVM/DPP/DSP alike.
        let mut base_target = state.b0;
        let mut quote_target = state.q0;
        if state.r != new_r_state {
            match direction {
                SwapDirection::BaseToQuote => base_target = state.b0,
                SwapDirection::QuoteToBase => quote_target = state.q0,
            }
        }
        Ok((base_target, quote_target))
    }

    /// Apply an exact-input swap in place, mutating reserves + targets + R state.
    pub fn apply_exact_in_local(
        &mut self,
        amount_in_native: U256,
        direction: SwapDirection,
        block_number: u64,
        log_index: u32,
    ) -> AmmResult<DodoQuote> {
        let quote = self.quote_exact_in_local(amount_in_native, direction)?;
        match direction {
            SwapDirection::BaseToQuote => {
                self.base_reserve = self
                    .base_reserve
                    .checked_add(amount_in_native)
                    .ok_or(AmmError::Overflow)?;
                self.quote_reserve = self
                    .quote_reserve
                    .checked_sub(
                        quote
                            .amount_out_native
                            .checked_add(quote.mt_fee_native)
                            .ok_or(AmmError::Overflow)?,
                    )
                    .ok_or(AmmError::ZeroLiquidity)?;
            }
            SwapDirection::QuoteToBase => {
                self.quote_reserve = self
                    .quote_reserve
                    .checked_add(amount_in_native)
                    .ok_or(AmmError::Overflow)?;
                self.base_reserve = self
                    .base_reserve
                    .checked_sub(
                        quote
                            .amount_out_native
                            .checked_add(quote.mt_fee_native)
                            .ok_or(AmmError::Overflow)?,
                    )
                    .ok_or(AmmError::ZeroLiquidity)?;
            }
        }
        // Class-agnostic roll-forward: adopt the post-trade R + targets, then re-derive the
        // resolved targets exactly as `getPMMStateForCall` would for the next quote.
        self.base_target = quote.next_base_target;
        self.quote_target = quote.next_quote_target;
        self.r_state = quote.new_r_state;
        let next = self.effective_pmm_state()?;
        self.base_target = next.b0;
        self.quote_target = next.q0;
        self.r_state = next.r;
        self.fee_ppm = fee_rate_to_ppm(
            self.lp_fee_rate_d18
                .checked_add(self.mt_fee_rate_d18)
                .ok_or(AmmError::Overflow)?,
        )?;
        self.block_number = block_number;
        self.log_index = log_index;
        Ok(quote)
    }

    /// Available output-side liquidity (used for sizing caps).
    pub fn output_liquidity_native(&self, direction: SwapDirection) -> U256 {
        match direction {
            SwapDirection::BaseToQuote => self.quote_reserve,
            SwapDirection::QuoteToBase => self.base_reserve,
        }
    }
}

/// Build a `DodoState` from on-chain `getPMMStateForCall + getVaultReserve + getUserFeeRate`
/// results. Caller is responsible for fetching these via RPC; the math kernel takes a
/// raw tuple so it stays RPC-agnostic.
#[allow(clippy::too_many_arguments)]
pub fn dodo_state_from_view(
    address: Address,
    pool_class: DodoPoolClass,
    base_token: Address,
    quote_token: Address,
    i: U256,
    k: U256,
    base_reserve: U256,
    quote_reserve: U256,
    base_target: U256,
    quote_target: U256,
    r_raw: U256,
    lp_fee_rate_d18: U256,
    mt_fee_rate_d18: U256,
    block_number: u64,
    log_index: u32,
) -> AmmResult<DodoState> {
    let total_fee_rate = lp_fee_rate_d18
        .checked_add(mt_fee_rate_d18)
        .ok_or(AmmError::Overflow)?;
    Ok(DodoState {
        address,
        pool_class,
        base_token,
        quote_token,
        i,
        k,
        base_reserve,
        quote_reserve,
        base_target,
        quote_target,
        r_state: DodoRState::from_u256(r_raw)?,
        lp_fee_rate_d18,
        mt_fee_rate_d18,
        fee_ppm: fee_rate_to_ppm(total_fee_rate)?,
        block_number,
        log_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state(pool_class: DodoPoolClass) -> DodoState {
        DodoState {
            address: Address::from([0x11; 20]),
            pool_class,
            base_token: Address::from([0x22; 20]),
            quote_token: Address::from([0x33; 20]),
            i: U256::from(2u8) * U256::from(D18),
            k: U256::from(D18 / 2),
            base_reserve: U256::from(100u64) * U256::from(D18),
            quote_reserve: U256::from(200u64) * U256::from(D18),
            base_target: U256::from(100u64) * U256::from(D18),
            quote_target: U256::from(200u64) * U256::from(D18),
            r_state: DodoRState::One,
            lp_fee_rate_d18: U256::from(3_000_000_000_000_000u64),
            mt_fee_rate_d18: U256::from(1_000_000_000_000_000u64),
            fee_ppm: 4_000,
            block_number: 1,
            log_index: 0,
        }
    }

    #[test]
    fn direction_matches_base_quote_orientation() {
        let state = sample_state(DodoPoolClass::Dvm);
        assert_eq!(
            state.direction_for_tokens(state.base_token, state.quote_token),
            Some(SwapDirection::BaseToQuote)
        );
        assert_eq!(
            state.direction_for_tokens(state.quote_token, state.base_token),
            Some(SwapDirection::QuoteToBase)
        );
    }

    #[test]
    fn apply_swap_moves_reserves_and_updates_pmm_fields() {
        let state = sample_state(DodoPoolClass::Dpp);
        let dx = U256::from(10u64) * U256::from(D18);
        let (dy, new) = state.apply_swap(dx, SwapDirection::BaseToQuote).unwrap();
        // Output equals the kernel's net quote.
        let q = state
            .quote_exact_in_local(dx, SwapDirection::BaseToQuote)
            .unwrap();
        assert_eq!(dy, q.amount_out_native);
        // Base reserve up by dx; quote reserve down by net output + maintainer fee (mtFee leaves
        // the pool on-chain — see apply_swap_reserve_decrease_includes_maintainer_fee_byte_exact).
        assert_eq!(new.base_reserve, state.base_reserve + dx);
        assert_eq!(
            new.quote_reserve,
            state.quote_reserve - dy - q.mt_fee_native
        );
        // Post-trade PMM fields tracked (the i/K/R staleness the audit flagged).
        assert_eq!(new.r_state, q.new_r_state);
        assert_eq!(new.base_target, q.next_base_target);
        assert_eq!(new.quote_target, q.next_quote_target);
        // Oracle price + concavity unchanged (they move via the oracle, not the swap).
        assert_eq!(new.i, state.i);
        assert_eq!(new.k, state.k);
        // Price impact: after buying quote with base, the next identical base→quote yields less.
        let dy2 = new
            .quote_exact_in_local(dx, SwapDirection::BaseToQuote)
            .unwrap()
            .amount_out_native;
        assert!(dy2 < dy, "post-trade rate must be worse: dy2={dy2} dy={dy}");
    }

    #[test]
    fn apply_swap_reserve_decrease_includes_maintainer_fee_byte_exact() {
        // [Gate9 route268 root cause] On-chain DODO transfers the maintainer fee OUT of the pool
        // (`_transferQuoteOut(_MAINTAINER_, mtFee)`), so the outgoing reserve decreases by
        // (net_to_user + mtFee), NOT just net. The same-block forward-apply previously omitted mtFee
        // ("a few bps"), which leaked into REVM corpus-validation and produced a thin-margin
        // V4>DODO false positive (predicted +profit, on-chain Loss). The forward-applied reserve
        // MUST be byte-exact so a same-block candidate trading the same pool prices correctly.
        let state = sample_state(DodoPoolClass::Dpp);
        assert!(
            !state.mt_fee_rate_d18.is_zero(),
            "test needs a nonzero mt fee"
        );

        // BaseToQuote: quote leaves the pool = net + mtFee.
        let dx = U256::from(10u64) * U256::from(D18);
        let (dy, new) = state.apply_swap(dx, SwapDirection::BaseToQuote).unwrap();
        let q = state
            .quote_exact_in_local(dx, SwapDirection::BaseToQuote)
            .unwrap();
        assert!(
            !q.mt_fee_native.is_zero(),
            "this trade must incur a maintainer fee"
        );
        assert_eq!(dy, q.amount_out_native);
        assert_eq!(new.base_reserve, state.base_reserve + dx);
        assert_eq!(
            new.quote_reserve,
            state.quote_reserve - q.amount_out_native - q.mt_fee_native,
            "quote reserve must drop by net + maintainer fee (mtFee leaves the pool on-chain)"
        );

        // QuoteToBase: base leaves the pool = net + mtFee.
        let dxq = U256::from(20u64) * U256::from(D18);
        let (dyb, newb) = state.apply_swap(dxq, SwapDirection::QuoteToBase).unwrap();
        let qb = state
            .quote_exact_in_local(dxq, SwapDirection::QuoteToBase)
            .unwrap();
        assert!(
            !qb.mt_fee_native.is_zero(),
            "this trade must incur a maintainer fee"
        );
        assert_eq!(dyb, qb.amount_out_native);
        assert_eq!(newb.quote_reserve, state.quote_reserve + dxq);
        assert_eq!(
            newb.base_reserve,
            state.base_reserve - qb.amount_out_native - qb.mt_fee_native,
            "base reserve must drop by net + maintainer fee (mtFee leaves the pool on-chain)"
        );
    }

    #[test]
    fn exact_in_quote_is_positive() {
        let state = sample_state(DodoPoolClass::Dpp);
        let quote = state
            .quote_exact_in_local(
                U256::from(10u64) * U256::from(D18),
                SwapDirection::BaseToQuote,
            )
            .unwrap();
        assert!(quote.amount_out_native > U256::ZERO);
        assert!(quote.gross_amount_out_native > quote.amount_out_native);
    }

    #[test]
    fn apply_exact_in_updates_reserves() {
        let mut state = sample_state(DodoPoolClass::Dsp);
        let base_before = state.base_reserve;
        let quote_before = state.quote_reserve;
        state
            .apply_exact_in_local(
                U256::from(5u64) * U256::from(D18),
                SwapDirection::BaseToQuote,
                12,
                7,
            )
            .unwrap();
        assert!(state.base_reserve > base_before);
        assert!(state.quote_reserve < quote_before);
        assert_eq!(state.block_number, 12);
        assert_eq!(state.log_index, 7);
    }

    #[test]
    fn pair_view_conversion_rounds_fee_to_ppm() {
        let state = dodo_state_from_view(
            Address::from([0x66; 20]),
            DodoPoolClass::Dpp,
            Address::from([0x44; 20]),
            Address::from([0x55; 20]),
            U256::from(D18),
            U256::ZERO,
            U256::from(1u8),
            U256::from(1u8),
            U256::from(1u8),
            U256::from(1u8),
            U256::from(0u8),
            U256::from(3_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000u64),
            1,
            0,
        )
        .unwrap();
        assert_eq!(state.fee_ppm, 4_000);
    }
}

/// K2: randomized property tests for the DODO V2 PMM kernel (perturb a known-good state by amount).
/// No panic; output monotonic in input.
#[cfg(test)]
mod dodo_property {
    use super::*;
    use proptest::prelude::*;

    fn sample() -> DodoState {
        DodoState {
            address: Address::from([0x11; 20]),
            pool_class: DodoPoolClass::Dvm,
            base_token: Address::from([0x22; 20]),
            quote_token: Address::from([0x33; 20]),
            i: U256::from(2u8) * U256::from(D18),
            k: U256::from(D18 / 2),
            base_reserve: U256::from(100u64) * U256::from(D18),
            quote_reserve: U256::from(200u64) * U256::from(D18),
            base_target: U256::from(100u64) * U256::from(D18),
            quote_target: U256::from(200u64) * U256::from(D18),
            r_state: DodoRState::One,
            lp_fee_rate_d18: U256::from(3_000_000_000_000_000u64),
            mt_fee_rate_d18: U256::from(1_000_000_000_000_000u64),
            fee_ppm: 4_000,
            block_number: 1,
            log_index: 0,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1200))]

        #[test]
        fn monotonic_in_amount(a in 1u64..=40u64, bump in 1u64..=40u64) {
            let s = sample();
            let dir = SwapDirection::BaseToQuote;
            let o1 = s.quote_exact_in_local(U256::from(a) * U256::from(D18), dir);
            let o2 = s.quote_exact_in_local(U256::from(a + bump) * U256::from(D18), dir);
            if let (Ok(q1), Ok(q2)) = (o1, o2) {
                prop_assert!(q2.amount_out_native >= q1.amount_out_native, "more in ⇒ not less out");
            }
        }
    }
}
