//! CROSS-DEX COMPLETENESS MATRIX — proves every in-scope DEX family can participate in a cross-dex
//! arbitrage route end-to-end, and that the excluded families are safely dead.
//!
//! The engine builds routes family-agnostically (the token graph), encodes them with a per-hop
//! `TradeHopV2[]` (exchangeId) mixed calldata, and the executor dispatches each hop by exchangeId.
//! So ANY mixed-family cycle (e.g. buy UniV3 → sell CurveCrypto → settle Balancer) works IFF every
//! family clears all four layers. This gate asserts that, per family:
//!   1. ENUMERATION  — admitted by `is_phase1_supported` (engine-routes), so routes through it are built.
//!   2. DETECTION    — priced by `quote_one_hop` (a non-`None` arm), so cross-dex cycles get a quote.
//!   3. CALLDATA     — mapped to a Mantis exchange_id by `PoolKind::to_mantis_exchange_id`.
//!   4. EXECUTION    — the on-chain executor defines that exact id constant AND dispatches it.
//! It also cross-checks the engine's exchange_id == the executor's constant value (catches id drift
//! between off-chain and on-chain — a class of bug that silently routes to the wrong venue).
//!
//! Excluded per operator directive (Kyber, Maverick) + by version (Fluid v2/D3/D4, a sub-version of
//! the in-scope FluidDex v1). For the two that ARE enumerated but must not trade (Kyber/Maverick),
//! it asserts detection gating so they can never produce a tradeable quote. (Trader Joe LB was
//! promoted into IN_SCOPE — it clears all four layers and is now admitted by is_phase1_supported.)

use std::{fs, path::Path};

const ENUMERATOR: &str = "../engine-routes/src/enumerator.rs";
const POOL: &str = "../engine-state/src/pool.rs";
const PIPELINE: &str = "src/pipeline.rs";
const STARTUP: &str = "src/startup.rs";
const EXECUTOR: &str = "../../contracts/src/MantisExecutor.sol";
const CALLDATA: &str = "../engine-routes/src/calldata.rs";

/// In-scope families: (PoolKind, engine exchange_id, executor id-constant). All must trade cross-dex.
const IN_SCOPE: &[(&str, u8, &str)] = &[
    ("UniswapV2", 24, "UNISWAP_V2_ID"),
    ("SushiV2", 2, "SUSHI_ID"),
    ("CamelotV2", 1, "CAMELOT_ID"),
    ("UniswapV3", 0, "UNISWAP_V3_ID"),
    ("SushiV3", 25, "SUSHISWAP_V3_ID"),
    ("PancakeV3", 6, "PANCAKE_V3_ID"),
    ("AlgebraV19", 9, "CAMELOT_V3_ID"),
    ("UniswapV4", 20, "UNISWAP_V4_ID"),
    ("CurveStableSwapNg", 18, "CURVE_STABLESWAP_PLAIN_NG_ID"),
    ("CurveCryptoSwap", 17, "CURVE_TWOCRYPTO_NG_ID"),
    ("BalancerStable", 26, "BALANCER_STABLE_ID"),
    ("BalancerWeighted", 27, "BALANCER_WEIGHTED_ID"),
    ("DodoV2", 23, "DODO_V2_ID"),
    // SolidlyV3 is intentionally NOT in-scope: it is FAIL-CLOSED at the calldata layer
    // (to_mantis_exchange_id -> None; its only executor mapping id 12 = Ramses-CL is a wrong-route
    // for a Solidly-stable quote). It is quote/rank-only, never submittable. See the fail-closed
    // assertion in `excluded_families_are_enumerated_but_detection_gated`. (AUD-009)
    ("FluidDex", 21, "FLUID_DEX_ID"),
    ("TraderJoeLb", 15, "TRADER_JOE_LB_ID"),
];

