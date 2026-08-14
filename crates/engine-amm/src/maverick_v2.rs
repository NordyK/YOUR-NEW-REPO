//! Maverick V2 dual-reserve tick AMM math kernel.
//!
//! Maverick V2 uses bidirectional ticks with explicit `reserveA` / `reserveB` per
//! tick, plus `sqrt_lower / sqrt_upper` price bounds and a `liquidity_d18`
//! amount. Unlike Uniswap V3 (single reserve, ticks store only `liquidity_net`),
//! Maverick records both sides explicitly so swap math walks tick-by-tick using
//! the bounded reserves directly.
//!
//! Ported from the Fulcrum codebase. Pure math kernel — RPC bindings and
//! discovery live in separate modules.
//!
//! A8/A9 (verified-correct, locked): Maverick V2 is EXCLUDED from live use — this module is kept
//! for reference and parity research only, never on the money path. Two deliberate gaps make it
//! unsafe to wire as-is:
//!   * A8: `lending_fee_rate_d18` (the field) is carried but never applied, and the per-step
//!     `_protocol_fee` (see [`swap_step_exact_in`]) is computed and then discarded — so the kernel
//!     does not split LP vs protocol fees the way the on-chain pool does.
//!   * A9: multi-`kind` pools (the `kinds` bitmask: right/left/both/static modes) are only
//!     APPROXIMATED — this walk treats liquidity as a single mode and does not reproduce Maverick's
//!     per-kind bin movement, so quotes can diverge from chain.
//! Because it is never enabled, these approximations cost nothing; do NOT promote this kernel to a
//! live quoter without closing both gaps. Behavior is intentionally unchanged.

use alloy_primitives::{Address, U256, U512};

use crate::error::{AmmError, AmmResult};

/// 1e18 fixed-point precision.
pub const D18: u128 = 1_000_000_000_000_000_000;
/// 1e3 — Maverick uses this for protocol-fee ratio.
pub const D3: u64 = 1_000;
/// Default tick radius to fetch from the lens.
pub const DEFAULT_TICK_RADIUS: i32 = 64;
/// Hard cap on tick radius to bound RPC cost.
pub const MAX_TICK_RADIUS: i32 = 256;
/// Defence cap on inner-loop iterations during a single quote.
pub const MAX_HOPS_PER_LOCAL_QUOTE: usize = 512;
/// Sentinel for "no tick limit" toward +∞.
pub const MAX_TICK_I32: i32 = i32::MAX;
/// Sentinel for "no tick limit" toward -∞.
pub const MIN_TICK_I32: i32 = i32::MIN;

/// Function selectors for direct pool / router calls.
pub mod selectors {
    /// `swap` on the pool — selector `0x3eece7db`.
    pub const POOL_SWAP: [u8; 4] = [0x3e, 0xec, 0xe7, 0xdb];
}

/// Minimum number of pinned live Maverick quote vectors required before the engine may
/// enable Maverick discovery or quote routing.
pub const MIN_LIVE_REFERENCE_VECTORS: usize = 3;
/// Current count of reviewed live Maverick quote vectors wired into the parity harness.
///
/// This stays at zero until real Arbitrum pool/quoter vectors are captured and reviewed. The
/// runtime `MAVERICK_ENABLE` gate consults [`live_reference_vectors_ready`], so the operator flag
/// cannot accidentally promote the reference-only kernel while this count is below the minimum.
pub const LIVE_REFERENCE_VECTOR_COUNT: usize = 0;

/// True only after the in-repository live-vector parity harness has enough reviewed vectors to
/// support opt-in runtime Maverick quoting.
#[inline]
pub const fn live_reference_vectors_ready() -> bool {
    LIVE_REFERENCE_VECTOR_COUNT >= MIN_LIVE_REFERENCE_VECTORS
}

/// Swap direction relative to `(token0, token1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapDirection {
    /// Pay token0, receive token1.
    ZeroToOne,
    /// Pay token1, receive token0.
    OneToZero,
}

impl SwapDirection {
    /// Maverick's `tokenAIn` flag — true when paying token0.
    #[inline]
    pub const fn token_a_in(self) -> bool {
        matches!(self, Self::ZeroToOne)
    }

