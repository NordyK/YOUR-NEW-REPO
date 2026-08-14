use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_sim::{
    canonical_net_profit, ev_decision, expected_value, EvDecision, EvDecomposition, ProfitTerms,
    SubmissionDecision,
};
use ruint::aliases::U256;

fn terms() -> ProfitTerms {
    ProfitTerms {
        gross_profit_wei: U256::from(1_000_000u64),
        l2_execution_cost_wei: U256::from(100_000u64),
        l1_data_fee_wei: U256::from(50_000u64),
        flash_loan_fee_wei: U256::ZERO,
        executor_overhead_wei: U256::from(30_000u64),
        safety_buffer_wei: U256::from(1u64),
    }
}

fn ev() -> EvDecomposition {
    EvDecomposition {
        net_profit_wei: U256::from(850_000u64),
        p_win_e6: 800_000,
        p_revert_e6: 100_000,
        p_stale_e6: 50_000,
        p_ops_e6: 1_000,
        revert_loss_wei: U256::from(200u64),
        stale_loss_wei: U256::from(50u64),
        operational_penalty_wei: U256::from(10u64),
    }
}

fn bench_canonical_net_profit(c: &mut Criterion) {
    let t = terms();
    c.bench_function("sim_canonical_net_profit", |b| {
        b.iter(|| canonical_net_profit(black_box(t)))
    });
}

fn bench_expected_value(c: &mut Criterion) {
    let d = ev();
    c.bench_function("sim_expected_value", |b| {
        b.iter(|| expected_value(black_box(d)))
    });
}

fn bench_submission_decision(c: &mut Criterion) {
    let t = terms();
    let d = ev();
    let dec = SubmissionDecision {
        min_required_net_profit_wei: U256::from(100_000u64),
        min_ev_absolute_wei: 10_000,
        min_roi_bps: 1,
        gas_spike_ratio_q8: 384,
        confidence_floor: 0.95,
        max_flashloan_fee_1e18: 0,
    };
    let amt = U256::from(1_000_000u64);
    c.bench_function("sim_submission_decision_submit_path", |b| {
        b.iter(|| {
            let r = ev_decision(
                black_box(t),
                black_box(d),
                black_box(dec),
                black_box(0.99),
                black_box(amt),
            );
            assert_eq!(r, EvDecision::Submit);
        })
    });
}

criterion_group!(
    benches,
    bench_canonical_net_profit,
    bench_expected_value,
    bench_submission_decision
);
criterion_main!(benches);
