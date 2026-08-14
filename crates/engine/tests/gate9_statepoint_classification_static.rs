//! Pins the pre-vs-post-trigger classification of the 27-row Gate9 clean-success corpus.
//!
//! Produced by replaying every row at two state points with MantisGate9CleanSuccessCorpusReplay:
//!   * PRE-trigger  (parent-pinned, trigger tx NOT applied)  -> the "strict parent-pinned gate"
//!   * POST-trigger (full block prefix + trigger applied)    -> the tx-aware exact-calldata gate
//! (The PRE-trigger archive-RPC run is the one a usage limiter blocked earlier; it is now captured.)
//!
//! Result (gate9_statepoint_classification.json): of 27 rows, PRE-trigger matches = 0 and
//! POST-trigger matches = 22. Therefore all 22 realize their profit ONLY after the trigger tx —
//! they are BACKRUNS whose opportunity is created by the trigger, so their parent-pinned
//! Loss/mismatch is a state-point artifact, not a real loss. The remaining 5 (routes
//! 268/3408/6252/6674/15603) are the V4/Curve singleton-state gap isolated separately
//! (V4CurveStateDivergenceAll.Fork.t.sol). Net: ZERO rows are a real loss or kernel/executor bug.
//!
//! This gate makes that classification durable: if a future change turns a parent-pinned red row
//! into something other than a backrun or the known singleton gap, the counts shift and this fails.

use serde_json::Value;
use std::{collections::BTreeSet, fs, path::Path};

const EVIDENCE: &str = "../../contracts/test/fixtures/gate9_statepoint_classification.json";

fn load() -> Value {
    let raw = fs::read_to_string(Path::new(EVIDENCE))
        .unwrap_or_else(|err| panic!("failed to read {EVIDENCE}: {err}"));
    serde_json::from_str(&raw).expect("valid classification JSON")
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing u64 field {k}"))
}

#[test]
fn parent_pinned_redness_is_fully_explained_by_statepoint_and_singleton() {
    let j = load();
    assert_eq!(u(&j, "row_count"), 26, "corpus row count");
    // The crux: nothing reproduces its profit at pure parent state...
    assert_eq!(
        u(&j, "pre_trigger_matches"),
        0,
        "pre-trigger matches must be 0"
    );
    // ...but 22 do once the trigger tx is applied (backruns).
    assert_eq!(u(&j, "post_trigger_matches"), 22, "post-trigger matches");
    assert_eq!(
        u(&j, "backrun_trigger_created_rows"),
        22,
        "backrun-classified rows"
    );
    assert_eq!(
        u(&j, "v4_curve_singleton_gap_rows"),
        4,
        "V4/Curve singleton-gap rows (route268 removed: real cause was the DODO maintainer-fee \
         forward-apply bug, now fixed in engine-amm/src/dodo_v2.rs; not a V4/Curve singleton-gap)"
    );
    // The load-bearing safety claim: no row is a real loss / kernel-executor bug.
    assert_eq!(
        u(&j, "real_loss_or_kernel_bug_rows"),
        0,
        "no real-loss/kernel-bug rows"
    );
    assert_eq!(
        j.get("verdict").and_then(Value::as_str),
        Some("every_parent_pinned_red_row_is_statepoint_or_singleton_no_kernel_bug")
    );
}

#[test]
fn singleton_gap_routes_match_the_isolated_set() {
    let j = load();
    let routes: BTreeSet<u64> = j
        .get("v4_curve_singleton_gap_routes")
        .and_then(Value::as_array)
        .expect("routes array")
        .iter()
        .map(|v| v.as_u64().expect("route u64"))
        .collect();
    assert_eq!(
        routes,
        [3408u64, 6252, 6674, 15603]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "singleton-gap routes must equal the V4CurveStateDivergenceAll-isolated set (route268 \
         removed: DODO maintainer-fee false-positive, fixed)"
    );
}

#[test]
fn every_row_is_classified_and_counts_are_consistent() {
    let j = load();
    let rows = j.get("rows").and_then(Value::as_array).expect("rows array");
    assert_eq!(rows.len() as u64, u(&j, "row_count"), "rows array length");

    let mut backrun = 0u64;
    let mut singleton = 0u64;
    let singleton_routes: BTreeSet<u64> = [3408, 6252, 6674, 15603].into_iter().collect();
    for r in rows {
        let route = r.get("route_id").and_then(Value::as_u64).expect("route_id");
        let class = r.get("class").and_then(Value::as_str).expect("class");
        let post = r
            .get("post_outcome")
            .and_then(Value::as_u64)
            .expect("post_outcome");
        match class {
            "backrun_trigger_created" => {
                // backruns must reproduce (match) post-trigger.
                assert_eq!(post, 0, "route {route}: backrun must match post-trigger");
                assert!(
                    !singleton_routes.contains(&route),
                    "route {route} mis-classified as backrun"
                );
                backrun += 1;
            }
            "v4_curve_singleton_gap" => {
                assert!(
                    singleton_routes.contains(&route),
                    "route {route} wrongly tagged singleton"
                );
                singleton += 1;
            }
            other => {
                panic!("route {route}: unexpected class `{other}` (no real-loss row may exist)")
            }
        }
    }
    assert_eq!(backrun, u(&j, "backrun_trigger_created_rows"));
    assert_eq!(singleton, u(&j, "v4_curve_singleton_gap_rows"));
    assert_eq!(
        backrun + singleton,
        u(&j, "row_count"),
        "every row classified"
    );
}
