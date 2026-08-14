//! KyberSwap Elastic swap math (Uniswap-V3-style concentrated liquidity with
//! fee reinvestment).
//!
//! Key differences vs Uniswap V3:
//! * Fees are reinvested as additional liquidity (`deltaL`) during the swap step,
//!   so the effective liquidity is `L + deltaL`. This shifts the output amount,
//!   the final sqrt price, and the tick-crossing boundary.
//! * Fee denominator is `100_000` (Kyber `FEE_UNITS`), not Uniswap's `1_000_000`
//!   ppm. The engine passes our standard PPM in and we convert.
//!
//! Ported from the Fulcrum codebase, adapted to use our `engine_amm` V3 helpers
//! (`sqrt_price_at_tick`, `TickInfo`).

use alloy_primitives::{U256, U512};

use crate::error::{AmmError, AmmResult};
use crate::uniswap_v3::{sqrt_price_at_tick, TickInfo};

/// Kyber fee denominator (100_000 = 100%).
pub const FEE_UNITS: u64 = 100_000;
const TWO_FEE_UNITS: u64 = 200_000;
/// 2^96 as U256.
const TWO_POW_96: U256 = U256::from_limbs([0, 0x1_0000_0000, 0, 0]);

#[inline(always)]
fn mul_div(a: U256, b: U256, d: U256) -> Option<U256> {
    if d.is_zero() {
        return None;
    }
    let q = U512::from(a) * U512::from(b) / U512::from(d);
    if q > U512::from(U256::MAX) {
        None
    } else {
        Some(U256::from_limbs(
            q.as_limbs()[..4].try_into().expect("U512 has 8 limbs"),
        ))
    }
}

#[inline(always)]
fn mul_div_up(a: U256, b: U256, d: U256) -> Option<U256> {
    if d.is_zero() {
        return None;
    }
    let prod = U512::from(a) * U512::from(b);
    let dw = U512::from(d);
    let q = prod / dw;
    let r = prod - (q * dw);
    let res = if r > U512::ZERO {
        q + U512::from(1u64)
    } else {
        q
    };
    if res > U512::from(U256::MAX) {
        None
    } else {
        Some(U256::from_limbs(
            res.as_limbs()[..4].try_into().expect("U512 has 8 limbs"),
        ))
    }
}

/// Adjust active liquidity when crossing an initialized tick.
/// `liquidity_net` is positive when the tick is the low boundary of a position,
/// negative when it's the high boundary. Direction-aware: subtract on
/// zero-for-one crossings, add otherwise.
#[inline]
fn adj_liquidity(liq: U256, signed: i128) -> AmmResult<U256> {
    if signed >= 0 {
        liq.checked_add(U256::from(signed as u128))
            .ok_or(AmmError::Overflow)
    } else {
        let abs = (-signed) as u128;
        liq.checked_sub(U256::from(abs)).ok_or(AmmError::Underflow)
    }
}

// ============================================================================
// Step math
// ============================================================================

fn calc_reach_amount(
    liquidity: U256,
    current_sqrt_p: U256,
    target_sqrt_p: U256,
    fee_in_fee_units: u64,
    is_token0: bool,
) -> Option<U256> {
    let abs_price_diff = if current_sqrt_p >= target_sqrt_p {
        current_sqrt_p - target_sqrt_p
    } else {
        target_sqrt_p - current_sqrt_p
    };
    if is_token0 {
        let denom = U256::from(TWO_FEE_UNITS)
            .checked_mul(target_sqrt_p)?
            .checked_sub(U256::from(fee_in_fee_units).checked_mul(current_sqrt_p)?)?;
        if denom.is_zero() {
            return None;
        }
        let numerator = mul_div(
            liquidity,
            U256::from(TWO_FEE_UNITS).checked_mul(abs_price_diff)?,
            denom,
        )?;
        mul_div(numerator, TWO_POW_96, current_sqrt_p)
    } else {
        let denom = U256::from(TWO_FEE_UNITS)
            .checked_mul(current_sqrt_p)?
            .checked_sub(U256::from(fee_in_fee_units).checked_mul(target_sqrt_p)?)?;
        if denom.is_zero() {
            return None;
        }
        let numerator = mul_div(
            liquidity,
            U256::from(TWO_FEE_UNITS).checked_mul(abs_price_diff)?,
            denom,
        )?;
        mul_div(numerator, current_sqrt_p, TWO_POW_96)
    }
}

