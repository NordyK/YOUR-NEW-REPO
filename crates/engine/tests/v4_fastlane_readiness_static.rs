//! V4 ZERO-RPC FAST-LANE READINESS — pins the complete contract so no piece can silently regress.
//!
//! V4 is the one singleton where zero-RPC is architecturally sound (state-complete events + byte-exact
//! math + per-pool-isolated state). For it to be promoted to the zero-RPC hot path, ALL of the
//! following must hold; this gate asserts each:
//!   1. MATH byte-exact — UniswapV4 in BYTE_EXACT_FAMILIES (hookless V4 == V3 CL math).
//!   2. SINGLETON-GATED — UniswapV4 in route_has_singleton_family, held off the lane until
//!      FASTLANE_SINGLETON_LIVE_PROVEN (so it falls through to REVM until freshness is proven).
//!   3. TICK-MAP FRESHNESS MECHANISM — the ModifyLiquidity event-mirror exists (decode + delta-apply)
//!      AND is on the LIVE subscription (its topic is in the eth_subscribe filter), so the tick map
//!      stays fresh from events with no RPC.
//!   4. FRESHNESS NOT LOCALLY CLAIMED — live fork proof is not a laptop test; the singleton gate
//!      must remain explicit until AWS/live evidence exists.
//!   5. MEASURABLE — the shadow monitor scores V4 vs REVM so the AWS dry run can flip the gate.
//!
//! When all five hold, the only remaining step is external live evidence confirming disagree==0 +
//! zero staleness, then flipping FASTLANE_SINGLETON_LIVE_PROVEN. This test is the laptop-side guard.

use std::{fs, path::Path};

const PIPELINE: &str = "src/pipeline.rs";
const FASTLANE: &str = "src/fastlane_policy.rs";
const V3_STATE: &str = "../engine-state/src/v3_state.rs";

fn read(p: &str) -> String {
    fs::read_to_string(Path::new(p)).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

#[test]
fn v4_math_is_byte_exact_and_singleton_gated() {
    let fastlane = read(FASTLANE);
    // 1) byte-exact math: V4 is in the default zero-RPC promoted set.
    let bx_start = fastlane
        .find("BYTE_EXACT_FAMILIES")
        .expect("BYTE_EXACT_FAMILIES");
    let bx = &fastlane[bx_start..bx_start + fastlane[bx_start..].find("];").expect("close")];
    assert!(
        bx.contains("PoolKind::UniswapV4"),
        "UniswapV4 must be in BYTE_EXACT_FAMILIES (hookless V4 = byte-exact V3 CL math)"
    );

    // 2) singleton-gated: V4 is treated as a singleton and held off the lane until proven fresh.
    let pipeline = read(PIPELINE);
    let sg_start = pipeline
        .find("fn route_has_singleton_family")
        .expect("route_has_singleton_family");
    let sg = &pipeline[sg_start..sg_start + 400];
    assert!(
        sg.contains("PoolKind::UniswapV4"),
        "UniswapV4 must be a route_has_singleton_family member (needs freshness proof)"
    );
    assert!(
        pipeline.contains("FASTLANE_SINGLETON_LIVE_PROVEN"),
        "the singleton freshness gate FASTLANE_SINGLETON_LIVE_PROVEN must exist"
    );
}

#[test]
fn v4_modify_liquidity_mirror_is_wired_for_zero_rpc_freshness() {
    let v3_state = read(V3_STATE);
    // The event-mirror primitives.
    assert!(
        v3_state.contains("pub fn decode_v4_modify_liquidity"),
        "decode_v4_modify_liquidity must exist"
    );
    assert!(
        v3_state.contains("pub fn apply_modify_liquidity"),
        "V3State::apply_modify_liquidity must exist"
    );

    let pipeline = read(PIPELINE);
    // The exact ModifyLiquidity event signature is subscribed.
    assert!(
        pipeline.contains("ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)"),
        "the V4 ModifyLiquidity topic must be registered"
    );
    // CRITICAL: it must be on the LIVE eth_subscribe topic filter (appended after v4swap), else the
    // live stream never delivers ModifyLiquidity logs and the tick map silently relies on RPC.
    let sub_start = pipeline
        .find("fn wss_subscribed_topics")
        .expect("wss_subscribed_topics");
    let subscribed = &pipeline[sub_start..sub_start + 900];
    let v4swap_pos = subscribed
        .find("t.v4swap")
        .expect("v4swap must be in wss_subscribed_topics");
    let v4_modify_pos = subscribed
        .find("t.v4_modify")
        .expect("v4_modify must be in wss_subscribed_topics");
    assert!(
        v4swap_pos < v4_modify_pos,
        "v4_modify must be in the live eth_subscribe topics filter after v4swap"
    );
    // The decode+apply branch is wired in the log updater.
    assert!(
        pipeline.contains("is_v4_modify && is_v3"),
        "apply_log_value must have the ModifyLiquidity delta-apply branch"
    );
    assert!(
        pipeline.contains("engine_state::decode_v4_modify_liquidity")
            && pipeline.contains("apply_modify_liquidity"),
        "the branch must decode then delta-apply the ModifyLiquidity event"
    );
}

#[test]
fn v4_freshness_is_measurable_and_proof_exists() {
    let pipeline = read(PIPELINE);
    // The shadow monitor scores V4 (mirror) vs REVM (truth) — the AWS-side promotion measurement.
    assert!(
        pipeline.contains("fn shadow_fastlane_compare"),
        "shadow_fastlane_compare must exist to measure V4 promotability"
    );
    // The mechanism-level live proof is external; the laptop must not silently flip the singleton
    // fast lane on without that evidence.
    assert!(
        pipeline.contains("FASTLANE_SINGLETON_LIVE_PROVEN"),
        "singleton fast-lane promotion must remain gated by FASTLANE_SINGLETON_LIVE_PROVEN"
    );
    assert!(
        pipeline.contains("unwrap_or(false)") || pipeline.contains("== Some(\"1\")"),
        "singleton live gate must default closed locally"
    );
}