    /// Default tick limit (sentinel) for this direction.
    #[inline]
    pub const fn default_tick_limit(self) -> i32 {
        if self.token_a_in() {
            MAX_TICK_I32
        } else {
            MIN_TICK_I32
        }
    }
}

/// Per-token decimal scaling. Maverick V2 stores reserves in a 1e18 domain and
/// scales raw token amounts in/out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenScale {
    /// Raw scale word from `tokenAScale()` / `tokenBScale()`.
    pub raw: u64,
    /// Magnitude of the scale (a power of ten).
    pub factor: U256,
    /// `true` if the token has more than 18 decimals.
    pub more_than_18: bool,
}

impl TokenScale {
    /// Decode the on-chain scale word.
    pub fn from_pool_scale(raw: U256) -> AmmResult<Self> {
        let raw_u64: u64 = raw.try_into().map_err(|_| AmmError::InvariantViolation)?;
        let more_than_18 = (raw_u64 >> 63) == 1u64;
        let magnitude = raw_u64 & 0x7fff_ffff_ffff_ffff;
        Ok(Self {
            raw: raw_u64,
            factor: U256::from(magnitude.max(1)),
            more_than_18,
        })
    }

    /// Native → 1e18 domain (floor).
    #[inline]
    pub fn to_d18_floor(self, native_amount: U256) -> AmmResult<U256> {
        if self.more_than_18 {
            Ok(native_amount / self.factor)
        } else {
            native_amount
                .checked_mul(self.factor)
                .ok_or(AmmError::Overflow)
        }
    }

    /// 1e18 domain → native (floor).
    #[inline]
    pub fn from_d18_floor(self, d18_amount: U256) -> AmmResult<U256> {
        if self.more_than_18 {
            d18_amount
                .checked_mul(self.factor)
                .ok_or(AmmError::Overflow)
        } else {
            Ok(d18_amount / self.factor)
        }
    }

    /// 1e18 domain → native (ceil), used for exact-output sizing.
    #[inline]
    pub fn from_d18_ceil(self, d18_amount: U256) -> AmmResult<U256> {
        if self.more_than_18 {
            d18_amount
                .checked_mul(self.factor)
                .ok_or(AmmError::Overflow)
        } else {
            mul_div_up(d18_amount, U256::from(1u8), self.factor)
        }
    }
}

/// One Maverick V2 tick — explicit per-side reserves + sqrt-price bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaverickTick {
    /// Tick index.
    pub tick: i32,
    /// Token0 reserve at this tick (1e18 domain).
    pub reserve0_d18: U256,
    /// Token1 reserve at this tick (1e18 domain).
    pub reserve1_d18: U256,
    /// LP total supply at this tick.
    pub total_supply: U256,
    /// Liquidity (1e18 domain).
    pub liquidity_d18: U256,
    /// Lower sqrt-price bound of the tick.
    pub sqrt_lower_price_d18: U256,
    /// Upper sqrt-price bound of the tick.
    pub sqrt_upper_price_d18: U256,
}

/// Maverick V2 pool snapshot — the cached on-chain state the engine quotes against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaverickState {
    /// Pool contract address.
    pub address: Address,
    /// Token0 (a).
    pub token0: Address,
    /// Token1 (b).
    pub token1: Address,
    /// Token0 scale.
    pub token0_scale: TokenScale,
    /// Token1 scale.
    pub token1_scale: TokenScale,
    /// Fee for zero→one swaps (1e18 precision).
    pub fee_zto_d18: U256,
    /// Fee for one→zero swaps (1e18 precision).
    pub fee_otz_d18: U256,
    /// Tick spacing.
    pub tick_spacing: u16,
    /// TWA lookback (seconds).
    pub lookback: u32,
    /// Bitmask of supported pool kinds (set by deployer). A9: multi-kind bins are only
    /// approximated by this (excluded, reference-only) kernel — see the module-level note.
    pub kinds: u8,
    /// Lending fee rate (1e18 precision). A8: carried for fidelity but NEVER applied by this
    /// kernel (Maverick is excluded from live use — see the module-level note).
    pub lending_fee_rate_d18: U256,
    /// Protocol fee ratio (1e3 precision).
    pub protocol_fee_ratio_d3: u8,
    /// `true` if the pool requires permissioned swaps (unsupported here).
    pub permissioned_swap: bool,
    /// Current sqrt price (1e18 precision).
    pub sqrt_price_d18: U256,
    /// Active tick.
    pub active_tick: i32,
    /// Pool-wide token0 reserve (1e18).
    pub reserve0_d18: U256,
    /// Pool-wide token1 reserve (1e18).
    pub reserve1_d18: U256,
    /// Timestamp of the snapshot block.
    pub last_timestamp: u64,
    /// Snapshot block number.
    pub block_number: u64,
    /// Log index within block.
    pub log_index: u32,
    /// Tick radius used when fetching this snapshot.
    pub tick_radius: i32,
    /// Ticks within `±tick_radius` of `active_tick`, sorted by index.
    pub ticks: Vec<MaverickTick>,
}

