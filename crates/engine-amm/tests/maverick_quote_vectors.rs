//! Maverick V2 quote-vector validation harness.
//!
//! Purpose: gate `MAVERICK_ENABLE=1`. The Maverick lens ABI was fixed and the kernel exists, but
//! before the engine routes real size through a Maverick pool we must prove the quote kernel
//! reproduces real on-chain swap outputs. This harness does two things:
//!
//!   1. **Kernel-invariant checks** (run now, offline, no fixture needed): determinism,
//!      monotonicity in amountIn, output bounded by reserves, and round-trip-loses-to-fees.
//!      These validate the kernel's *internal* consistency — a kernel that fails these is wrong
//!      regardless of any reference data.
//!
//!   2. **Reference-vector parity** (the `MAVERICK_ENABLE` gate): the `KNOWN_LIVE_VECTORS` table
//!      holds `(direction, amount_in, expected_amount_out)` triples captured from a LIVE Maverick
//!      pool (via the pool's on-chain quoter / a mined swap). When populated, the harness asserts
//!      the kernel reproduces each within a tight tolerance. The table is empty today — populating
//!      it with ≥3 real vectors that pass is the precondition documented in `startup.rs` for
//!      flipping `MAVERICK_ENABLE=1`.
//!
//! ## Capturing real vectors (operator runbook)
//! For a chosen live Maverick V2 pool at a pinned block:
//!   * snapshot its state via `engine_rpc::fetch_maverick_lens` + `fetch_maverick_state_basic`,
//!   * for ≥3 input amounts, read the true output from the Maverick quoter
//!     (`MaverickV2Quoter.calculateSwap`) at the same block,
//!   * paste the pool params into `live_pool_state()` and the triples into `KNOWN_LIVE_VECTORS`.
//! The parity test then proves the offline kernel == on-chain quoter before any live trade.
#![allow(clippy::doc_lazy_continuation)] // cosmetic doc-list-indentation nit in the runbook above

use alloy_primitives::{Address, U256};
use engine_amm::maverick_v2::{MaverickState, MaverickTick, SwapDirection, TokenScale};

const D18: u128 = 1_000_000_000_000_000_000;

fn scale6() -> TokenScale {
    // raw = 10^(18-6) = 1e12 → a 6-decimal token's 1e18-domain factor.
    TokenScale::from_pool_scale(U256::from(1_000_000_000_000u64)).expect("scale6")
}