fn estimate_incremental_liquidity(
    abs_delta: U256,
    _liquidity: U256,
    current_sqrt_p: U256,
    fee_in_fee_units: u64,
    is_token0: bool,
) -> Option<U256> {
    if is_token0 {
        let numerator = abs_delta.checked_mul(U256::from(fee_in_fee_units))?;
        let denominator = U256::from(TWO_FEE_UNITS).checked_mul(TWO_POW_96)?;
        mul_div(current_sqrt_p, numerator, denominator)
    } else {
        let numerator = abs_delta.checked_mul(U256::from(fee_in_fee_units))?;
        let denominator = U256::from(TWO_FEE_UNITS).checked_mul(current_sqrt_p)?;
        mul_div(TWO_POW_96, numerator, denominator)
    }
}

fn calc_incremental_liquidity(
    abs_delta: U256,
    liquidity: U256,
    current_sqrt_p: U256,
    next_sqrt_p: U256,
    is_token0: bool,
) -> Option<U256> {
    if is_token0 {
        let tmp1 = mul_div(liquidity, TWO_POW_96, current_sqrt_p)?;
        let tmp2 = tmp1.checked_add(abs_delta)?;
        let tmp3 = mul_div(next_sqrt_p, tmp2, TWO_POW_96)?;
        Some(if tmp3 > liquidity {
            tmp3 - liquidity
        } else {
            U256::ZERO
        })
    } else {
        let tmp1 = mul_div(liquidity, current_sqrt_p, TWO_POW_96)?;
        let tmp2 = tmp1.checked_add(abs_delta)?;
        let tmp3 = mul_div(tmp2, TWO_POW_96, next_sqrt_p)?;
        Some(if tmp3 > liquidity {
            tmp3 - liquidity
        } else {
            U256::ZERO
        })
    }
}

fn calc_final_price(
    abs_delta: U256,
    liquidity: U256,
    delta_l: U256,
    current_sqrt_p: U256,
    is_token0: bool,
) -> Option<U256> {
    if is_token0 {
        let tmp = mul_div(abs_delta, current_sqrt_p, TWO_POW_96)?;
        mul_div_up(
            liquidity.checked_add(delta_l)?,
            current_sqrt_p,
            liquidity.checked_add(tmp)?,
        )
    } else {
        let tmp = mul_div(abs_delta, TWO_POW_96, current_sqrt_p)?;
        mul_div(
            liquidity.checked_add(tmp)?,
            current_sqrt_p,
            liquidity.checked_add(delta_l)?,
        )
    }
}

fn calc_returned_amount(
    liquidity: U256,
    current_sqrt_p: U256,
    next_sqrt_p: U256,
    delta_l: U256,
    is_token0: bool,
) -> Option<U256> {
    if is_token0 {
        let delta_l_part = mul_div_up(delta_l, next_sqrt_p, TWO_POW_96)?;
        let liq_part = if current_sqrt_p >= next_sqrt_p {
            mul_div(liquidity, current_sqrt_p - next_sqrt_p, TWO_POW_96)?
        } else {
            return None;
        };
        Some(liq_part.saturating_sub(delta_l_part))
    } else {
        let term_a = mul_div_up(liquidity.checked_add(delta_l)?, TWO_POW_96, next_sqrt_p)?;
        let term_b = mul_div(liquidity, TWO_POW_96, current_sqrt_p)?;
        Some(term_b.saturating_sub(term_a))
    }
}

