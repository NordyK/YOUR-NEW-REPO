//! FAST-LANE PROMOTION READINESS — answers "can the finished venues be promoted to the fast lane?"
//!
//! The finished Tier-2 families (CurveCryptoSwap/Tricrypto, BalancerWeighted, FluidDex v1) must be
//! MEASURABLE by the shadow monitor during the AWS dry run (so the harness can decide promotion) but
//! must NOT be auto-promoted onto the zero-RPC lane from local evidence alone. A non-byte-exact
//! kernel can never satisfy the promotion gate (`disagree == 0` vs REVM), so this gate pins the
//! laptop-provable contract: measure the families, keep them out of the byte-exact default set, and
//! keep the release matrix red unless unsafe-overquote venues remain contained.

use std::{fs, path::Path};

const FASTLANE: &str = "src/fastlane_policy.rs";
const PIPELINE: &str = "src/pipeline.rs";
const REMAINING_GATES: &str =
    "../../../Codex/REMAINING_GATES_EDGE_CASE_CLOSURE_MATRIX_2026-06-20.json";

/// The Tier-2 families finished this cycle (measurable, not byte-exact, REVM-validated).
const FINISHED_TIER2: [&str; 3] = ["CurveCryptoSwap", "BalancerWeighted", "FluidDex"];

fn read(p: &str) -> String {
    fs::read_to_string(Path::new(p)).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// Extract a `const NAME: [...] = [ ... ];` array literal body.
fn array_block(src: &str, name: &str) -> String {
    let s = src.find(name).unwrap_or_else(|| panic!("{name} not found"));
    let open = src[s..].find('[').expect("array open") + s;
    let close = src[open..].find("];").expect("array close") + open;
    src[open..close].to_string()
}

#[test]
fn finished_families_are_measurable_but_not_auto_promoted() {
    let fastlane = read(FASTLANE);
    let all = array_block(&fastlane, "ALL_FAMILIES");
    let byte_exact = array_block(&fastlane, "BYTE_EXACT_FAMILIES");

    for fam in FINISHED_TIER2 {
        let pk = format!("PoolKind::{fam}");
        // MEASURABLE: must have a per-family agree/disagree slot so the shadow monitor scores it.
        assert!(
            all.contains(&pk),
            "{fam}: missing from ALL_FAMILIES — the fastlane harness cannot measure its promotability"
        );
        // NOT AUTO-PROMOTED: must be absent from the default zero-RPC set (it is not byte-exact).
        assert!(
            !byte_exact.contains(&pk),
            "{fam}: must NOT be in BYTE_EXACT_FAMILIES — its kernel is not byte-exact (see live parity)"
        );
    }
}

#[test]
fn promotion_gate_blocks_non_byte_exact_families() {
    let fastlane = read(FASTLANE);
    // The gate requires zero disagreements with REVM — a ~1e-6..1e-12 kernel will always disagree by
    // a few wei, so it can never be promoted. This is the mechanism that makes the verdict honest.
    assert!(
        fastlane.contains("disagree == 0"),
        "family_promotion_ready must require disagree == 0 (else a non-exact family could be promoted)"
    );
    assert!(
        fastlane.contains("fn family_promotion_ready"),
        "family_promotion_ready gate must exist"
    );
}

#[test]
fn shadow_monitor_and_laptop_containment_evidence_exist() {
    let pipeline = read(PIPELINE);
    // The measurement mechanism the AWS dry run runs to score each family.
    for marker in ["fn shadow_fastlane_compare", "fn record_family_agreement"] {
        assert!(
            pipeline.contains(marker),
            "pipeline missing `{marker}` — per-family agree/disagree is not collected"
        );
    }
    let matrix = read(REMAINING_GATES);
    for required in [
        "venue_parity_and_fastlane_promotion",
        "red_if_kyber_or_balancer_weighted_enabled_or_promoted",
        "contained_amm_kernel_quote_parity_issue",
        "\"production_logic_issue\": true",
        "unsafe over-quote",
    ] {
        assert!(
            matrix.contains(required),
            "remaining-gates matrix must preserve laptop containment boundary: {required}"
        );
    }
}