fn read(p: &str) -> String {
    fs::read_to_string(Path::new(p)).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// Slice from `fn <name>` to the next top-level `\nfn ` (or end) — the function body.
fn fn_body(src: &str, name: &str) -> String {
    let start = src
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("fn {name} not found"));
    let rest = &src[start..];
    let end = rest[3..].find("\nfn ").map(|i| i + 3).unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Slice the `is_phase1_supported` matches! block.
fn enumerated_block(enumerator: &str) -> String {
    let s = enumerator
        .find("fn is_phase1_supported")
        .expect("is_phase1_supported");
    enumerator[s..s + 800].to_string()
}

#[test]
fn every_in_scope_family_clears_all_four_cross_dex_layers() {
    let enumerator = read(ENUMERATOR);
    let enum_block = enumerated_block(&enumerator);
    let pool = read(POOL);
    let pipeline = read(PIPELINE);
    let quote = fn_body(&pipeline, "quote_one_hop");
    let executor = read(EXECUTOR);

    for (kind, id, exec_const) in IN_SCOPE {
        let pk = format!("PoolKind::{kind}");

        // 1. ENUMERATION
        assert!(
            enum_block.contains(&pk),
            "{kind}: not admitted by is_phase1_supported — cross-dex routes through it are never built"
        );

        // 2. DETECTION — priced (appears in quote_one_hop) and NOT a `=> None` arm.
        assert!(
            quote.contains(&pk),
            "{kind}: no quote_one_hop arm — cross-dex cycles through it get no quote"
        );
        assert!(
            !quote.contains(&format!("{pk} => None")),
            "{kind}: quote_one_hop returns None — it cannot be detected/traded"
        );

        // 3. CALLDATA — mapped to the exchange_id.
        assert!(
            pool.contains(&format!("{pk} => {id}")),
            "{kind}: to_mantis_exchange_id has no `{pk} => {id}` mapping"
        );

        // 4. EXECUTION — executor defines the id constant with the SAME value and dispatches it.
        assert!(
            executor.contains(&format!("{exec_const} = {id};")),
            "{kind}: executor missing constant `{exec_const} = {id};` (engine/executor id drift?)"
        );
        assert!(
            executor.matches(exec_const).count() >= 2,
            "{kind}: executor defines `{exec_const}` but never dispatches it (define + >=1 use expected)"
        );
    }
}

#[test]
fn excluded_families_are_enumerated_but_detection_gated() {
    let enumerator = read(ENUMERATOR);
    let enum_block = enumerated_block(&enumerator);
    let pipeline = read(PIPELINE);
    let quote = fn_body(&pipeline, "quote_one_hop");
    let maverick_gate = fn_body(&pipeline, "maverick_enabled");

    // Kyber & Maverick ARE admitted to enumeration (the containment is detection, not exclusion)...
    for v in ["KyberSwapElastic", "MaverickV2"] {
        assert!(
            enum_block.contains(&format!("PoolKind::{v}")),
            "{v} expected in is_phase1_supported (containment is at detection)"
        );
    }
    // ...and remain gated at DETECTION — since WI-02 through the ONE boot-sealed
    // RuntimePolicy: quote_one_hop consults the sealed dense family array (no env read on
    // the hot path), and the RESOLUTION owns the evidence gates (Kyber: explicit
    // KYBER_ENABLE opt-in — the 2026-07-02 default-on drift is corrected; Maverick:
    // MAVERICK_ENABLE AND in-code live-vector readiness).
    assert!(
        quote.contains(".family_active(meta.kind)"),
        "quote_one_hop must consult the boot-sealed RuntimePolicy dense family array \
         (WI-02 boundary 5) — the detection containment gate"
    );
    let policy_src = read("../engine/src/runtime_policy.rs");
    assert!(
        policy_src.contains("KYBER_ENABLE"),
        "the RuntimePolicy resolution must own the Kyber explicit-opt-in gate (KYBER_ENABLE)"
    );
    assert!(
        policy_src.contains("MAVERICK_ENABLE")
            && policy_src.contains("live_reference_vectors_ready()"),
        "the RuntimePolicy resolution must gate Maverick on MAVERICK_ENABLE AND reviewed \
         live-vector readiness"
    );
    assert!(
        maverick_gate.contains("live_reference_vectors_ready()"),
        "Maverick readiness gate must require reviewed live-vector readiness, not MAVERICK_ENABLE alone"
    );

    let startup = read(STARTUP);
    let maverick_startup = startup
        .find("PoolKind::MaverickV2")
        .and_then(|start| startup.get(start..start + 1800))
        .expect("startup Maverick branch");
    assert!(
        maverick_startup.contains("live_reference_vectors_ready()"),
        "Maverick startup discovery must require reviewed live-vector readiness"
    );
    assert!(
        maverick_startup.contains("requested && parity_ready"),
        "Maverick startup discovery must require both MAVERICK_ENABLE and parity readiness"
    );

    // SolidlyV3 is FAIL-CLOSED at the calldata layer (AUD-009): to_mantis_exchange_id returns None,
    // so a SolidlyV3 route is never submittable (quote/rank-only). Lock that guard.
    let pool = read(POOL);
    assert!(
        pool.contains("matches!(self, PoolKind::SolidlyV3)"),
        "SolidlyV3 must remain fail-closed (matches!(self, PoolKind::SolidlyV3) -> return None) in engine-state/pool.rs"
    );
}

#[test]
fn mixed_family_routing_is_structurally_wired() {
    // Calldata: the family-agnostic mixed encoder (per-hop exchangeId) must exist for 3- and 4-hop.
    let calldata = read(CALLDATA);
    let executor = read(EXECUTOR);
    assert!(
        calldata.contains("MANTIS_MIXED"),
        "mixed-family calldata encoder (MANTIS_MIXED*) missing — exotic cross-dex routes can't be encoded"
    );
    // Executor: the per-hop dispatcher that routes each TradeHopV2 by exchangeId, used by the
    // multi-hop mixed path — this is what makes a cycle mixing arbitrary families executable.
    for marker in ["_swapExactInHopV2", "_executeTradeHopsV2_4"] {
        assert!(
            executor.contains(marker),
            "executor missing `{marker}` — mixed-family multi-hop routes are not executable"
        );
    }
}
