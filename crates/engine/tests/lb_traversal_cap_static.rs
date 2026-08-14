//! Trader Joe LB MAX_TRAVERSAL_BINS=50 — data-grounded cap-sufficiency gate.
//!
//! Codex found 2 full-fill false-negatives from the 50-bin traversal cap and (correctly) classed it
//! as a fail-closed coverage limit, not an over-quote. This gate answers the only open question —
//! "does 50 silently drop *profitable* arbs?" — with on-chain evidence, and pins the decision.
//!
//! Evidence (lb_traversal_cap_evidence.json, Dune 7736963/7736970 over trader_joe_v2_1 LBPair swaps):
//!   * 593,935 swaps; only 86 (0.0145%) cross >50 bins; p99.9 = 2 bins.
//!   * every >50-bin swap is micro-notional (<= $932, mostly << $60) on thin/exotic pairs (USDV, ...).
//!   => 50 bins drops ZERO material profitable arbs.
//! Safety: at the cap the kernel returns `remaining_in > 0` (no fabricated full fill); the caller
//! rejects partial fills (None) — so the cap can only false-NEGATIVE, never over-quote. The constant
//! also sets the +/-50-bin snapshot window, so raising it costs snapshot RPC for a negligible tail.
//! DECISION: keep 50, fail-closed, documented — no production change warranted.
//!
//! This gate pins the production cap + its fail-closed shape + the evidence, so the cap cannot be
//! changed (or the kernel made to fabricate a full fill) without re-justifying against fresh data.

use serde_json::Value;
use std::{fs, path::Path};

const KERNEL: &str = "../engine-amm/src/trader_joe_lb.rs";
const SNAPSHOT: &str = "../engine-rpc/src/lb_snapshot.rs";
const FAILCLOSED_TEST: &str = "../engine-amm/tests/trader_joe_lb_traversal_cap.rs";
const EVIDENCE: &str = "../../contracts/test/fixtures/lb_traversal_cap_evidence.json";

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn evidence() -> Value {
    serde_json::from_str(&read(EVIDENCE)).expect("valid LB evidence JSON")
}

#[test]
fn production_cap_is_fifty_and_fail_closed_not_overquote() {
    let kernel = read(KERNEL);
    assert!(
        kernel.contains("pub const MAX_TRAVERSAL_BINS: u32 = 50;"),
        "production cap changed — re-run the Dune sufficiency analysis and update the evidence before changing it"
    );
    // At the cap the loop must BREAK (bounded traversal)...
    assert!(
        kernel.contains("if bins_processed >= MAX_TRAVERSAL_BINS"),
        "kernel must bound traversal at MAX_TRAVERSAL_BINS"
    );
    // ...and report the unfilled remainder rather than fabricating a full fill (no over-quote).
    assert!(
        kernel.contains("remaining_in: remaining"),
        "kernel must surface remaining_in (a capped swap is a partial fill, never a fabricated full quote)"
    );
    // The +/-50 snapshot window is coupled to the same constant (why raising it costs RPC).
    let snapshot = read(SNAPSHOT);
    assert!(
        snapshot.contains("MAX_TRAVERSAL_BINS"),
        "snapshot window must be coupled to MAX_TRAVERSAL_BINS"
    );
    // The fail-closed caller behavior is proven by a dedicated test (Codex).
    assert!(
        Path::new(FAILCLOSED_TEST).exists(),
        "the fail-closed traversal-cap proof test must exist: {FAILCLOSED_TEST}"
    );
}

#[test]
fn evidence_shows_fifty_bins_drops_no_material_arbs() {
    let e = evidence();
    let dist = e.get("distribution").expect("distribution");
    let total = dist
        .get("total_swaps")
        .and_then(Value::as_u64)
        .expect("total_swaps");
    let over50 = dist
        .get("swaps_over_50_bins")
        .and_then(Value::as_u64)
        .expect("over_50");
    assert!(
        total >= 100_000,
        "evidence sample too small to be representative: {total}"
    );
    // The >50-bin tail must be a negligible fraction of all swaps.
    let frac = over50 as f64 / total as f64;
    assert!(
        frac < 0.001,
        "more than 0.1% of LB swaps exceed the cap ({over50}/{total}={frac:.5}) — re-evaluate raising it"
    );
    // p99.9 traversal depth must sit far under the cap.
    let p999 = dist
        .get("p999_bins")
        .and_then(Value::as_u64)
        .expect("p999_bins");
    assert!(
        p999 <= 50,
        "p99.9 traversal depth {p999} approaches the cap — re-evaluate"
    );

    // The tail must be economically immaterial (micro-notional), i.e. not profitable arbs.
    let tail = e.get("tail_characterization").expect("tail");
    let max_usd = tail
        .get("max_usd_leg_among_over_50")
        .and_then(Value::as_f64)
        .expect("max_usd");
    assert!(
        max_usd < 5_000.0,
        "a >50-bin swap with ${max_usd} notional appeared — could be a real arb; re-evaluate the cap"
    );
}

#[test]
fn decision_is_keep_fifty_and_safety_is_false_negative_only() {
    let e = evidence();
    assert_eq!(
        e.get("decision").and_then(Value::as_str),
        Some("keep_50_economically_justified"),
        "the documented LB cap decision must remain keep-50"
    );
    let failure_mode = e
        .get("safety")
        .and_then(|s| s.get("failure_mode"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        failure_mode.contains("false_negative_only"),
        "the cap's failure mode must be false-negative only (never over-quote): {failure_mode}"
    );
}