fn compute_swap_step(
    liquidity: U256,
    current_sqrt_p: U256,
    target_sqrt_p: U256,
    fee_in_fee_units: u64,
    amount_in: U256,
    is_token0: bool,
) -> Option<(U256, U256, U256, U256)> {
    if current_sqrt_p == target_sqrt_p {
        return Some((U256::ZERO, U256::ZERO, U256::ZERO, current_sqrt_p));
    }
    let reach_amount = calc_reach_amount(
        liquidity,
        current_sqrt_p,
        target_sqrt_p,
        fee_in_fee_units,
        is_token0,
    );

    let (used_amount, next_sqrt_p_known) = match reach_amount {
        Some(ra) if !ra.is_zero() && amount_in >= ra => (ra, Some(target_sqrt_p)),
        _ => (amount_in, None),
    };

    let (delta_l, next_sqrt_p) = if let Some(nsp) = next_sqrt_p_known {
        let dl =
            calc_incremental_liquidity(used_amount, liquidity, current_sqrt_p, nsp, is_token0)?;
        (dl, nsp)
    } else {
        let dl = estimate_incremental_liquidity(
            used_amount,
            liquidity,
            current_sqrt_p,
            fee_in_fee_units,
            is_token0,
        )?;
        let nsp = calc_final_price(used_amount, liquidity, dl, current_sqrt_p, is_token0)?;
        (dl, nsp)
    };

    let returned_amount =
        calc_returned_amount(liquidity, current_sqrt_p, next_sqrt_p, delta_l, is_token0)?;
    Some((used_amount, returned_amount, delta_l, next_sqrt_p))
}

// ============================================================================
// Public API
// ============================================================================

