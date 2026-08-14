//! Canonical §12 profit math.

use alloy_primitives::U256;

/// Cost terms the engine subtracts from gross profit (§12.7).
#[derive(Debug, Clone, Copy)]
pub struct ProfitTerms {
    /// Gross profit in base-token wei units (already accounts for AMM fees per hop).
    pub gross_profit_wei: U256,
    /// L2 execution cost.
    pub l2_execution_cost_wei: U256,
    /// L1 data fee.
    pub l1_data_fee_wei: U256,
    /// Balancer flash loan fee.
    pub flash_loan_fee_wei: U256,
    /// Executor overhead constant per route kind.
    pub executor_overhead_wei: U256,
    /// Safety buffer (per-route bps + absolute floor).
    pub safety_buffer_wei: U256,
}

/// Canonical net profit (§12.7). Returns `None` if the result would be negative.
pub fn canonical_net_profit(t: ProfitTerms) -> Option<U256> {
    // gross - sum(costs)
    let total_costs = t
        .l2_execution_cost_wei
        .checked_add(t.l1_data_fee_wei)?
        .checked_add(t.flash_loan_fee_wei)?
        .checked_add(t.executor_overhead_wei)?
        .checked_add(t.safety_buffer_wei)?;
    if t.gross_profit_wei <= total_costs {
        None
    } else {
        Some(t.gross_profit_wei - total_costs)
    }
}

/// EV decomposition per §12.8.
#[derive(Debug, Clone, Copy)]
pub struct EvDecomposition {
    /// Net profit on success (computed by `canonical_net_profit`).
    pub net_profit_wei: U256,
    /// Probability the tx lands before the edge disappears, scaled by 1e6.
    pub p_win_e6: u32,
    /// Probability of revert, scaled by 1e6.
    pub p_revert_e6: u32,
    /// Stale-state probability, scaled by 1e6.
    pub p_stale_e6: u32,
    /// Operational risk probability, scaled by 1e6.
    pub p_ops_e6: u32,
    /// Loss on revert (gas burned).
    pub revert_loss_wei: U256,
    /// Loss when state is stale at execution.
    pub stale_loss_wei: U256,
    /// Operational risk penalty (constant).
    pub operational_penalty_wei: U256,
}

/// Compute expected value (signed) per §12.8.
pub fn expected_value(d: EvDecomposition) -> i128 {
    // Use saturating math throughout; EV is bounded by route's max_input * mark price.
    // Clamp every u128 magnitude to i128::MAX before signing: `u256_mul_e6` saturates to
    // u128::MAX, and a bare `as i128` would wrap any value ≥ 2^127 NEGATIVE — on a cost
    // term (`- revert`/`- stale`/`- ops`) a negative operand would ADD to EV (phantom).
    let win = clamp_u128_to_i128(u256_mul_e6(d.net_profit_wei, d.p_win_e6));
    let revert = clamp_u128_to_i128(u256_mul_e6(d.revert_loss_wei, d.p_revert_e6));
    let stale = clamp_u128_to_i128(u256_mul_e6(d.stale_loss_wei, d.p_stale_e6));
    let ops = clamp_u128_to_i128(u256_mul_e6(d.operational_penalty_wei, d.p_ops_e6));
    win.saturating_sub(revert)
        .saturating_sub(stale)
        .saturating_sub(ops)
}

/// Convert a u128 magnitude to a non-negative i128, clamping at `i128::MAX`.
/// Guarantees a term can never wrap negative (which would invert a cost into a gain).
fn clamp_u128_to_i128(v: u128) -> i128 {
    i128::try_from(v).unwrap_or(i128::MAX)
}

fn u256_mul_e6(x: U256, p_e6: u32) -> u128 {
    // x * p_e6 / 1_000_000; saturate to u128.
    let scale = U256::from(p_e6);
    let prod = x.saturating_mul(scale);
    let denom = U256::from(1_000_000u64);
    let q = prod / denom;
    if q > U256::from(u128::MAX) {
        u128::MAX
    } else {
        q.as_limbs()[0] as u128 | ((q.as_limbs()[1] as u128) << 64)
    }
}

