//! DET-3.3 — ECONOMIC-GATE CORRECTNESS (fixed gas constants protect the DETECTION path).
//!
//! Proves the §6 gas fix changed the live profit gate's behavior in the right direction:
//! a marginal arb whose edge is BELOW the corrected gas cost is REJECTED under the new fixed
//! constants (3-hop submit-floor 960k gas units), but would have been ACCEPTED under the OLD
//! constant (350k). Everything else is held identical — only the L2 gas-units estimate changes —
//! so the flip is attributable to the gas fix alone.
//!
//! Ground truth for "960k is correct": MantisGasOracle.Fork measured the worst real on-chain 3-hop
//! DODO cycle at 933,253 gas (this session) — so a 350k budget under-states true cost by ~2.7x and
//! would wave a genuine gas-loser through.
//!
//! Run: cargo test -p engine-sim --test det33_economic_gate -- --nocapture

use alloy_primitives::U256;
use engine_sim::profit::{
    canonical_net_profit, ev_decision, EvDecision, EvDecomposition, ProfitTerms, SubmissionDecision,
};

// Arbitrum L2 gas price floor = 0.02 gwei (getPricesInWei[3] = 20_000_000 wei), proven live this session.
const L2_GAS_PRICE_WEI: u128 = 20_000_000;
// 3-hop gas-units estimates: OLD (pre-fix) vs NEW (submit-floor, covers measured worst-case 933,253).
const OLD_GAS_UNITS_3HOP: u128 = 350_000;
const NEW_GAS_UNITS_3HOP: u128 = 960_000;
// Representative L1 Brotli data fee for a thin 3-hop calldata (held constant across both regimes).
const L1_DATA_FEE_WEI: u128 = 3_000_000_000_000; // 3e12

fn terms_with_l2_gas(gross_wei: u128, gas_units: u128) -> ProfitTerms {
    ProfitTerms {
        gross_profit_wei: U256::from(gross_wei),
        l2_execution_cost_wei: U256::from(gas_units * L2_GAS_PRICE_WEI),
        l1_data_fee_wei: U256::from(L1_DATA_FEE_WEI),
        flash_loan_fee_wei: U256::ZERO,    // Balancer 0%
        executor_overhead_wei: U256::ZERO, // isolate the gas-units effect
        safety_buffer_wei: U256::ZERO,     // isolate the gas-units effect
    }
}

fn marginal_decision() -> SubmissionDecision {
    SubmissionDecision {
        // Low floor so the GAS-UNITS change (not the min-profit floor) is what flips the decision.
        min_required_net_profit_wei: U256::from(1_000_000_000_000u128), // 1e12
        min_ev_absolute_wei: 0,
        min_roi_bps: 0,
        gas_spike_ratio_q8: 256, // 1.0x (current .env)
        confidence_floor: 0.95,
        max_flashloan_fee_1e18: 0,
    }
}

fn ev_components(net_wei: u128) -> EvDecomposition {
    EvDecomposition {
        net_profit_wei: U256::from(net_wei),
        p_win_e6: 1_000_000, // certain landing — isolate the cost gate, not the EV model
        p_revert_e6: 0,
        p_stale_e6: 0,
        p_ops_e6: 0,
        revert_loss_wei: U256::ZERO,
        stale_loss_wei: U256::ZERO,
        operational_penalty_wei: U256::ZERO,
    }
}

