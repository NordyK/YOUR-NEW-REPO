//! WIRED — fail-closed singleton-state guard (Codex #1/#2/#7). Was staged; the production changes
//! described below are now implemented in revm_fork.rs + pipeline.rs and these tests ENFORCE them.
//!
//! ============================ WHY ============================
//! The V4/Curve equivalence isolation (V4CurveStateDivergenceAll.Fork.t.sol) + the Gate9 state-point
//! classification (gate9_statepoint_classification_static.rs) proved the 5/27 "phantom success" rows
//! are an OFFLINE state-reproduction artifact: when the engine validates from a parent-pinned /
//! snapshot-pinned fork and re-executes only the trigger tx, any SINGLETON / shared-state venue (V4
//! PoolManager, Curve-NG, Fluid) mutated by an earlier same-block tx is STALE in the validation DB,
//! so the route is over-priced and accepted as a spurious success.
//!
//! That is NOT a kernel/executor bug, and PRODUCTION (live-tip fork) is unaffected. But the OFFLINE
//! path is unsafe: it can accept stale/over-priced singleton routes, and a missing-slot fetch error
//! is silently turned into fabricated ZERO state (revm_fork.rs fetch_storage). This module STAGES the
//! capital-safety guard that closes that hole. It does not modify production code — the production
//! change lands in the post-Codex reconciliation pass; these tests are written to flip green then.
//!
//! ============================ THE GUARD (two parts) ============================
//! Part 1 — FAIL-CLOSED FETCHER  (crates/engine/src/revm_fork.rs)
//!   Current (fail-OPEN): `fetch_storage` returns `U256::ZERO` on `Err(_)` (line ~94); `fetch_account`
//!   defaults balance=ZERO / code=None / nonce=0 on error. A transient RPC/archive error therefore
//!   becomes fabricated state and can manufacture a phantom profit.
//!   Target (fail-CLOSED): add an error counter (e.g. `static RPC_FETCH_ERRORS: AtomicU64`) bumped on
//!   every fetch `Err`. The validator snapshots it around each candidate's REVM run; if the delta > 0
//!   the candidate touched errored/fabricated state -> REJECT it with reject_reason
//!   "fetch_error_fail_closed" (never accept profit computed against fabricated state).
//!
//! Part 2 — SINGLETON-FAMILY OFFLINE GUARD  (crates/engine/src/pipeline.rs, validator accept site)
//!   A route containing a singleton / shared-state hop is only `accept`-able from LIVE-TIP validation.
//!   From parent-pinned/snapshot-pinned offline state its REVM verdict is NON-AUTHORITATIVE: downgrade
//!   accept -> reject with reject_reason "singleton_offline_nonauthoritative" (the row is still
//!   recorded for diagnostics, just never treated as a clean capital candidate).
//!   Singleton/shared-state Mantis exchange ids: 17 (CurveCryptoSwap), 18 (CurveStableSwapNg),
//!   20 (UniswapV4 — the proven-divergent case), 21 (FluidDex). Per-pair V2/V3 pools are exempt.
//!   "Offline" = ENGINE_SNAPSHOT_BLOCK set / FORCE_ISOLATED_REPLAY / REVM_BLOCK_CTX pinned (not tip).
//!
//! ============================ INVARIANTS (enforced by the target tests below) ============================
//!  I1. revm_fork.rs accounts for fetch errors (no silent fabricated-zero on the validation path).
//!  I2. pipeline.rs rejects a candidate whose validation touched a fetch error (fail-closed).
//!  I3. pipeline.rs marks singleton-family routes non-authoritative under offline validation.
//!  I4. The singleton id set {17,18,20,21} is the guard's family set.
//!
//! ============================ STATUS: WIRED ============================
//! Part 1 (revm_fork.rs RPC_FETCH_ERRORS + pipeline.rs fetch_error_fail_closed) and Part 2
//! (route_has_singleton_family + ValidationMode + singleton_offline_nonauthoritative + the fast-lane
//! freshness gate) are implemented; the tests below ENFORCE them. The V4/Curve offline phantom
//! successes are now rejected non-authoritative from a parent-pinned/offline state point.