/// Result of a Maverick V2 quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaverickQuote {
    /// Native-decimal input.
    pub amount_in_native: U256,
    /// 1e18-domain input (after scaling).
    pub amount_in_d18: U256,
    /// Native-decimal output.
    pub amount_out_native: U256,
    /// 1e18-domain output.
    pub amount_out_d18: U256,
    /// Total fee (1e18 domain).
    pub total_fee_d18: U256,
    /// Protocol fee component.
    pub protocol_fee_d18: U256,
    /// LP fee component.
    pub lp_fee_d18: U256,
    /// Final tick after the swap.
    pub final_tick: i32,
    /// Final sqrt price.
    pub final_sqrt_price_d18: U256,
    /// Number of tick crossings.
    pub ticks_crossed: u32,
    /// `true` if the quote exhausted the cached tick window before filling.
    pub partial_fill: bool,
}

/// One step of the multi-tick swap walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapStep {
    /// Gross input consumed including fee, 1e18 domain.
    pub amount_in_total_d18: U256,
    /// Net input after fee, 1e18 domain.
    pub amount_in_net_d18: U256,
    /// Output, 1e18 domain.
    pub amount_out_d18: U256,
    /// Total fee, 1e18 domain.
    pub total_fee_d18: U256,
    /// Protocol fee component.
    pub protocol_fee_d18: U256,
    /// LP fee component.
    pub lp_fee_d18: U256,
    /// New sqrt price after this step.
    pub new_sqrt_price_d18: U256,
    /// `true` if this step crossed the tick boundary.
    pub crossed_tick: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MaverickQuoteD18 {
    amount_in_d18: U256,
    amount_out_d18: U256,
    total_fee_d18: U256,
    protocol_fee_d18: U256,
    lp_fee_d18: U256,
    final_tick: i32,
    final_sqrt_price_d18: U256,
    ticks_crossed: u32,
    partial_fill: bool,
}

// ============================================================================
// U512 helpers + fixed-point math
// ============================================================================

/// 512-bit `floor((a * b) / denominator)`.
#[inline]
pub fn mul_div_down(a: U256, b: U256, denominator: U256) -> AmmResult<U256> {
    if denominator.is_zero() {
        return Err(AmmError::InvariantViolation);
    }
    let out = U512::from(a) * U512::from(b) / U512::from(denominator);
    u512_to_u256(out)
}