/// Submission thresholds.
#[derive(Debug, Clone, Copy)]
pub struct SubmissionDecision {
    /// Minimum net profit, per route.
    pub min_required_net_profit_wei: U256,
    /// Minimum EV.
    pub min_ev_absolute_wei: i128,
    /// Minimum ROI in basis points (bps).
    pub min_roi_bps: u32,
    /// Gas-spike absorption ratio (default 1.5x).
    pub gas_spike_ratio_q8: u32,
    /// Confidence floor (engine-detect uses 0.95 baseline).
    pub confidence_floor: f32,
    /// Maximum flash-loan fee as an 18-decimal fraction of the borrowed amount (1e18 = 100%).
    /// A route whose flash-loan fee exceeds this fraction of its input is rejected
    /// (`FlashFeeTooHigh`). Default 1e16 = 1%. `0` disables the cap.
    pub max_flashloan_fee_1e18: u128,
}

/// EV decision, per §12.9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvDecision {
    /// Submit the candidate.
    Submit,
    /// Reject because net profit is too low.
    NetProfitTooLow,
    /// Reject because EV is too low.
    EvTooLow,
    /// Reject because ROI is too low.
    RoiTooLow,
    /// Reject because gross profit does not cover 1.5× gas + L1 costs.
    GasSpikeTooThin,
    /// Reject because confidence floor not met.
    ConfidenceTooLow,
    /// Reject because the flash-loan fee exceeds `max_flashloan_fee_1e18` of the input.
    FlashFeeTooHigh,
}

