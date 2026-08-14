//! Default-off venue safety boundary — Kyber / BalancerWeighted / CurveCryptoSwap.
//!
//! Codex flags these as "not fully certified; Kyber has over-quotes but is default-off". An
//! over-quote (claiming more output than reality) is the *dangerous* failure mode (it can lose
//! capital), unlike LB's fail-closed false-negative. This gate pins the EXACT mechanism that keeps
//! the over-quote away from capital — because the audit found "default-off" is NOT enumeration
//! exclusion:
//!
//!   1. These venues ARE admitted to route enumeration (engine-routes is_phase1_supported).
//!   2. REVM_VALIDATE defaults ON (env_flag_default_on => env_flag_with_default(name, true)); every
//!      candidate is re-executed in REVM before submit, so an over-quote is caught + net-gated.
//!   3. The default fast-lane promoted set (BYTE_EXACT_FAMILIES) is V2/V3/V4/Algebra ONLY — it
//!      excludes Kyber/CryptoSwap/BalancerWeighted, so they cannot bypass REVM via the zero-RPC lane.
//!   4. The promotion gate (family_promotion_ready) requires disagree==0, so an over-quoting family
//!      is structurally INELIGIBLE to be promoted onto the authoritative lane.
//!
//! => the over-quote is contained by the REVM backstop + non-promotion, not by venue exclusion. This
//! gate trips if any of those four protections erode (e.g. a venue added to BYTE_EXACT_FAMILIES, the
//! promotion gate weakened, or REVM_VALIDATE made default-off). See
//! contracts/test/fixtures/venue_default_off_boundary_evidence.json.

use serde_json::Value;
use std::{fs, path::Path};

const ENUMERATOR: &str = "../engine-routes/src/enumerator.rs";
const PIPELINE: &str = "src/pipeline.rs";
const FASTLANE: &str = "src/fastlane_policy.rs";
const EVIDENCE: &str = "../../contracts/test/fixtures/venue_default_off_boundary_evidence.json";

const VENUES: [&str; 3] = ["KyberSwapElastic", "CurveCryptoSwap", "BalancerWeighted"];

fn read(path: &str) -> String {
    fs::read_to_string(Path::new(path)).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

/// Extract the text of the `BYTE_EXACT_FAMILIES` array literal (between `[` and `];`).
fn byte_exact_block(src: &str) -> String {
    let start = src
        .find("BYTE_EXACT_FAMILIES")
        .expect("BYTE_EXACT_FAMILIES must exist");
    let open = src[start..].find('[').expect("array open") + start;
    let close = src[open..].find("];").expect("array close") + open;
    src[open..close].to_string()
}

#[test]
fn venues_are_enumerated_not_excluded() {
    // The real boundary: these ARE routable, so containment must come from REVM, not exclusion.
    let enumerator = read(ENUMERATOR);
    let supported_start = enumerator
        .find("fn is_phase1_supported")
        .expect("is_phase1_supported must exist");
    let block = &enumerator[supported_start..supported_start + 800];
    for v in VENUES {
        assert!(
            block.contains(v),
            "{v} must be admitted by is_phase1_supported (the boundary is REVM, not exclusion)"
        );
    }
}

#[test]
fn revm_validation_is_default_on_backstop() {
    let pipeline = read(PIPELINE);
    // require_revm is gated on REVM_VALIDATE via the default-ON helper.
    assert!(
        pipeline.contains("env_flag_default_on(\"REVM_VALIDATE\")"),
        "submit must require REVM validation by default"
    );
    // env_flag_default_on must actually default to true.
    assert!(
        pipeline.contains("env_flag_with_default(name, true)"),
        "env_flag_default_on must default REVM_VALIDATE to ON"
    );
    assert!(
        pipeline.contains("require_revm"),
        "the validator must carry a require_revm gate"
    );
}

#[test]
fn over_quoting_venues_are_not_default_fast_lane_promoted() {
    let fastlane = read(FASTLANE);
    let block = byte_exact_block(&fastlane);
    // The default zero-RPC promoted set must NOT include any of the not-certified venues.
    for v in VENUES {
        assert!(
            !block.contains(v),
            "{v} must NOT be in BYTE_EXACT_FAMILIES (would let an over-quote bypass REVM on the fast lane)"
        );
    }
    // And the promotion gate must refuse any family that disagrees with REVM.
    assert!(
        fastlane.contains("disagree == 0"),
        "family_promotion_ready must require disagree==0 (an over-quoting family cannot be promoted)"
    );
}

#[test]
fn evidence_pins_the_contained_decision() {
    let e: Value = serde_json::from_str(&read(EVIDENCE)).expect("valid boundary evidence JSON");
    assert_eq!(
        e.get("decision").and_then(Value::as_str),
        Some("no_production_change_required_boundary_structurally_enforced")
    );
    let status = e
        .get("kyber_overquote_status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        status.contains("contained"),
        "Kyber over-quote must be recorded as contained"
    );
    assert!(
        e.get("enable_criterion")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("disagree==0")),
        "enable criterion must require proven REVM agreement before promotion"
    );
}