/// 512-bit `ceil((a * b) / denominator)`.
#[inline]
pub fn mul_div_up(a: U256, b: U256, denominator: U256) -> AmmResult<U256> {
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

#[inline]
fn mul_d18(a: U256, b: U256) -> AmmResult<U256> {
    mul_div_down(a, b, U256::from(D18))
}

#[inline]
fn div_d18(a: U256, b: U256) -> AmmResult<U256> {
    mul_div_down(a, U256::from(D18), b)
}

#[inline]
fn div_d18_up(a: U256, b: U256) -> AmmResult<U256> {
    mul_div_up(a, U256::from(D18), b)
}

fn u512_to_u256(value: U512) -> AmmResult<U256> {
    let bytes = value.to_be_bytes::<64>();
    if bytes[..32].iter().any(|byte| *byte != 0) {
        return Err(AmmError::Overflow);
    }
    Ok(U256::from_be_slice(&bytes[32..]))
}

// ============================================================================
// Swap step math
// ============================================================================

fn swap_step_exact_in(
    sqrt_price: U256,
    liquidity: U256,
    amount_in_total: U256,
    token_a_in: bool,
    fee_d18: U256,
    protocol_fee_ratio_d3: u8,
    sqrt_lower: U256,
    sqrt_upper: U256,
) -> AmmResult<SwapStep> {
    if liquidity.is_zero() || amount_in_total.is_zero() {
        return Ok(SwapStep {
            amount_in_total_d18: U256::ZERO,
            amount_in_net_d18: U256::ZERO,
            amount_out_d18: U256::ZERO,
            total_fee_d18: U256::ZERO,
            protocol_fee_d18: U256::ZERO,
            lp_fee_d18: U256::ZERO,
            new_sqrt_price_d18: sqrt_price,
            crossed_tick: false,
        });
    }

    let total_fee = mul_div_up(amount_in_total, fee_d18, U256::from(D18))?;
    let amount_in_net = amount_in_total.saturating_sub(total_fee);
    // A8: protocol-fee split computed for parity but DISCARDED — this excluded kernel does not
    // apply the LP/protocol fee split (Maverick is reference-only; see module-level note).
    let _protocol_fee = mul_div_down(total_fee, U256::from(protocol_fee_ratio_d3), U256::from(D3))?;

    let max_net_in = if token_a_in {
        mul_d18(liquidity, sqrt_upper.saturating_sub(sqrt_price))?
    } else {
        div_d18(liquidity, sqrt_lower)?.saturating_sub(div_d18(liquidity, sqrt_price)?)
    };

    let consumed_net = amount_in_net.min(max_net_in);
    let crossed_tick = !max_net_in.is_zero() && amount_in_net >= max_net_in;

    let new_sqrt_price = if token_a_in {
        sqrt_price
            .saturating_add(div_d18_up(consumed_net, liquidity)?)
            .min(sqrt_upper)
    } else {
        let denom = div_d18(liquidity, sqrt_price)?.saturating_add(consumed_net);
        div_d18(liquidity, denom)?.max(sqrt_lower)
    };

    let amount_out = if token_a_in {
        let numerator = mul_d18(liquidity, new_sqrt_price.saturating_sub(sqrt_price))?;
        let denominator = mul_d18(sqrt_price, new_sqrt_price)?;
        mul_div_down(numerator, U256::from(D18), denominator)?
    } else {
        mul_d18(liquidity, sqrt_price.saturating_sub(new_sqrt_price))?
    };

    let actual_in_total = if consumed_net == amount_in_net {
        amount_in_total
    } else {
        mul_div_up(
            consumed_net,
            U256::from(D18),
            U256::from(D18).saturating_sub(fee_d18),
        )?
    };
    let actual_fee = actual_in_total.saturating_sub(consumed_net);
    let actual_protocol_fee = mul_div_down(
        actual_fee,
        U256::from(protocol_fee_ratio_d3),
        U256::from(D3),
    )?;
    let actual_lp_fee = actual_fee.saturating_sub(actual_protocol_fee);

    Ok(SwapStep {
        amount_in_total_d18: actual_in_total,
        amount_in_net_d18: consumed_net,
        amount_out_d18: amount_out,
        total_fee_d18: actual_fee,
        protocol_fee_d18: actual_protocol_fee,
        lp_fee_d18: actual_lp_fee,
        new_sqrt_price_d18: new_sqrt_price,
        crossed_tick,
    })
}

// ============================================================================
// Public API
// ============================================================================

impl MaverickState {
    /// Detect swap direction for `(token_in, token_out)`.
    #[inline]
    pub fn direction_for_tokens(
        &self,
        token_in: Address,
        token_out: Address,
    ) -> Option<SwapDirection> {
        if token_in == self.token0 && token_out == self.token1 {
            Some(SwapDirection::ZeroToOne)
        } else if token_in == self.token1 && token_out == self.token0 {
            Some(SwapDirection::OneToZero)
        } else {
            None
        }
    }

    /// Conservative fee bound — return max of bidirectional fees as PPM.
    pub fn conservative_fee_ppm(&self) -> AmmResult<u32> {
        let bigger = self.fee_zto_d18.max(self.fee_otz_d18);
        let ppm = mul_div_up(bigger, U256::from(1_000_000u64), U256::from(D18))?;
        ppm.try_into().map_err(|_| AmmError::InvariantViolation)
    }

    /// Quote exact-input swap for token pair.
    pub fn quote_for_tokens_exact_in(
        &self,
        token_in: Address,
        token_out: Address,
        amount_in_native: U256,
    ) -> AmmResult<MaverickQuote> {
        let direction = self
            .direction_for_tokens(token_in, token_out)
            .ok_or(AmmError::InvariantViolation)?;
        self.quote_exact_in_local(amount_in_native, direction, None)
    }

    /// Quote exact-input swap (with pre-resolved direction + optional tick limit).
    pub fn quote_exact_in_local(
        &self,
        amount_in_native: U256,
        direction: SwapDirection,
        tick_limit: Option<i32>,
    ) -> AmmResult<MaverickQuote> {
        let input_scale = match direction {
            SwapDirection::ZeroToOne => self.token0_scale,
            SwapDirection::OneToZero => self.token1_scale,
        };
        let output_scale = match direction {
            SwapDirection::ZeroToOne => self.token1_scale,
            SwapDirection::OneToZero => self.token0_scale,
        };
        let amount_in_d18 = input_scale.to_d18_floor(amount_in_native)?;
        let quote_d18 = self.quote_d18(amount_in_d18, direction, tick_limit)?;
        Ok(MaverickQuote {
            amount_in_native,
            amount_in_d18,
            amount_out_native: output_scale.from_d18_floor(quote_d18.amount_out_d18)?,
            amount_out_d18: quote_d18.amount_out_d18,
            total_fee_d18: quote_d18.total_fee_d18,
            protocol_fee_d18: quote_d18.protocol_fee_d18,
            lp_fee_d18: quote_d18.lp_fee_d18,
            final_tick: quote_d18.final_tick,
            final_sqrt_price_d18: quote_d18.final_sqrt_price_d18,
            ticks_crossed: quote_d18.ticks_crossed,
            partial_fill: quote_d18.partial_fill,
        })
    }

    fn fee_for_direction(&self, direction: SwapDirection) -> U256 {
        match direction {
            SwapDirection::ZeroToOne => self.fee_zto_d18,
            SwapDirection::OneToZero => self.fee_otz_d18,
        }
    }

    fn quote_d18(
        &self,
        amount: U256,
        direction: SwapDirection,
        tick_limit: Option<i32>,
    ) -> AmmResult<MaverickQuoteD18> {
        if amount.is_zero() {
            return Ok(MaverickQuoteD18::default());
        }
        if self.permissioned_swap {
            return Err(AmmError::InvariantViolation);
        }

        let mut remaining = amount;
        let mut total_in = U256::ZERO;
        let mut total_out = U256::ZERO;
        let mut total_fee = U256::ZERO;
        let mut total_protocol_fee = U256::ZERO;
        let mut total_lp_fee = U256::ZERO;
        let mut ticks_crossed = 0u32;
        let mut current_tick = self.active_tick;
        let mut current_sqrt = self.sqrt_price_d18;
        let limit = tick_limit.unwrap_or(direction.default_tick_limit());
        let mut idx = self
            .ticks
            .binary_search_by_key(&self.active_tick, |tick| tick.tick)
            .map_err(|_| AmmError::TickWindowExhausted)?;

        for _ in 0..MAX_HOPS_PER_LOCAL_QUOTE {
            if remaining.is_zero() {
                break;
            }
            let tick = self.ticks.get(idx).ok_or(AmmError::TickWindowExhausted)?;
            if tick.tick != current_tick {
                return Err(AmmError::TickWindowExhausted);
            }
            if tick.liquidity_d18.is_zero() {
                return Err(AmmError::ZeroLiquidity);
            }
            let step = swap_step_exact_in(
                current_sqrt,
                tick.liquidity_d18,
                remaining,
                direction.token_a_in(),
                self.fee_for_direction(direction),
                self.protocol_fee_ratio_d3,
                tick.sqrt_lower_price_d18,
                tick.sqrt_upper_price_d18,
            )?;

            total_in = total_in.saturating_add(step.amount_in_total_d18);
            total_out = total_out.saturating_add(step.amount_out_d18);
            total_fee = total_fee.saturating_add(step.total_fee_d18);
            total_protocol_fee = total_protocol_fee.saturating_add(step.protocol_fee_d18);
            total_lp_fee = total_lp_fee.saturating_add(step.lp_fee_d18);
            current_sqrt = step.new_sqrt_price_d18;

            remaining = remaining.saturating_sub(step.amount_in_total_d18);

            if !step.crossed_tick {
                break;
            }
            ticks_crossed = ticks_crossed.saturating_add(1);
            match direction {
                SwapDirection::ZeroToOne => {
                    if current_tick >= limit {
                        break;
                    }
                    idx = idx.checked_add(1).ok_or(AmmError::TickWindowExhausted)?;
                    let next = self.ticks.get(idx).ok_or(AmmError::TickWindowExhausted)?;
                    current_tick = next.tick;
                    current_sqrt = next.sqrt_lower_price_d18;
                }
                SwapDirection::OneToZero => {
                    if current_tick <= limit {
                        break;
                    }
                    idx = idx.checked_sub(1).ok_or(AmmError::TickWindowExhausted)?;
                    let next = self.ticks.get(idx).ok_or(AmmError::TickWindowExhausted)?;
                    current_tick = next.tick;
                    current_sqrt = next.sqrt_upper_price_d18;
                }
            }
        }

        Ok(MaverickQuoteD18 {
            amount_in_d18: total_in,
            amount_out_d18: total_out,
            total_fee_d18: total_fee,
            protocol_fee_d18: total_protocol_fee,
            lp_fee_d18: total_lp_fee,
            final_tick: current_tick,
            final_sqrt_price_d18: current_sqrt,
            ticks_crossed,
            partial_fill: !remaining.is_zero(),
        })
    }

    /// Apply an exact-input swap in place — updates `active_tick` and `sqrt_price`.
    /// Tick reserves remain stale until the next on-chain refresh.
    pub fn apply_exact_in_local(
        &mut self,
        amount_in_native: U256,
        direction: SwapDirection,
        block_number: u64,
        log_index: u32,
    ) -> AmmResult<MaverickQuote> {
        let quote = self.quote_exact_in_local(amount_in_native, direction, None)?;
        self.active_tick = quote.final_tick;
        self.sqrt_price_d18 = quote.final_sqrt_price_d18;
        self.block_number = block_number;
        self.log_index = log_index;
        Ok(quote)
    }

    /// Total output-side liquidity available in the cached tick window.
    pub fn output_liquidity_native(&self, direction: SwapDirection) -> AmmResult<U256> {
        let output_scale = match direction {
            SwapDirection::ZeroToOne => self.token1_scale,
            SwapDirection::OneToZero => self.token0_scale,
        };
        let total_d18 = self.ticks.iter().fold(U256::ZERO, |acc, tick| {
            let reserve = match direction {
                SwapDirection::ZeroToOne => tick.reserve1_d18,
                SwapDirection::OneToZero => tick.reserve0_d18,
            };
            acc.saturating_add(reserve)
        });
        output_scale.from_d18_floor(total_d18)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(800))]

        /// K2: Maverick V2 quote is monotonic in input over random small in-tick amounts and never
        /// panics (perturbs the known-good `sample_state` fixture by amount).
        #[test]
        fn quote_monotonic_in_amount(
            a in 1u64..=500_000_000u64,
            bump in 1u64..=500_000_000u64,
            zero_to_one in any::<bool>(),
        ) {
            let s = sample_state();
            let dir = if zero_to_one { SwapDirection::ZeroToOne } else { SwapDirection::OneToZero };
            let o1 = s.quote_exact_in_local(U256::from(a), dir, None);
            let o2 = s.quote_exact_in_local(U256::from(a + bump), dir, None);
            if let (Ok(q1), Ok(q2)) = (o1, o2) {
                prop_assert!(
                    q2.amount_out_native >= q1.amount_out_native,
                    "more in ⇒ not less out"
                );
            }
        }
    }

    fn scale(decimals: u8) -> TokenScale {
        if decimals <= 18 {
            TokenScale {
                raw: 10u64.pow((18 - decimals) as u32),
                factor: U256::from(10u64.pow((18 - decimals) as u32)),
                more_than_18: false,
            }
        } else {
            TokenScale {
                raw: (1u64 << 63) | 10u64.pow((decimals - 18) as u32),
                factor: U256::from(10u64.pow((decimals - 18) as u32)),
                more_than_18: true,
            }
        }
    }

    fn sample_state() -> MaverickState {
        MaverickState {
            address: Address::from([0x11; 20]),
            token0: Address::from([0x22; 20]),
            token1: Address::from([0x33; 20]),
            token0_scale: scale(6),
            token1_scale: scale(6),
            fee_zto_d18: U256::from(3_000_000_000_000_000u64),
            fee_otz_d18: U256::from(3_000_000_000_000_000u64),
            tick_spacing: 1,
            lookback: 60,
            kinds: 15,
            lending_fee_rate_d18: U256::ZERO,
            protocol_fee_ratio_d3: 100,
            permissioned_swap: false,
            sqrt_price_d18: U256::from(D18),
            active_tick: 0,
            reserve0_d18: U256::from(2_000_000u64) * U256::from(D18),
            reserve1_d18: U256::from(2_000_000u64) * U256::from(D18),
            last_timestamp: 0,
            block_number: 1,
            log_index: 0,
            tick_radius: 1,
            ticks: vec![
                MaverickTick {
                    tick: -1,
                    reserve0_d18: U256::from(1_000_000u64) * U256::from(D18),
                    reserve1_d18: U256::from(1_000_000u64) * U256::from(D18),
                    total_supply: U256::from(1_000_000u64),
                    liquidity_d18: U256::from(1_000_000u64) * U256::from(D18),
                    sqrt_lower_price_d18: U256::from(D18 / 2),
                    sqrt_upper_price_d18: U256::from(D18),
                },
                MaverickTick {
                    tick: 0,
                    reserve0_d18: U256::from(1_000_000u64) * U256::from(D18),
                    reserve1_d18: U256::from(1_000_000u64) * U256::from(D18),
                    total_supply: U256::from(1_000_000u64),
                    liquidity_d18: U256::from(1_000_000u64) * U256::from(D18),
                    sqrt_lower_price_d18: U256::from(D18),
                    sqrt_upper_price_d18: U256::from(2 * D18),
                },
            ],
        }
    }

    #[test]
    fn direction_resolution() {
        let s = sample_state();
        assert_eq!(
            s.direction_for_tokens(s.token0, s.token1),
            Some(SwapDirection::ZeroToOne)
        );
        assert_eq!(
            s.direction_for_tokens(s.token1, s.token0),
            Some(SwapDirection::OneToZero)
        );
    }

    #[test]
    fn live_reference_vector_readiness_is_false_until_minimum_vectors_exist() {
        assert_eq!(LIVE_REFERENCE_VECTOR_COUNT, 0);
        assert_eq!(MIN_LIVE_REFERENCE_VECTORS, 3);
        assert!(!live_reference_vectors_ready());
    }

    #[test]
    fn exact_input_monotonic() {
        let s = sample_state();
        let small = s
            .quote_exact_in_local(U256::from(100_000_000u64), SwapDirection::ZeroToOne, None)
            .unwrap();
        let large = s
            .quote_exact_in_local(U256::from(200_000_000u64), SwapDirection::ZeroToOne, None)
            .unwrap();
        assert!(large.amount_out_native >= small.amount_out_native);
    }

    #[test]
    fn apply_updates_tick_and_price() {
        let mut s = sample_state();
        let q = s
            .apply_exact_in_local(U256::from(100_000_000u64), SwapDirection::ZeroToOne, 2, 3)
            .unwrap();
        assert_eq!(s.active_tick, q.final_tick);
        assert_eq!(s.sqrt_price_d18, q.final_sqrt_price_d18);
        assert_eq!(s.block_number, 2);
        assert_eq!(s.log_index, 3);
    }
}