/// A balanced two-tick Maverick pool, structurally valid for the kernel. Used for the
/// invariant checks (which don't need real reference data — they test internal consistency).
fn balanced_pool() -> MaverickState {
    MaverickState {
        address: Address::from([0x11; 20]),
        token0: Address::from([0x22; 20]),
        token1: Address::from([0x33; 20]),
        token0_scale: scale6(),
        token1_scale: scale6(),
        fee_zto_d18: U256::from(3_000_000_000_000_000u64), // 0.3%
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
fn kernel_quote_is_deterministic() {
    let s = balanced_pool();
    let amt = U256::from(1_000_000u64); // 1.0 token (6 dp)
    let q1 = s
        .quote_for_tokens_exact_in(s.token0, s.token1, amt)
        .expect("quote 1");
    let q2 = s
        .quote_for_tokens_exact_in(s.token0, s.token1, amt)
        .expect("quote 2");
    assert_eq!(
        q1.amount_out_native, q2.amount_out_native,
        "kernel must be deterministic"
    );
}

#[test]
fn kernel_quote_is_monotonic_in_amount_in() {
    // Larger input → at least as large output. A non-monotone AMM quote is a kernel bug.
    let s = balanced_pool();
    let mut prev = U256::ZERO;
    for mult in [1u64, 10, 100, 1_000] {
        let amt = U256::from(100_000u64 * mult);
        let q = s
            .quote_for_tokens_exact_in(s.token0, s.token1, amt)
            .expect("quote");
        assert!(
            q.amount_out_native >= prev,
            "amountOut must be monotone in amountIn: {} < {} at mult {mult}",
            q.amount_out_native,
            prev
        );
        prev = q.amount_out_native;
    }
}

#[test]
fn kernel_quote_stays_within_output_reserve() {
    // The pool cannot pay out more token1 than it holds.
    let s = balanced_pool();
    let total_out_reserve = s.reserve1_d18; // 1e18-domain
    let amt = U256::from(500_000u64); // 0.5 token
    let q = s
        .quote_for_tokens_exact_in(s.token0, s.token1, amt)
        .expect("quote");
    assert!(
        q.amount_out_d18 < total_out_reserve,
        "amountOut {} must stay below output reserve {}",
        q.amount_out_d18,
        total_out_reserve
    );
    assert!(
        q.amount_out_native > U256::ZERO,
        "a non-trivial input must yield positive output"
    );
}

#[test]
fn kernel_round_trip_loses_to_fees() {
    // Swap token0→token1 then token1→token0; with a 0.3% fee each way the round-trip MUST
    // return less than the original input. A round-trip that gains is a free-money kernel bug.
    let s = balanced_pool();
    let amt_in = U256::from(1_000_000u64); // 1.0 token0
    let fwd = s
        .quote_for_tokens_exact_in(s.token0, s.token1, amt_in)
        .expect("fwd");
    assert!(fwd.amount_out_native > U256::ZERO);
    let back = s
        .quote_for_tokens_exact_in(s.token1, s.token0, fwd.amount_out_native)
        .expect("back");
    assert!(
        back.amount_out_native < amt_in,
        "round-trip must lose to fees: got {} back from {} in",
        back.amount_out_native,
        amt_in
    );
}

// ─── MAVERICK_ENABLE gate: real on-chain reference vectors ──────────────────────────────────
//
// Each entry: (is_zero_to_one, amount_in_native, expected_amount_out_native) captured from a
// live Maverick pool's quoter at a pinned block. EMPTY today — populate with ≥3 real vectors
// (see the operator runbook at the top of this file) and they must pass before MAVERICK_ENABLE=1.
const KNOWN_LIVE_VECTORS: &[(bool, u128, u128)] = &[];

/// Reconstruct the exact live pool state the `KNOWN_LIVE_VECTORS` were captured against. Returns
/// `None` while the table is empty (no captured pool yet). When populating the vectors, replace
/// this with the snapshotted live pool params.
fn live_pool_state() -> Option<MaverickState> {
    if KNOWN_LIVE_VECTORS.is_empty() {
        return None;
    }
    // TODO(operator): paste the live pool's snapshotted MaverickState here alongside the vectors.
    Some(balanced_pool())
}

#[test]
fn live_reference_vectors_match_kernel_or_are_pending() {
    assert_eq!(
        KNOWN_LIVE_VECTORS.len(),
        engine_amm::maverick_v2::LIVE_REFERENCE_VECTOR_COUNT,
        "runtime Maverick readiness count must match the reviewed live-vector table"
    );
    let Some(state) = live_pool_state() else {
        // Gate not yet armed — the table is empty. This is the explicit, visible reminder that
        // MAVERICK_ENABLE must NOT be flipped until ≥3 real vectors are captured + pass here.
        assert!(
            !engine_amm::maverick_v2::live_reference_vectors_ready(),
            "runtime Maverick readiness must stay false while the live-vector table is empty"
        );
        eprintln!(
            "MAVERICK_ENABLE gate: KNOWN_LIVE_VECTORS is empty — capture ≥3 live Maverick quotes \
             before enabling Maverick discovery (see runbook in this file's header)."
        );
        return;
    };
    assert!(
        KNOWN_LIVE_VECTORS.len() >= 3,
        "the MAVERICK_ENABLE gate requires at least 3 reference vectors, got {}",
        KNOWN_LIVE_VECTORS.len()
    );
    for &(zto, amount_in, expected_out) in KNOWN_LIVE_VECTORS {
        let (tin, tout) = if zto {
            (state.token0, state.token1)
        } else {
            (state.token1, state.token0)
        };
        let q = state
            .quote_for_tokens_exact_in(tin, tout, U256::from(amount_in))
            .expect("kernel must quote a captured live swap");
        let got: u128 = q.amount_out_native.try_into().expect("output fits u128");
        // Tolerance: 1e-6 relative (the kernel is integer-exact; allow tiny rounding vs quoter).
        let tol = (expected_out / 1_000_000).max(1);
        let diff = got.abs_diff(expected_out);
        assert!(
            diff <= tol,
            "kernel quote diverged from live vector (zto={zto}, in={amount_in}): got {got}, \
             expected {expected_out}, diff {diff} > tol {tol}"
        );
    }
    let _ = SwapDirection::ZeroToOne; // keep the import meaningful for future direction-typed vectors
}