use std::{fs, path::Path};

const REVM_FORK_RS: &str = "src/revm_fork.rs";
const PIPELINE_RS: &str = "src/pipeline.rs";

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

/// I1+I2 — fail-closed fetcher (Codex #1) is WIRED: errors are accounted and the validator rejects.
#[test]
fn fetcher_is_fail_closed_and_rejects_on_error() {
    let revm = read(REVM_FORK_RS);
    let pipeline = read(PIPELINE_RS);
    assert!(
        revm.contains("RPC_FETCH_ERRORS"),
        "I1: fetch errors must be accounted (no silent fabricated ZERO)"
    );
    assert!(
        revm.contains("rpc_fetch_error_count"),
        "I1: the validator must be able to sample the fetch-error count"
    );
    assert!(
        pipeline.contains("fetch_error_fail_closed"),
        "I2: validator must reject a candidate whose validation hit a fetch error"
    );
    // [WP3 · Fix D] The verdict samples the fetch-error delta from the VALIDATOR-LOCAL counter bundle
    // (`val_counters` errors) around the REVM call — NOT the process-global `rpc_fetch_error_count()`,
    // which cross-thread traffic (advancer/shadow/audit) contaminates. This assertion was updated when
    // WP3 moved candidate error attribution from the global to the per-instance bundle.
    assert!(
        pipeline.contains("fetch_errors") && pipeline.contains("val_counters"),
        "I2: validator must sample the fetch-error delta from the validator-local counter bundle"
    );
    assert!(
        !pipeline.contains("rpc_fetch_error_count("),
        "WP3: the candidate verdict must NOT read the process-global rpc_fetch_error_count()"
    );
}

/// I3+I4 — singleton-family offline non-authoritative guard (Codex #2) is WIRED.
#[test]
fn singleton_routes_are_nonauthoritative_offline() {
    let pipeline = read(PIPELINE_RS);
    assert!(
        pipeline.contains("route_has_singleton_family"),
        "I3: a route-level singleton-family predicate must exist"
    );
    assert!(
        pipeline.contains("singleton_offline_nonauthoritative"),
        "I3: offline-validated singleton-family routes must be rejected non-authoritative"
    );
    assert!(
        pipeline.contains("ValidationMode") && pipeline.contains("LiveTipCurrent"),
        "I3: the guard must distinguish the live-tip state point from offline/unknown"
    );
    // I4: the singleton/shared-state families must all be in the predicate's match.
    for fam in [
        "PoolKind::UniswapV4",
        "PoolKind::CurveStableSwapNg",
        "PoolKind::CurveCryptoSwap",
        "PoolKind::FluidDex",
    ] {
        assert!(
            pipeline.contains(fam),
            "I4: singleton family {fam} must be part of the guard's family set"
        );
    }
}

/// #7 — a remote cold RPC on the live submit path is rejected, not just warned.
#[test]
fn remote_cold_rpc_on_live_path_is_rejected() {
    let pipeline = read(PIPELINE_RS);
    assert!(
        pipeline.contains("remote_cold_rpc_on_submit_path"),
        "#7: a live-mode remote cold RPC during validation must reject the candidate"
    );
    assert!(
        pipeline.contains("remote_cold_on_submit"),
        "#7: the validator must compute the remote-cold-on-submit condition"
    );
}

/// #2 fast-lane sub-point — singleton venues are held off the authoritative zero-RPC lane until live
/// event-mirror freshness is proven (so an operator flipping FASTLANE_AUTHORITATIVE can't silently
/// put V4/Curve/Fluid on the no-REVM path).
#[test]
fn singletons_held_off_authoritative_fastlane_until_proven() {
    let pipeline = read(PIPELINE_RS);
    assert!(
        pipeline.contains("fastlane_singletons_allowed"),
        "fast-lane eligibility must gate singleton families on a freshness flag"
    );
    assert!(
        pipeline.contains("FASTLANE_SINGLETON_LIVE_PROVEN"),
        "the singleton fast-lane gate must default OFF behind an explicit freshness env flag"
    );
}