/// Apply the submission decision logic per §12.9.
pub fn ev_decision(
    terms: ProfitTerms,
    ev_components: EvDecomposition,
    decision: SubmissionDecision,
    confidence: f32,
    amount_in: U256,
) -> EvDecision {
    if confidence < decision.confidence_floor {
        return EvDecision::ConfidenceTooLow;
    }
    // Total zero-input guard: the flash-fee cap and the ROI floor below are both gated on
    // `!amount_in.is_zero()`, so a zero-input candidate would bypass both and could reach
    // `Submit` (fail-open). A route that trades nothing has no real profit; reject it here
    // so the gate set is total regardless of the sizer.
    if amount_in.is_zero() {
        return EvDecision::RoiTooLow;
    }
    // Flash-loan fee cap: reject if the borrow fee is too large a fraction of the input
    // (independent safety bound; `0` disables). flash_fraction_1e18 = fee * 1e18 / amount_in.
    if decision.max_flashloan_fee_1e18 > 0
        && !amount_in.is_zero()
        && !terms.flash_loan_fee_wei.is_zero()
    {
        let frac = terms
            .flash_loan_fee_wei
            .saturating_mul(U256::from(1_000_000_000_000_000_000u128))
            / amount_in;
        if frac > U256::from(decision.max_flashloan_fee_1e18) {
            return EvDecision::FlashFeeTooHigh;
        }
    }
    let Some(net) = canonical_net_profit(terms) else {
        return EvDecision::NetProfitTooLow;
    };
    if net < decision.min_required_net_profit_wei {
        return EvDecision::NetProfitTooLow;
    }
    let ev = expected_value(ev_components);
    if ev < decision.min_ev_absolute_wei {
        return EvDecision::EvTooLow;
    }
    // ROI: ev / amount_in in bps.
    if !amount_in.is_zero() {
        let ev_u256 = if ev > 0 {
            U256::from(ev as u128)
        } else {
            U256::ZERO
        };
        let roi_bps_num = ev_u256.saturating_mul(U256::from(10_000u64));
        let roi_bps = roi_bps_num / amount_in;
        if roi_bps < U256::from(decision.min_roi_bps as u64) {
            return EvDecision::RoiTooLow;
        }
    }
    // Gas-spike absorption.
    let l2_plus_l1 = terms
        .l2_execution_cost_wei
        .saturating_add(terms.l1_data_fee_wei);
    let needed = l2_plus_l1.saturating_mul(U256::from(decision.gas_spike_ratio_q8));
    // gas_spike_ratio_q8 is fixed-point q8 (1.5 = 384). Round the rejection bar UP (ceiling
    // division: (x + 255) / 256) so a candidate can't sit up to 255 wei under the true bar and
    // still be admitted — the conservative direction for a cost threshold.
    let needed = needed.saturating_add(U256::from(255u64)) / U256::from(256u64);
    if terms.gross_profit_wei < needed {
        return EvDecision::GasSpikeTooThin;
    }
    EvDecision::Submit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(gross: u64, l2: u64, l1: u64) -> ProfitTerms {
        ProfitTerms {
            gross_profit_wei: U256::from(gross),
            l2_execution_cost_wei: U256::from(l2),
            l1_data_fee_wei: U256::from(l1),
            flash_loan_fee_wei: U256::ZERO,
            executor_overhead_wei: U256::ZERO,
            safety_buffer_wei: U256::ZERO,
        }
    }

    #[test]
    fn net_profit_positive() {
        let t = terms(1000, 100, 50);
        let n = canonical_net_profit(t).unwrap();
        assert_eq!(n, U256::from(850u64));
    }

    #[test]
    fn net_profit_negative_returns_none() {
        let t = terms(100, 1000, 0);
        assert!(canonical_net_profit(t).is_none());
    }

    #[test]
    fn ev_basic() {
        let d = EvDecomposition {
            net_profit_wei: U256::from(1_000u64),
            p_win_e6: 800_000,
            p_revert_e6: 100_000,
            p_stale_e6: 50_000,
            p_ops_e6: 1_000,
            revert_loss_wei: U256::from(200u64),
            stale_loss_wei: U256::from(50u64),
            operational_penalty_wei: U256::from(10u64),
        };
        let ev = expected_value(d);
        // 0.8 * 1000 - 0.1 * 200 - 0.05 * 50 - 0.001 * 10 = 800 - 20 - 2.5 - 0.01 ≈ 777
        // Integer truncation: 0.8*1000 = 800, 0.1*200 = 20, 0.05*50 = 2, 0.001*10 = 0
        assert!(ev > 770 && ev < 800, "ev = {ev}");
    }

    #[test]
    fn ev_huge_cost_term_cannot_wrap_positive() {
        // D1 repro: a near-u128::MAX cost term must NOT add to EV. Before the clamp fix
        // `u256_mul_e6` saturates to u128::MAX; `as i128` wraps that NEGATIVE, and a
        // negative operand on a `- revert` term ADDS to EV (phantom profit).
        // revert_loss huge, p_revert_e6 = 1e6 (certainty) => the cost should DECREASE EV.
        let d = EvDecomposition {
            net_profit_wei: U256::ZERO,
            p_win_e6: 0,
            p_revert_e6: 1_000_000, // 100%
            p_stale_e6: 0,
            p_ops_e6: 0,
            revert_loss_wei: U256::from(u128::MAX), // multiplied → saturates to u128::MAX
            stale_loss_wei: U256::ZERO,
            operational_penalty_wei: U256::ZERO,
        };
        let ev = expected_value(d);
        // A guaranteed huge revert loss with zero upside must be a large NEGATIVE EV,
        // never positive. (Pre-fix this was +i128::MIN-ish flipped positive.)
        assert!(
            ev <= 0,
            "huge cost term must not make EV positive: ev = {ev}"
        );
    }

    #[test]
    fn ev_zero_amount_in_is_rejected() {
        // D2 repro: with amount_in == 0 the flash-fee cap and ROI floor were both skipped,
        // so a positive-net candidate could reach Submit (fail-open). The hoisted total
        // guard must reject any zero-input candidate regardless of the sizer.
        let t = terms(1_000_000, 100_000, 50_000);
        let d = EvDecomposition {
            net_profit_wei: U256::from(850_000u64),
            p_win_e6: 900_000,
            p_revert_e6: 50_000,
            p_stale_e6: 5_000,
            p_ops_e6: 0,
            revert_loss_wei: U256::from(150_000u64),
            stale_loss_wei: U256::from(100_000u64),
            operational_penalty_wei: U256::ZERO,
        };
        let dec = SubmissionDecision {
            min_required_net_profit_wei: U256::from(100_000u64),
            min_ev_absolute_wei: 10_000,
            min_roi_bps: 1,
            gas_spike_ratio_q8: 384,
            confidence_floor: 0.95,
            max_flashloan_fee_1e18: 0,
        };
        let res = ev_decision(t, d, dec, 0.99, U256::ZERO);
        assert_ne!(res, EvDecision::Submit, "zero amount_in must never Submit");
    }

    #[test]
    fn gas_spike_threshold_rounds_up_rejects_one_wei_under() {
        // The gas-spike bar is `ceil((l2+l1) * ratio_q8 / 256)`. With l2=100, l1=55,
        // ratio_q8=384 (1.5x): (155 * 384) / 256 = 59520 / 256 = 232.5 → ceil = 233.
        // A candidate whose gross_profit is exactly 1 wei under the ceil bar (232) must be
        // REJECTED as GasSpikeTooThin. Truncating-down division would set the bar at 232 and
        // wrongly admit it (a candidate up to 255 wei under the true bar slips through).
        let t = terms(232, 100, 55); // net = 232 - 155 = 77
        let d = EvDecomposition {
            net_profit_wei: U256::from(77u64),
            p_win_e6: 1_000_000, // certainty so EV/ROI are not the binding constraint
            p_revert_e6: 0,
            p_stale_e6: 0,
            p_ops_e6: 0,
            revert_loss_wei: U256::ZERO,
            stale_loss_wei: U256::ZERO,
            operational_penalty_wei: U256::ZERO,
        };
        let dec = SubmissionDecision {
            min_required_net_profit_wei: U256::from(1u64),
            min_ev_absolute_wei: 1,
            min_roi_bps: 1,
            gas_spike_ratio_q8: 384, // 1.5x in q8
            confidence_floor: 0.95,
            max_flashloan_fee_1e18: 0,
        };
        let res = ev_decision(t, d, dec, 0.99, U256::from(232u64));
        assert_eq!(
            res,
            EvDecision::GasSpikeTooThin,
            "gross 1 wei under the ceil gas-spike bar must be rejected"
        );

        // Boundary: a candidate AT the ceil bar (233) must pass the gas-spike gate.
        let t_ok = terms(233, 100, 55);
        let d_ok = EvDecomposition {
            net_profit_wei: U256::from(78u64),
            ..d
        };
        let res_ok = ev_decision(t_ok, d_ok, dec, 0.99, U256::from(233u64));
        assert_eq!(
            res_ok,
            EvDecision::Submit,
            "gross at the ceil bar must pass"
        );
    }

    #[test]
    fn submission_passes() {
        let t = terms(1_000_000, 100_000, 50_000);
        let d = EvDecomposition {
            net_profit_wei: U256::from(850_000u64),
            p_win_e6: 900_000,
            p_revert_e6: 50_000,
            p_stale_e6: 5_000,
            p_ops_e6: 0,
            revert_loss_wei: U256::from(150_000u64),
            stale_loss_wei: U256::from(100_000u64),
            operational_penalty_wei: U256::ZERO,
        };
        let dec = SubmissionDecision {
            min_required_net_profit_wei: U256::from(100_000u64),
            min_ev_absolute_wei: 10_000,
            min_roi_bps: 1,
            gas_spike_ratio_q8: 384, // 1.5x in q8
            confidence_floor: 0.95,
            max_flashloan_fee_1e18: 0, // cap disabled in this unit
        };
        let res = ev_decision(t, d, dec, 0.99, U256::from(1_000_000u64));
        assert_eq!(res, EvDecision::Submit);
    }
}