/// Multi-tick KyberSwap Elastic swap. Matches the V3 entry point signature so
/// the dispatcher can call this with the same shape as `v3_get_amount_out`.
///
/// `fee_ppm` is our standard parts-per-million (1_000_000 = 100%). Internally
/// converted to Kyber's 100_000-denominator fee units.
///
/// `ticks` must be sorted ascending by `tick`. Used to determine direction and
/// to look up `liquidity_net` for tick crossings.
///
/// A7 (verified-correct, locked): each `compute_swap_step` here also returns the intra-swap
/// `deltaL` (Kyber reinvests the swap fee as additional in-range liquidity mid-swap). We
/// deliberately DISCARD that `deltaL` (`_delta_l` at the two call sites below) and carry only the
/// base `liq` across tick crossings. Reinvested liquidity can only ADD depth, so ignoring it makes
/// the curve very slightly shallower than reality and the multi-tick quote a small, monotone
/// UNDER-quote — capital-safe (we never over-promise output). Single-tick swaps (no crossing) are
/// unaffected. Kyber is default-OFF and, when enabled, every candidate is REVM-validated against
/// the real pool before submit, so the residual is caught there too. Behavior is unchanged.
pub fn get_amount_out(
    amount_in: u128,
    sqrt_price_x96: U256,
    liquidity: U256,
    current_tick: i32,
    fee_ppm: u32,
    zero_for_one: bool,
    ticks: &[TickInfo],
) -> AmmResult<(u128, U256)> {
    if amount_in == 0 || liquidity.is_zero() {
        return Ok((0, sqrt_price_x96));
    }
    let fee_in_fee_units = (fee_ppm / 10) as u64;
    let mut amount_remaining = U256::from(amount_in);
    let mut sqrt_p = sqrt_price_x96;
    let mut liq = liquidity;
    let mut amount_out = U256::ZERO;
    let is_token0 = zero_for_one;
    let tick_len = ticks.len();

    // Cursor into the ticks slice — first tick strictly past current_tick in the
    // direction of swap.
    let mut cursor_idx: usize = if zero_for_one {
        ticks.partition_point(|t| t.tick < current_tick)
    } else {
        ticks.partition_point(|t| t.tick <= current_tick)
    };

    loop {
        if amount_remaining.is_zero() {
            break;
        }

        let next_tick_opt: Option<&TickInfo> = if zero_for_one {
            if cursor_idx > 0 {
                ticks.get(cursor_idx - 1)
            } else {
                None
            }
        } else if cursor_idx < tick_len {
            ticks.get(cursor_idx)
        } else {
            None
        };

        if let Some(next_tick) = next_tick_opt {
            let target_sqrt = sqrt_price_at_tick(next_tick.tick)?;
            let (used, returned, _delta_l, new_sqrt) = compute_swap_step(
                liq,
                sqrt_p,
                target_sqrt,
                fee_in_fee_units,
                amount_remaining,
                is_token0,
            )
            .ok_or(AmmError::Overflow)?;
            amount_out = amount_out.checked_add(returned).ok_or(AmmError::Overflow)?;
            amount_remaining = amount_remaining.saturating_sub(used);
            sqrt_p = new_sqrt;

            if new_sqrt == target_sqrt {
                // Crossed tick — adjust active liquidity (sign-flipped for zero-for-one).
                let signed = if zero_for_one {
                    -next_tick.liquidity_net
                } else {
                    next_tick.liquidity_net
                };
                liq = adj_liquidity(liq, signed)?;
                if zero_for_one {
                    cursor_idx -= 1;
                } else {
                    cursor_idx += 1;
                }
                if liq.is_zero() {
                    break;
                }
            } else {
                break;
            }
        } else {
            // No more initialized ticks in the warmed window. Extrapolating to MIN/MAX_TICK at
            // the CURRENT liquidity assumes constant liquidity out to the price extreme — a
            // phantom-large-size / low-slippage OVER-QUOTE (the real pool almost certainly has
            // more initialized ticks the window did not load). Fail closed exactly like
            // `uniswap_v3::get_amount_out` (`TickWindowExhausted`) so the routing layer demotes
            // the route instead of trusting an over-quote. A swap fully consumed *before* this
            // branch (`amount_remaining == 0`) already returned above via the `break` paths.
            return Err(AmmError::TickWindowExhausted);
        }
    }

    let final_amount_out: u128 = amount_out.try_into().map_err(|_| AmmError::Overflow)?;
    Ok((final_amount_out, sqrt_p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sqrt_price() -> U256 {
        sqrt_price_at_tick(-201_280).unwrap()
    }

    #[test]
    fn zero_amount_returns_zero() {
        let ticks = vec![TickInfo {
            tick: -201_290,
            liquidity_net: 1_000_000_000_000,
        }];
        let r = get_amount_out(
            0,
            test_sqrt_price(),
            U256::from(26_000_000_000_000u128),
            -201_280,
            3_000,
            true,
            &ticks,
        )
        .unwrap();
        assert_eq!(r.0, 0);
    }

    #[test]
    fn zero_liquidity_returns_zero() {
        let ticks = vec![TickInfo {
            tick: -201_290,
            liquidity_net: 1,
        }];
        let r = get_amount_out(
            1_000_000_000,
            test_sqrt_price(),
            U256::ZERO,
            -201_280,
            3_000,
            true,
            &ticks,
        )
        .unwrap();
        assert_eq!(r.0, 0);
    }

    #[test]
    fn small_swap_produces_output() {
        // Multiple ticks below current — small swap won't reach them.
        let ticks: Vec<TickInfo> = (1..=5)
            .map(|i| TickInfo {
                tick: -201_280 - i * 10,
                liquidity_net: 1_000_000_000_000,
            })
            .collect();
        let r = get_amount_out(
            10u128.pow(10),
            test_sqrt_price(),
            U256::from(26_000_000_000_000u128),
            -201_280,
            3_000,
            true,
            &ticks,
        )
        .unwrap();
        assert!(r.0 > 0, "expected positive output, got {}", r.0);
        assert!(
            r.1 < test_sqrt_price(),
            "expected price to decrease for token0-in"
        );
    }
}

/// K2: randomized property tests for the KyberSwap Elastic (V3-family) tick-walk kernel.
/// No panic on any input; output monotonic in amount; sqrt price moves the correct direction.
#[cfg(test)]
mod kyber_property {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1500))]

        #[test]
        fn monotonic_and_price_direction(
            fee_ppm in prop::sample::select(vec![80u32, 300, 1000, 3000, 10000]),
            liq in 1_000_000_000_000u128..=1_000_000_000_000_000_000_000u128,
            amount in 1u128..=1_000_000_000_000_000u128,
            bump in 1u128..=1_000_000_000_000_000u128,
            zero_for_one in any::<bool>(),
        ) {
            let sp = sqrt_price_at_tick(0).unwrap();
            let liq_u = U256::from(liq);
            let ticks = vec![
                TickInfo { tick: -200_000, liquidity_net: 0 },
                TickInfo { tick: 200_000, liquidity_net: 0 },
            ];
            let o1 = get_amount_out(amount, sp, liq_u, 0, fee_ppm, zero_for_one, &ticks);
            let o2 = get_amount_out(amount.saturating_add(bump), sp, liq_u, 0, fee_ppm, zero_for_one, &ticks);
            if let (Ok((a, sa)), Ok((b, _))) = (o1, o2) {
                prop_assert!(b >= a, "more input ⇒ not less output");
                if a > 0 {
                    if zero_for_one {
                        prop_assert!(sa <= sp, "token0-in must not raise sqrt price");
                    } else {
                        prop_assert!(sa >= sp, "token1-in must not lower sqrt price");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tick_exhaustion_tests {
    //! AUD-KYBER-EXHAUST: exhausting the warmed tick window must FAIL CLOSED
    //! (`TickWindowExhausted`), mirroring `uniswap_v3::get_amount_out`. The old behavior
    //! extrapolated to MIN/MAX_TICK at constant liquidity — a phantom-size/low-slippage
    //! OVER-QUOTE for any amount larger than the warmed window can serve.
    use super::*;

    #[test]
    fn kyber_quote_fails_closed_on_tick_exhaustion() {
        // One warmed tick just below current; a huge zero-for-one swap needs liquidity far
        // beyond it. Old code: partial step to MIN_TICK at constant liq → Ok(over-quote).
        // Required: Err(TickWindowExhausted).
        let ticks = vec![TickInfo {
            tick: -201_290,
            liquidity_net: 1_000_000_000_000,
        }];
        let r = get_amount_out(
            u128::MAX / 2, // absurdly large — cannot be served inside the window
            sqrt_price_at_tick(-201_280).unwrap(),
            U256::from(26_000_000_000_000u128),
            -201_280,
            3_000,
            true,
            &ticks,
        );
        assert!(
            matches!(r, Err(AmmError::TickWindowExhausted)),
            "exhausted tick window must fail closed, got {r:?}"
        );
    }

    #[test]
    fn kyber_quote_inside_window_still_succeeds() {
        // Regression guard (same params as `small_swap_produces_output`): a swap fully served
        // inside the warmed window keeps working unchanged after the fail-closed fix.
        let ticks: Vec<TickInfo> = (1..=5)
            .map(|i| TickInfo {
                tick: -201_280 - i * 10,
                liquidity_net: 1_000_000_000_000,
            })
            .collect();
        let r = get_amount_out(
            10u128.pow(10),
            sqrt_price_at_tick(-201_280).unwrap(),
            U256::from(26_000_000_000_000u128),
            -201_280,
            3_000,
            true,
            &ticks,
        );
        assert!(r.is_ok(), "in-window quote must still succeed: {r:?}");
        assert!(r.unwrap().0 > 0);
    }
}