#[test]
fn det33_fixed_gas_rejects_marginal_arb_that_old_gas_accepted() {
    // A marginal arb: gross edge sits BETWEEN the old-budgeted cost and the new (correct) cost.
    //   OLD total cost = 350k*0.02gwei + L1 = 7.0e12 + 3.0e12 = 1.0e13
    //   NEW total cost = 960k*0.02gwei + L1 = 1.92e13 + 3.0e12 = 2.22e13
    let gross_wei: u128 = 15_000_000_000_000; // 1.5e13 — above OLD cost, below NEW cost
    let decision = marginal_decision();

    let old_terms = terms_with_l2_gas(gross_wei, OLD_GAS_UNITS_3HOP);
    let new_terms = terms_with_l2_gas(gross_wei, NEW_GAS_UNITS_3HOP);

    let old_net = canonical_net_profit(old_terms);
    let new_net = canonical_net_profit(new_terms);

    println!("=== DET-3.3 ECONOMIC GATE (gas-units 350k -> 960k @3-hop) ===");
    println!(
        "L2 gas price            : {} wei (0.02 gwei)",
        L2_GAS_PRICE_WEI
    );
    println!("gross edge              : {} wei (1.5e13)", gross_wei);
    println!(
        "OLD 350k total cost     : {} wei  -> net = {:?}",
        350_000u128 * L2_GAS_PRICE_WEI + L1_DATA_FEE_WEI,
        old_net
    );
    println!(
        "NEW 960k total cost     : {} wei  -> net = {:?}",
        960_000u128 * L2_GAS_PRICE_WEI + L1_DATA_FEE_WEI,
        new_net
    );

    // canonical_net_profit: OLD accepts (net > 0), NEW rejects (net would be negative -> None).
    assert!(
        old_net.is_some(),
        "OLD 350k gas: marginal arb should be net-positive (accepted)"
    );
    assert!(old_net.unwrap() > U256::ZERO, "OLD net must be > 0");
    assert!(
        new_net.is_none(),
        "NEW 960k gas: marginal arb must be net-NEGATIVE (rejected at canonical_net_profit)"
    );

    // Full live gate (ev_decision): OLD -> Submit, NEW -> reject (NetProfitTooLow / GasSpikeTooThin).
    let amount_in = U256::from(16_000_000_000_000_000_000u128); // 16 ETH notional
    let old_decision = ev_decision(
        old_terms,
        ev_components(old_net.unwrap().to::<u128>()),
        decision,
        0.99,
        amount_in,
    );
    let new_decision = ev_decision(new_terms, ev_components(0), decision, 0.99, amount_in);

    println!("ev_decision OLD (350k)  : {:?}", old_decision);
    println!("ev_decision NEW (960k)  : {:?}", new_decision);

    assert_eq!(
        old_decision,
        EvDecision::Submit,
        "OLD 350k gas: live gate should SUBMIT this marginal arb (the dangerous pre-fix behavior)"
    );
    assert_ne!(
        new_decision,
        EvDecision::Submit,
        "NEW 960k gas: live gate must REJECT — the fix protects the detection path"
    );
    // Specifically a cost-driven rejection.
    assert!(
        matches!(
            new_decision,
            EvDecision::NetProfitTooLow | EvDecision::GasSpikeTooThin
        ),
        "NEW rejection must be cost-driven (NetProfitTooLow/GasSpikeTooThin), got {new_decision:?}"
    );
    println!("VERDICT: gas fix flips accept->reject on a gas-losing marginal arb. DET-3.3 PASS.");
}

#[test]
fn det33_healthy_arb_still_submits_under_new_gas() {
    // Control: a genuinely profitable arb (gross well above the NEW cost) is still ACCEPTED — the fix
    // rejects gas-losers, it does not break real money.
    let gross_wei: u128 = 80_000_000_000_000; // 8.0e13, comfortably above NEW 2.22e13 cost
    let decision = marginal_decision();
    let new_terms = terms_with_l2_gas(gross_wei, NEW_GAS_UNITS_3HOP);
    let net =
        canonical_net_profit(new_terms).expect("healthy arb must be net-positive under new gas");
    let d = ev_decision(
        new_terms,
        ev_components(net.to::<u128>()),
        decision,
        0.99,
        U256::from(16_000_000_000_000_000_000u128),
    );
    println!("healthy arb under NEW 960k gas: net={net} -> {d:?}");
    assert_eq!(
        d,
        EvDecision::Submit,
        "healthy arb must still submit under the fixed constants"
    );
}
