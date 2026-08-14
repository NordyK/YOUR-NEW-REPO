//! ZERO-RPC FAST-LANE READINESS across the non-V4 families — pins the contract per family so none
//! can silently regress. Companion to v4_fastlane_readiness_static.
//!
//! Laptop-checkable boundary:
//!   * V2 (Uni/Sushi/Camelot): reserves are state-complete from `Sync` ⇒ ALREADY zero-RPC; byte-exact
//!     (in BYTE_EXACT_FAMILIES). Promotable on the AWS `disagree==0` measurement, nothing to build.
//!   * V3 (Uni/Sushi/Pancake/Algebra): byte-exact CL; the tick map was async-RPC (`fetch_v3_tick_window`).
//!     This cycle added the Mint/Burn event-mirror (decode + delta-apply, reusing the V4 primitive) on
//!     the subscription pipeline ⇒ the local event mirror is wired. Live byte-exact freshness is not
//!     claimed from this laptop-only static test.
//!   * SolidlyV3: priced from V2 reserves (Solidly stable x³y+xy³), so reserves are zero-RPC-fresh, but
//!     it is NOT a V3-CL/Mint-Burn case and its kernel is NOT yet parity-proven ⇒ Tier-2 (not in
//!     BYTE_EXACT_FAMILIES). Promotion needs a solidly_stable getAmountOut parity proof (low priority).
//!   * CurveStableSwapNg: NOT byte-exact (NG dynamic-offpeg fee + rate-oracle/metapool) ⇒ Tier-2 only;
//!     A-ramp/zero-RPC freshness is not a laptop-provable promotion proof.

use std::{fs, path::Path};

const PIPELINE: &str = "src/pipeline.rs";
const FASTLANE: &str = "src/fastlane_policy.rs";
const V3_STATE: &str = "../engine-state/src/v3_state.rs";

fn read(p: &str) -> String {
    fs::read_to_string(Path::new(p)).unwrap_or_else(|e| panic!("read {p}: {e}"))
}
fn byte_exact_block(fastlane: &str) -> String {
    let s = fastlane
        .find("BYTE_EXACT_FAMILIES")
        .expect("BYTE_EXACT_FAMILIES");
    let open = fastlane[s..].find('[').expect("open") + s;
    let close = fastlane[open..].find("];").expect("close") + open;
    fastlane[open..close].to_string()
}

#[test]
fn v2_family_is_zero_rpc_capable() {
    let fastlane = read(FASTLANE);
    let bx = byte_exact_block(&fastlane);
    for v in ["UniswapV2", "SushiV2", "CamelotV2"] {
        assert!(
            bx.contains(&format!("PoolKind::{v}")),
            "{v} must be in BYTE_EXACT_FAMILIES"
        );
    }
    // State completeness: V2 reserves are delta-applied from Sync in the log updater (no RPC needed).
    let pipeline = read(PIPELINE);
    assert!(
        pipeline.contains("topic_set.sync112") && pipeline.contains("topic_set.sync256"),
        "V2 reserves must be kept fresh from Sync events"
    );
}

#[test]
fn v3_family_tick_map_is_zero_rpc_via_mint_burn_mirror() {
    let fastlane = read(FASTLANE);
    let bx = byte_exact_block(&fastlane);
    for v in ["UniswapV3", "SushiV3", "PancakeV3", "AlgebraV19"] {
        assert!(
            bx.contains(&format!("PoolKind::{v}")),
            "{v} must be in BYTE_EXACT_FAMILIES"
        );
    }
    // The event-mirror primitive (shared with V4).
    let v3_state = read(V3_STATE);
    assert!(
        v3_state.contains("pub fn decode_v3_liquidity_event")
            && v3_state.contains("pub fn apply_modify_liquidity"),
        "V3 Mint/Burn decode + the shared apply primitive must exist"
    );
    // Wired: the Mint/Burn topics are on the LIVE eth_subscribe filter AND dispatched in apply_log_value.
    let pipeline = read(PIPELINE);
    assert!(
        pipeline.contains("Mint(address,address,int24,int24,uint128,uint256,uint256)")
            && pipeline.contains("Burn(address,int24,int24,uint128,uint256,uint256)"),
        "V3 Mint/Burn topics must be registered"
    );
    // The subscribed topic[0] set is now the named `wss_subscribed_topics()` list (which the
    // eth_subscribe message builds from), cross-checked at runtime by
    // `wss_subscription_covers_handled_log_topics`. v3_mint/v3_burn must be in it.
    assert!(
        pipeline.contains("wss_subscribed_topics(&topics)")
            && pipeline.contains("t.v3_mint,")
            && pipeline.contains("t.v3_burn,"),
        "v3_mint/v3_burn must be in the live eth_subscribe set (wss_subscribed_topics)"
    );
    assert!(
        pipeline.contains("topic0 == topic_set.v3_mint || topic0 == topic_set.v3_burn"),
        "apply_log_value must dispatch the V3 Mint/Burn delta-apply branch"
    );
    // This is the local proof boundary: code wiring and default promotion policy only.
    // On-chain freshness/parity is an AWS/live measurement, so this suite must not require it.
}

#[test]
fn solidlyv3_and_curve_stableswap_are_correctly_tier2_only() {
    let fastlane = read(FASTLANE);
    let bx = byte_exact_block(&fastlane);
    // Both are NOT byte-exact (SolidlyV3 unproven reserves-stable kernel; CurveStableSwapNg NG fee/rate)
    // → must stay out of the default zero-RPC promoted set.
    for v in ["SolidlyV3", "CurveStableSwapNg"] {
        assert!(
            !bx.contains(&format!("PoolKind::{v}")),
            "{v} must NOT be in BYTE_EXACT_FAMILIES (Tier-2 only — not byte-exact)"
        );
    }
    // SolidlyV3 is reserves-based (priced from v2_state), not a V3-CL/Mint-Burn case.
    let pipeline = read(PIPELINE);
    assert!(
        pipeline.contains("Solidly stable pools reuse V2State reserves"),
        "SolidlyV3 must remain documented as reserves-based (not a tick-map venue)"
    );
    // This test intentionally stops at local policy containment. Live Curve verdict proof belongs to
    // the AWS/live measurement phase and must not be encoded as a laptop gate.
}
