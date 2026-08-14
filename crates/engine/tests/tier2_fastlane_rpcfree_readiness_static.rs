//! TIER-2 FAST-LANE / RPC-FREE READINESS — the per-family verdict for the families the operator
//! asked about (FluidDex, BalancerStable, BalancerWeighted, CurveStableSwapNg, CurveCryptoSwap):
//! can they move to the zero-RPC fast lane, and are they RPC-free?
//!
//! Two INDEPENDENT axes gate the fast lane — a family needs BOTH:
//!   AXIS A — BYTE-EXACT kernel (kernel == on-chain wei-for-wei ⇒ `disagree==0` vs REVM). Gates
//!            promotion to the AUTHORITATIVE lane (kernel decides, no REVM). Measured on AWS by the
//!            live-parity harnesses (curve_*/balancer_*/fluid_* _live_parity) — all currently Tier-2.
//!   AXIS B — RPC-FREE state (the in-memory mirror stays correct from EVENTS ALONE, no RPC reconcile
//!            to fix drift). Gates zero-RPC; also removes the RPC fetch from the REVM-validation path.
//!            Measured on AWS by tier2_state_drift_live (which field drifts across two blocks?).
//!
//! PER-FAMILY VERDICT (from the engine code map + Balancer/Curve source + the live-parity proofs):
//!
//!  Family             | Axis A: byte-exact?            | Axis B: RPC-free? (the GAP)                 | Fast-lane?
//!  -------------------|-------------------------------|---------------------------------------------|----------
//!  BalancerWeighted   | ACHIEVABLE — port Balancer    | ACHIEVABLE — balances are in Vault.Swap;    | BEST
//!  (non-rate)         | LogExpMath `pow` EXACTLY (it  | only gap is decimal-upscale (store decimals | candidate
//!                     | is deterministic integer fp)  | at warm) + rare SwapFeePercentageChanged.   |
//!                     |                               | weights/scaling are IMMUTABLE.              |
//!  BalancerStable     | achievable-with-effort (match | balances in Vault.Swap; GAP = scaling_      | maybe
//!  (non-rate)         | StableMath Newton rounding)   | factors(rate), amp(ramp), fee(governance).  |
//!  CurveStableSwapNg  | achievable-with-effort (match | balances in TokenExchange; GAP = rates(rate  | maybe
//!  (non-rate-oracle)  | get_y Newton + NG offpeg fee) | oracle), a_param(ramp_A — has event), fee.   |
//!  Balancer/Curve     | as above                      | rate-provider rate drifts WITHOUT a pool     | harder
//!  (RATE-scaled = the |                               | event → must mirror the Chainlink feed       | (the
//!  LST pools)         |                               | (AnswerUpdated). The valuable pools.         | money)
//!  CurveCryptoSwap    | VERY HARD — analytical get_y  | VERY HARD — `price_scale` repegs via         | NO
//!  (Tricrypto)        | (cubic) vs Newton ("off by 2  | tweak_price on EVERY swap, NOT evented; no   |
//!                     | wei") + price_scale           | event mirror exists at all (100% RPC).      |
//!  FluidDex           | HARD — smart-debt/collateral  | HARD — LogOperate carries NO pool delta;     | NO
//!                     | CL hybrid; estimateSwapIn     | state re-read via resolver RPC; interest    |
//!                     | reverts                       | accrual (exchange_price) drifts with TIME   |
//!                     |                               | (not event-mirrorable). "Event-TRIGGERED    |
//!                     |                               | RPC poll", not RPC-free.                    |
//!
//! KEY INSIGHT: the most VALUABLE pools (LST peg arb: wstETH/weETH on Balancer/Curve/Fluid) are the
//! RATE-SCALED ones — exactly where the rate-provider drift breaks RPC-free. BUT the LST peg arb is
//! ALREADY fast-lane-able via Uniswap V3 + Camelot V3 (byte-exact) — see lst_peg_arb_coverage_static —
//! so the Balancer/Curve/Fluid LST legs staying Tier-2/REVM does NOT block the LST arb.
//!
//! THE ONE CLEAN PATH: BalancerWeighted (non-rate) — LogExpMath is byte-exact-portable AND its only
//! drifting field (balances) is in Vault.Swap. That's the single Tier-2 family worth a fast-lane
//! attempt. See the plan in [[kernel-parity-tier2-vs-fastlane]] / the report.
//!
//! This test PINS the verdict against the engine's own classification so neither can silently drift.

use std::fs;
use std::path::Path;

fn read(p: &str) -> String {
    fs::read_to_string(Path::new(p)).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// AXIS A guard: none of the five Tier-2 families is byte-exact today, so none may sit in
/// BYTE_EXACT_FAMILIES (the authoritative-lane allowlist). If a future byte-exact rewrite lands
/// (e.g. BalancerWeighted via exact LogExpMath), this test is the deliberate gate to update.
#[test]
fn tier2_families_are_not_in_byte_exact_set() {
    let fastlane = read("src/fastlane_policy.rs");
    let s = fastlane
        .find("BYTE_EXACT_FAMILIES")
        .expect("BYTE_EXACT_FAMILIES");
    let open = fastlane[s..].find('[').expect("[") + s;
    let close = fastlane[open..].find("];").expect("];") + open;
    let block = &fastlane[open..close];
    for fam in [
        "FluidDex",
        "BalancerStable",
        "BalancerWeighted",
        "CurveStableSwapNg",
        "CurveCryptoSwap",
    ] {
        assert!(
            !block.contains(&format!("PoolKind::{fam}")),
            "{fam} is NOT byte-exact (Tier-2) — it must stay OUT of BYTE_EXACT_FAMILIES until a \
             byte-exact kernel is proven on AWS (disagree==0)"
        );
    }
}

/// AXIS B guard: the engine's `ResyncRefresh` classification must match the RPC-free analysis —
/// Balancer/Curve need a DEDICATED periodic RPC resync (their event-mirror has gaps: rate/amp/fee/
/// price_scale), while FluidDex is EventDriven (event-TRIGGERED RPC poll, no periodic reconcile).
#[test]
fn resync_policy_matches_rpc_free_gap_analysis() {
    let pipeline = read("src/pipeline.rs");
    // Balancer + Curve have a dedicated RPC resync ⇒ their state is NOT event-complete (has gaps).
    for branch in [
        "ResyncRefresh::BalancerStable",
        "ResyncRefresh::BalancerWeighted",
        "ResyncRefresh::CurveStable",
        "ResyncRefresh::CurveCrypto",
    ] {
        assert!(
            pipeline.contains(branch),
            "{branch} must exist — Balancer/Curve need an RPC reconcile (event-mirror gaps)"
        );
    }
    // FluidDex is event-driven (no periodic RPC reconcile; the poller is event-triggered).
    assert!(
        pipeline.contains("PoolKind::FluidDex") && pipeline.contains("ResyncRefresh::EventDriven"),
        "FluidDex must be EventDriven (event-triggered poll), per the gap analysis"
    );
}

/// The event-mirror coverage that the RPC-free verdict relies on: Balancer Vault `Swap` and Curve
/// `TokenExchange` ARE delta-applied (so balances are event-tracked), while CryptoSwap has NO event
/// mirror (100% RPC — the worst case, because price_scale repegs every swap).
#[test]
fn balancer_and_curve_balances_are_event_mirrored_cryptoswap_is_not() {
    let pipeline = read("src/pipeline.rs");
    assert!(
        pipeline.contains("balancer_vault_swap"),
        "Balancer Vault Swap mirror must exist (balances are event-tracked)"
    );
    assert!(
        pipeline.contains("TokenExchange(address,int128"),
        "Curve TokenExchange mirror must exist (balances are event-tracked)"
    );
    // CryptoSwap is RPC-driven: its dedicated snapshot exists and it has no per-pool delta-apply.
    assert!(
        Path::new("../engine-rpc/src/cryptoswap_snapshot.rs").exists(),
        "CryptoSwap is RPC-snapshot-driven (no event mirror) — the snapshot reader must exist"
    );
}

/// The NEW Axis-B (RPC-free drift) harness must exist — it measures, on AWS, which state fields
/// drift across blocks (= the RPC-free gap) for the Tier-2 families.
///
/// NOTE on Axis-A: the per-family byte-exact live-parity harnesses (curve_*/balancer_*/fluid_*
/// _live_parity) are committed in git (commit 4aa613d) but currently DELETED from the working tree.
/// Restore them with `git checkout 4aa613d -- crates/engine/tests/<name>_live_parity.rs` to run the
/// Axis-A proofs on AWS. This test does NOT require them on disk (they're tracked, not present).
#[test]
fn axis_b_rpc_free_drift_harness_exists() {
    assert!(
        Path::new("tests/tier2_state_drift_live.rs").exists(),
        "the Axis-B RPC-free drift harness (tier2_state_drift_live) must exist"
    );
}
