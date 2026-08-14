//! WI-02 — early immutable `RuntimePolicy` + all six family boundaries (codex plan §5 WI-02).
//!
//! RED contract: every test here compiles and fails at an ASSERTION against the permissive
//! WI-02 stub (`engine::runtime_policy`), never at compilation. The six boundaries are
//! (1) config/discovery, (2) worker construction, (3) universe/state stores, (4) route graph,
//! (5) prediction/simulation, (6) submission/encoding.
//!
//! Hermetic: every resolution uses an injected env reader (no process-env mutation), so tests
//! are parallel-safe and the golden matrix cannot be perturbed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use engine::pipeline::{campaign_cycle_walk, DetectionContext};
use engine::runtime_policy::{
    self, LiveSubmitAuthorityMode, PolicyRefusal, RevmValidationMode, RuntimePolicy,
};
use engine_routes::{CsrBuilder, Hop, Route, RouteTable, StrategyKind};
use engine_state::{
    PoolId, PoolKind, RiskClass, RouteId, TokenId, UniverseBuilder, V2Reserves256, V2State,
    V3State, VenueId,
};
use ruint::aliases::U256;

// ───────────────────────────── helpers ─────────────────────────────

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// Read one live source file (for the boundary consult-site scans).
fn source_of(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Injected env reader over a fixed set of pairs — the hermetic resolution seam.
fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| (*v).to_string())
    }
}

/// Parity proof that must NOT be consulted (non-authoritative resolutions).
fn proof_unconsulted() -> std::result::Result<
    [bool; engine::fastlane_policy::N_FAMILIES],
    engine::fastlane_policy::ParityProofError,
> {
    panic!("strict parity proof must not be consulted for a non-authoritative resolution")
}

fn resolve(pairs: &[(&str, &str)]) -> Result<RuntimePolicy, PolicyRefusal> {
    runtime_policy::resolve_runtime_policy_with(&env_of(pairs), proof_unconsulted)
}

/// A live/capital env with EVERY required policy field explicitly set (the complete base the
/// unset-field matrix subtracts from).
const LIVE_COMPLETE: &[(&str, &str)] = &[
    ("DRY_RUN", "false"),
    ("SUBMIT_TX", "true"),
    ("LIVE_SUBMIT", "1"),
    ("ACTIVE_FAMILIES", "uniswap_v3,pancake_v3"),
    ("PROVEN_FAMILIES", "uniswap_v3,pancake_v3"),
    ("ACTIVE_ROUTE_SHAPES", "two_hop,triangle"),
    ("REVM_VALIDATION_MODE", "sampled_shadow"),
    ("SHADOW_STATE_DETECTION", "observe"),
    ("LIVE_SUBMIT_AUTHORITY", "live_explicit"),
];

/// LIVE_COMPLETE with one key replaced (family-request variants).
fn live_with_active(active: &str) -> Vec<(&'static str, String)> {
    LIVE_COMPLETE
        .iter()
        .map(|(k, v)| {
            if *k == "ACTIVE_FAMILIES" {
                (*k, active.to_string())
            } else {
                (*k, (*v).to_string())
            }
        })
        .collect()
}

fn resolve_owned(pairs: &[(&'static str, String)]) -> Result<RuntimePolicy, PolicyRefusal> {
    let var = move |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
    };
    runtime_policy::resolve_runtime_policy_with(&var, proof_unconsulted)
}

/// A live request for `family` must be a TYPED per-family activation refusal.
fn assert_live_family_refused(family_token: &str) {
    let pairs = live_with_active(&format!("uniswap_v3,pancake_v3,{family_token}"));
    let verdict = resolve_owned(&pairs);
    match verdict {
        Err(PolicyRefusal::FamilyActivationRefused { family, .. }) => {
            assert_eq!(
                family, family_token,
                "the live refusal must name the refused family"
            );
        }
        other => panic!(
            "a live/capital profile requesting `{family_token}` must refuse boot with a typed \
             FamilyActivationRefused, got {other:?} — the capital policy is U3+Pancake only"
        ),
    }
}

/// Deep single-range V3 state with a warmed tick window (mirrors the WI-01 matrix fixture).
fn v3_state(tick: i32) -> V3State {
    V3State {
        sqrt_price_x96: engine_amm::sqrt_price_at_tick(tick).expect("tick in range"),
        tick,
        liquidity: 1_000_000_000_000_000_000_000_000,
        fee_pips: 100,
        tick_spacing: 1,
        initialised_ticks: vec![
            engine_amm::TickInfo {
                tick: tick - 100_000,
                liquidity_net: 1_000_000_000_000_000_000_000_000,
            },
            engine_amm::TickInfo {
                tick: tick + 100_000,
                liquidity_net: -1_000_000_000_000_000_000_000_000,
            },
        ],
    }
}

fn v2_state(reserve0: u128, reserve1: u128) -> V2State {
    V2State {
        reserves: V2Reserves256 {
            reserve0,
            reserve1,
            block_timestamp_last: 0,
        },
    }
}

/// A minimal mixed-family pool table: one U3 pair, one Pancake pair, one SushiV2 pair, one
/// Kyber pair — enough to observe family gating at the route boundary.
fn mixed_pool_table() -> (engine_state::Universe, Vec<(PoolKind, PoolId, PoolId)>) {
    let mut b = UniverseBuilder::new();
    let q96 = |num: u128, den: u128| (U256::from(num) << 96) / U256::from(den);
    let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
    b.set_token_eth_price_q96(weth, q96(1, 1));
    let c0 = b.add_token([0xC0; 20], 18, RiskClass::Permissionless);
    b.set_token_eth_price_q96(c0, q96(1, 1));
    let mut fams = Vec::new();
    let mut addr = 0x10u8;
    for kind in [
        PoolKind::UniswapV3,
        PoolKind::PancakeV3,
        PoolKind::KyberSwapElastic,
    ] {
        let base = b
            .add_v3_pool([addr; 20], kind, weth, c0, VenueId(addr as u16), 100, 1, v3_state(0))
            .expect("v3 base");
        let rich = b
            .add_v3_pool(
                [addr + 1; 20],
                kind,
                weth,
                c0,
                VenueId(addr as u16 + 1),
                100,
                1,
                v3_state(200),
            )
            .expect("v3 rich");
        fams.push((kind, base, rich));
        addr += 4;
    }
    let s_base = b
        .add_v2_pool(
            [0x30; 20],
            PoolKind::SushiV2,
            weth,
            c0,
            VenueId(0x30),
            30,
            v2_state(1_000_000_000_000_000_000_000, 1_000_000_000_000_000_000_000),
        )
        .expect("sushi base");
    let s_rich = b
        .add_v2_pool(
            [0x31; 20],
            PoolKind::SushiV2,
            weth,
            c0,
            VenueId(0x31),
            30,
            v2_state(1_000_000_000_000_000_000_000, 1_100_000_000_000_000_000_000),
        )
        .expect("sushi rich");
    fams.push((PoolKind::SushiV2, s_base, s_rich));
    (b.build(), fams)
}

fn kinds_of_routes(universe: &engine_state::Universe, rt: &RouteTable) -> Vec<PoolKind> {
    let mut kinds = Vec::new();
    for r in rt.all() {
        for h in r.hops.iter().take(r.hop_count as usize) {
            kinds.push(universe.pools.meta(h.pool).kind);
        }
    }
    kinds.sort_by_key(|k| *k as u8);
    kinds.dedup();
    kinds
}

// ───────────────────────────── the 8 core WI-02 tests ─────────────────────────────

/// Unset `ACTIVE_FAMILIES` = the development default = all currently-supported families:
/// byte-identical structural behavior. The default set is all 18 minus the two families whose
/// activation is evidence-gated opt-in TODAY (Maverick: flag+vectors; Kyber: the WI-01-found
/// default-activation drift, corrected here as the reviewed candidate→typed-decline entry).
#[test]
fn active_families_default_all_preserves_full_route_graph() {
    let policy = resolve(&[]).expect("dev default resolves");
    // PRIMARY (RED): the default active set is the 16 currently-supported families — Kyber
    // must NOT silently default active (the WI-01 drift), Maverick stays vectors-gated.
    assert!(
        !policy.active_families.contains(PoolKind::KyberSwapElastic),
        "Kyber silently defaulted ACTIVE — the 2026-07-02 default-on drift must be corrected \
         (explicit KYBER_ENABLE required)"
    );
    assert!(
        !policy.active_families.contains(PoolKind::MaverickV2),
        "Maverick must stay inactive by default (flag + live reference vectors required)"
    );
    assert_eq!(
        policy.active_families.count(),
        16,
        "dev default = all currently-supported families (18 minus Kyber/Maverick)"
    );
    // Every non-opt-in family IS active — the byte-identical guarantee.
    for k in engine::fastlane_policy::ALL_FAMILIES {
        if !matches!(k, PoolKind::KyberSwapElastic | PoolKind::MaverickV2) {
            assert!(
                policy.active_families.contains(k),
                "family {k:?} must be active in the unset-default profile (byte-identical)"
            );
        }
    }
    assert_eq!(
        policy.active_families,
        runtime_policy::default_active_families(),
        "resolution and default_active_families() must agree"
    );
    // Route-graph preservation: gated enumeration under the default mask keeps every route of
    // every default-active family and drops only opt-in-family routes.
    let (universe, _) = mixed_pool_table();
    let n = universe.pools.len();
    let dense = policy.active_families.dense();
    let eligible: Vec<bool> = (0..n)
        .map(|i| {
            dense[engine::fastlane_policy::family_index(
                universe.pools.meta(PoolId(i as u32)).kind,
            )]
        })
        .collect();
    let mut gated = RouteTable::new();
    engine_routes::enumerate_two_hop_routes_gated(&universe.pools, &eligible, &mut gated);
    let gated_kinds = kinds_of_routes(&universe, &gated);
    assert!(
        !gated_kinds.contains(&PoolKind::KyberSwapElastic),
        "default-mask gated enumeration must not build routes over the inactive Kyber family"
    );
    for k in [PoolKind::UniswapV3, PoolKind::PancakeV3, PoolKind::SushiV2] {
        assert!(
            gated_kinds.contains(&k),
            "default-mask gated enumeration lost active-family {k:?} routes (route graph \
             not preserved)"
        );
    }
    let mut ungated = RouteTable::new();
    engine_routes::enumerate_two_hop_routes(&universe.pools, &mut ungated);
    let all_true = vec![true; n];
    let mut gated_all = RouteTable::new();
    engine_routes::enumerate_two_hop_routes_gated(&universe.pools, &all_true, &mut gated_all);
    assert_eq!(
        gated_all.len(),
        ungated.len(),
        "an all-true mask must be byte-identical to the ungated enumerator"
    );
}

/// One inactive family is ABSENT at all six boundaries.
#[test]
fn inactive_family_is_absent_at_config_discovery_workers_universe_routes_prediction_simulation_and_submission()
{
    let policy = resolve(&[("ACTIVE_FAMILIES", "uniswap_v3")]).expect("explicit dev profile");
    // PRIMARY (RED): the explicit list is the ENTIRE active set.
    assert_eq!(
        policy.active_families.count(),
        1,
        "ACTIVE_FAMILIES=uniswap_v3 must activate exactly one family, got {:?}",
        policy.active_families.tokens()
    );
    assert!(policy.active_families.contains(PoolKind::UniswapV3));
    // Boundary predicates for an INACTIVE family (SushiV2 here) must all refuse.
    let inactive = PoolKind::SushiV2;
    assert!(
        !runtime_policy::family_admitted_at_discovery(&policy, inactive),
        "boundary 1 (config/discovery) admits an inactive family"
    );
    assert!(
        !runtime_policy::fluid_workers_allowed(&policy),
        "boundary 2 (workers): the Fluid pollers must not start when FluidDex is inactive"
    );
    assert!(
        !runtime_policy::pool_admitted_to_universe(&policy, inactive),
        "boundary 3 (universe/state stores) admits an inactive family"
    );
    assert!(
        !runtime_policy::route_pool_eligible(&policy, inactive, true, true),
        "boundary 4 (route graph) admits an inactive family"
    );
    // Boundary 4, functionally: gated enumeration over a mixed table with only U3 eligible.
    let (universe, _) = mixed_pool_table();
    let n = universe.pools.len();
    let eligible: Vec<bool> = (0..n)
        .map(|i| universe.pools.meta(PoolId(i as u32)).kind == PoolKind::UniswapV3)
        .collect();
    let mut gated = RouteTable::new();
    engine_routes::enumerate_two_hop_routes_gated(&universe.pools, &eligible, &mut gated);
    assert_eq!(
        kinds_of_routes(&universe, &gated),
        vec![PoolKind::UniswapV3],
        "gated route enumeration must build routes over ONLY the active family"
    );
    // Boundaries 1/3/4 consult sites exist in the REAL startup path.
    let startup = source_of("engine/crates/engine/src/startup.rs");
    assert!(
        startup.contains("family_admitted_at_discovery("),
        "startup.rs venue-discovery loop does not consult family_admitted_at_discovery — \
         boundary 1 is not wired"
    );
    assert!(
        startup.contains("pool_admitted_to_universe("),
        "startup.rs universe registration does not consult pool_admitted_to_universe — \
         boundary 3 is not wired"
    );
    assert!(
        startup.contains("route_pool_eligible("),
        "startup.rs route enumeration does not consult route_pool_eligible — boundary 4 is \
         not wired"
    );
    // Boundary 2 consult site (fluid poller spawn).
    let main_src = source_of("engine/crates/engine/src/main.rs");
    assert!(
        main_src.contains("fluid_workers_allowed("),
        "main.rs fluid_poller spawn does not consult fluid_workers_allowed — boundary 2 is \
         not wired"
    );
    // Boundary 5 (prediction/simulation): the per-hop quote dispatch consults the
    // boot-built dense family array on the DetectionContext.
    let pipeline = source_of("engine/crates/engine/src/pipeline.rs");
    assert!(
        pipeline.contains(".family_active(meta.kind)"),
        "pipeline.rs quote_one_hop does not consult the DetectionContext dense \
         family_active array — boundary 5 is not wired"
    );
    // Boundary 6 (submission/encoding).
    let submit = source_of("engine/crates/engine/src/mantis_submit.rs");
    assert!(
        submit.contains(".family_active(meta.kind)"),
        "mantis_submit.rs build_mantis_calldata does not consult the dense family_active \
         array — boundary 6 is not wired"
    );
}

/// `proven ⊆ active` always (equality legal when every active family is proven).
#[test]
fn fastlane_proven_families_is_a_subset_of_active_families() {
    let policy = resolve(&[("ACTIVE_FAMILIES", "uniswap_v3")]).expect("explicit dev profile");
    // PRIMARY (RED): active is exactly {U3}; proven must be demoted into it.
    assert_eq!(
        policy.active_families.tokens(),
        vec!["uniswap_v3"],
        "explicit ACTIVE_FAMILIES must BE the active set"
    );
    assert!(
        policy.proven_families.is_subset_of(&policy.active_families),
        "proven {:?} ⊄ active {:?}",
        policy.proven_families.tokens(),
        policy.active_families.tokens()
    );
    assert!(
        policy.proven_families.contains(PoolKind::UniswapV3),
        "U3 is byte-exact + executor-bound + active: it must stay proven"
    );
    // Equality is legal when every active family is proven.
    let policy = resolve(&[("ACTIVE_FAMILIES", "uniswap_v3,pancake_v3")]).expect("dev profile");
    assert!(policy.proven_families.is_subset_of(&policy.active_families));
    // Default profile: subset holds with the derived sets too.
    let policy = resolve(&[]).expect("dev default");
    assert!(policy.proven_families.is_subset_of(&policy.active_families));
    assert!(policy.proven_families.count() >= 1, "default proven set must be non-empty");
}

/// P1-08: pricing anchors are rebuilt active-only, and an unpriced token excludes its pools
/// BEFORE routes are built.
#[test]
fn active_family_filter_recertifies_every_token_price_anchor() {
    let policy = resolve(&[("ACTIVE_FAMILIES", "uniswap_v3")]).expect("explicit dev profile");
    // PRIMARY (RED): an inactive family may not contribute a token price anchor.
    assert!(
        !runtime_policy::anchor_pool_admitted(&policy, PoolKind::CurveStableSwapNg),
        "an INACTIVE family's pools still contribute token price anchors (P1-08 open)"
    );
    assert!(
        runtime_policy::anchor_pool_admitted(&policy, PoolKind::UniswapV3),
        "an ACTIVE family's pools must contribute anchors"
    );
    // An unpriced token excludes the pool from route enumeration (fail-closed sizing).
    assert!(
        !runtime_policy::route_pool_eligible(&policy, PoolKind::UniswapV3, false, true),
        "a pool with an UNPRICED token0 must be excluded before routes are built"
    );
    assert!(
        !runtime_policy::route_pool_eligible(&policy, PoolKind::UniswapV3, true, false),
        "a pool with an UNPRICED token1 must be excluded before routes are built"
    );
    assert!(runtime_policy::route_pool_eligible(&policy, PoolKind::UniswapV3, true, true));
    // The real anchor build consults the boundary.
    let startup = source_of("engine/crates/engine/src/startup.rs");
    assert!(
        startup.contains("anchor_pool_admitted("),
        "startup.rs price-anchor projection does not consult anchor_pool_admitted — the \
         anchor set is not policy-recertified"
    );
}

/// The run manifest binds the policy: active/proven family hashes, route-shape hash,
/// executor exchange-id table hash, and gas-profile table hash.
#[test]
fn run_manifest_binds_active_proven_family_route_shape_executor_and_gas_profile_hashes() {
    let policy = resolve(&[]).expect("dev default");
    let mb = runtime_policy::manifest_binding(&policy);
    // PRIMARY (RED): the binding object exists and carries every required hash.
    let obj = mb.as_object().unwrap_or_else(|| {
        panic!("manifest_binding must be a JSON object binding the policy, got {mb}")
    });
    for key in [
        "active_families",
        "active_families_hash",
        "proven_families",
        "proven_families_hash",
        "route_shapes",
        "route_shapes_hash",
        "executor_exchange_id_hash",
        "gas_profile_hash",
        "record_hash",
        "generation",
    ] {
        assert!(obj.contains_key(key), "manifest binding missing `{key}`");
    }
    for key in [
        "active_families_hash",
        "proven_families_hash",
        "route_shapes_hash",
        "executor_exchange_id_hash",
        "gas_profile_hash",
        "record_hash",
    ] {
        let h = obj[key].as_str().unwrap_or("");
        assert_eq!(h.len(), 64, "`{key}` must be a sha256 hex digest, got `{h}`");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
    assert!(obj["generation"].as_u64().unwrap_or(0) > 0, "generation must be nonzero");
    // The executor + gas tables are content-hashed (distinct inputs ⇒ distinct digests).
    assert_ne!(
        runtime_policy::executor_exchange_id_hash(),
        runtime_policy::gas_profile_hash(),
        "executor and gas-profile hashes must bind DIFFERENT canonical tables"
    );
    // The network run manifest embeds the binding.
    let manifest_src = source_of("engine/crates/engine/src/run_manifest.rs");
    assert!(
        manifest_src.contains("manifest_binding("),
        "run_manifest.rs does not embed runtime_policy::manifest_binding — the run manifest \
         does not bind the policy hashes"
    );
    // Different policies ⇒ different record hashes (the binding is not a constant).
    let p2 = resolve(&[("ACTIVE_FAMILIES", "uniswap_v3")]).expect("dev profile");
    assert_ne!(
        policy.record_hash, p2.record_hash,
        "two different policy records must not share a record_hash"
    );
}

/// Single binary: REVM stays compiled; the manifest declares it plus the runtime validation
/// mode; an unknown mode value refuses boot.
#[test]
fn single_binary_manifest_declares_revm_compiled_and_runtime_validation_mode() {
    let policy = resolve(&[]).expect("dev default");
    let mb = runtime_policy::manifest_binding(&policy);
    // PRIMARY (RED): the manifest declares REVM compiled + the mode.
    assert_eq!(
        mb.get("revm_compiled").and_then(|v| v.as_bool()),
        Some(true),
        "the single-binary manifest must declare revm_compiled=true (REVM is never compiled \
         out), got {mb}"
    );
    assert_eq!(
        mb.get("revm_validation_mode").and_then(|v| v.as_str()),
        Some("sampled_shadow"),
        "the default REVM runtime validation mode must be declared"
    );
    assert_eq!(policy.revm_validation_mode, RevmValidationMode::SampledShadow);
    // Explicit modes parse; unknown values refuse boot (fail-closed).
    let policy = resolve(&[("REVM_VALIDATION_MODE", "off")]).expect("explicit off");
    assert_eq!(policy.revm_validation_mode, RevmValidationMode::Off);
    let verdict = resolve(&[("REVM_VALIDATION_MODE", "garbage")]);
    assert!(
        matches!(verdict, Err(PolicyRefusal::UnknownModeValue { field: "REVM_VALIDATION_MODE", .. })),
        "an unknown REVM_VALIDATION_MODE must refuse boot, got {verdict:?}"
    );
}

/// A live/capital profile must set EVERY policy field explicitly; one missing field refuses
/// boot.
#[test]
fn live_profile_rejects_unset_active_proven_route_shape_and_authority_scope() {
    // The complete live profile resolves (positive control).
    let complete: Vec<(&'static str, String)> = LIVE_COMPLETE
        .iter()
        .map(|(k, v)| (*k, (*v).to_string()))
        .collect();
    let policy = resolve_owned(&complete).expect("a COMPLETE live profile must resolve");
    assert_eq!(policy.live_submit_authority, LiveSubmitAuthorityMode::LiveExplicit);
    // PRIMARY (RED): dropping any ONE required field refuses boot with the field named.
    for missing in [
        "ACTIVE_FAMILIES",
        "PROVEN_FAMILIES",
        "ACTIVE_ROUTE_SHAPES",
        "REVM_VALIDATION_MODE",
        "SHADOW_STATE_DETECTION",
        "LIVE_SUBMIT_AUTHORITY",
    ] {
        let partial: Vec<(&'static str, String)> = LIVE_COMPLETE
            .iter()
            .filter(|(k, _)| *k != missing)
            .map(|(k, v)| (*k, (*v).to_string()))
            .collect();
        let verdict = resolve_owned(&partial);
        match verdict {
            Err(PolicyRefusal::LiveFieldUnset { field }) => assert_eq!(
                field, missing,
                "the refusal must name the unset field"
            ),
            other => panic!(
                "a live profile with `{missing}` UNSET must refuse boot (LiveFieldUnset), \
                 got {other:?}"
            ),
        }
    }
    // The capital scope itself is enforced: a live profile beyond U3+Pancake refuses.
    assert_live_family_refused("uniswap_v2");
}

/// The capital profile (U3 + Pancake V3 only) preserves every supported detection type.
#[test]
fn active_families_u3_pancake_preserve_all_supported_detection_types() {
    let policy =
        resolve(&[("ACTIVE_FAMILIES", "uniswap_v3,pancake_v3")]).expect("capital family set");
    // PRIMARY (RED): exactly the two capital families are active.
    assert_eq!(
        policy.active_families.tokens(),
        vec!["uniswap_v3", "pancake_v3"],
        "the capital profile must activate exactly U3+Pancake"
    );
    assert!(policy.proven_families.is_subset_of(&policy.active_families));
    assert!(
        !runtime_policy::fluid_workers_allowed(&policy),
        "no Fluid workers under the capital profile"
    );
    assert!(!runtime_policy::family_admitted_at_discovery(&policy, PoolKind::SushiV2));
    // Route boundary: a mixed table yields ONLY U3/Pancake routes under the capital mask.
    let (universe, _) = mixed_pool_table();
    let n = universe.pools.len();
    let dense = policy.active_families.dense();
    let eligible: Vec<bool> = (0..n)
        .map(|i| {
            dense[engine::fastlane_policy::family_index(
                universe.pools.meta(PoolId(i as u32)).kind,
            )]
        })
        .collect();
    let mut gated = RouteTable::new();
    engine_routes::enumerate_two_hop_routes_gated(&universe.pools, &eligible, &mut gated);
    assert_eq!(
        kinds_of_routes(&universe, &gated),
        vec![PoolKind::UniswapV3, PoolKind::PancakeV3],
        "capital-mask enumeration must contain exactly U3+Pancake routes"
    );
    // Every supported StrategyKind stays detectable with U3+Pancake pools alone: build the
    // capital fixture and walk each shape via the REAL engine decision.
    let (detection, specs) = capital_u3_pancake_fixture();
    let mut positive_by_kind: BTreeMap<&'static str, bool> = BTreeMap::new();
    for (strategy, route_id, amount) in &specs {
        let out = campaign_cycle_walk(&detection, *route_id, *amount);
        let name = match strategy {
            StrategyKind::TwoHop => "TwoHop",
            StrategyKind::Triangle => "Triangle",
            StrategyKind::FourHop => "FourHop",
            StrategyKind::FiveHop => "FiveHop",
            StrategyKind::SixHop => "SixHop",
            StrategyKind::SevenHop => "SevenHop",
            StrategyKind::EightHop => "EightHop",
            StrategyKind::SelfImpact => "SelfImpact",
        };
        let positive = matches!(out, Some(o) if o > *amount);
        *positive_by_kind.entry(name).or_insert(false) |= positive;
    }
    for kind in [
        "TwoHop", "Triangle", "FourHop", "FiveHop", "SixHop", "SevenHop", "EightHop",
        "SelfImpact",
    ] {
        assert_eq!(
            positive_by_kind.get(kind),
            Some(&true),
            "strategy `{kind}` lost its positive detection under the U3+Pancake capital \
             profile"
        );
    }
}

/// U3+Pancake-only fixture covering all 8 supported strategy kinds (V3-state pools only).
fn capital_u3_pancake_fixture() -> (Arc<DetectionContext>, Vec<(StrategyKind, RouteId, u128)>) {
    let mut b = UniverseBuilder::new();
    let q96 = |num: u128, den: u128| (U256::from(num) << 96) / U256::from(den);
    let weth = b.add_token([0x82; 20], 18, RiskClass::Permissionless);
    b.set_token_eth_price_q96(weth, q96(1, 1));
    let chain: Vec<TokenId> = (0..7u8)
        .map(|i| {
            let t = b.add_token([0xC0 + i; 20], 18, RiskClass::Permissionless);
            b.set_token_eth_price_q96(t, q96(1, 1));
            t
        })
        .collect();
    // U3 dislocated pair (weth, c0) + Pancake dislocated pair (c0, c1).
    let u3_base = b
        .add_v3_pool([0x10; 20], PoolKind::UniswapV3, weth, chain[0], VenueId(0), 100, 1, v3_state(0))
        .expect("u3 base");
    let u3_rich = b
        .add_v3_pool([0x11; 20], PoolKind::UniswapV3, weth, chain[0], VenueId(1), 100, 1, v3_state(200))
        .expect("u3 rich");
    let pk_base = b
        .add_v3_pool([0x14; 20], PoolKind::PancakeV3, chain[0], chain[1], VenueId(2), 100, 1, v3_state(0))
        .expect("pancake base");
    let pk_rich = b
        .add_v3_pool([0x15; 20], PoolKind::PancakeV3, chain[0], chain[1], VenueId(3), 100, 1, v3_state(200))
        .expect("pancake rich");
    // Triangle: U3 rich opener (weth,c0), Pancake balanced (c0,c1), U3 balanced closer (c1,weth).
    let tri_mid = b
        .add_v3_pool([0x18; 20], PoolKind::PancakeV3, chain[0], chain[1], VenueId(4), 100, 1, v3_state(0))
        .expect("tri mid");
    let tri_close = b
        .add_v3_pool([0x19; 20], PoolKind::UniswapV3, chain[1], weth, VenueId(5), 100, 1, v3_state(0))
        .expect("tri close");
    // Deep chain: rich U3 edge (weth,c0), balanced alternating U3/Pancake links (ci,ci+1),
    // balanced closers (ck,weth).
    let edge = b
        .add_v3_pool([0x20; 20], PoolKind::UniswapV3, weth, chain[0], VenueId(6), 100, 1, v3_state(200))
        .expect("deep edge");
    let mut link = Vec::new();
    for i in 0..6usize {
        let kind = if i % 2 == 0 { PoolKind::PancakeV3 } else { PoolKind::UniswapV3 };
        link.push(
            b.add_v3_pool(
                [0x21 + i as u8; 20],
                kind,
                chain[i],
                chain[i + 1],
                VenueId(7 + i as u16),
                100,
                1,
                v3_state(0),
            )
            .expect("deep link"),
        );
    }
    let mut close = BTreeMap::new();
    for k in 1..=6usize {
        close.insert(
            k,
            b.add_v3_pool(
                [0x30 + k as u8; 20],
                PoolKind::PancakeV3,
                chain[k],
                weth,
                VenueId(13 + k as u16),
                100,
                1,
                v3_state(0),
            )
            .expect("deep close"),
        );
    }
    let universe = b.build();
    let eth = 1_000_000_000_000_000_000u128;
    let mut protos: Vec<(StrategyKind, Vec<(PoolId, u8)>, TokenId, u128)> = vec![
        (StrategyKind::TwoHop, vec![(u3_rich, 0), (u3_base, 1)], weth, eth),
        (StrategyKind::TwoHop, vec![(pk_rich, 0), (pk_base, 1)], chain[0], eth),
        (
            StrategyKind::Triangle,
            vec![(u3_rich, 0), (tri_mid, 0), (tri_close, 0)],
            weth,
            eth,
        ),
        (StrategyKind::SelfImpact, vec![(u3_rich, 0), (u3_base, 1)], weth, eth),
    ];
    for depth in 4..=8usize {
        let mut hops = vec![(edge, 0u8)];
        for i in 0..depth - 2 {
            hops.push((link[i], 0));
        }
        hops.push((close[&(depth - 2)], 0));
        let strategy = match depth {
            4 => StrategyKind::FourHop,
            5 => StrategyKind::FiveHop,
            6 => StrategyKind::SixHop,
            7 => StrategyKind::SevenHop,
            _ => StrategyKind::EightHop,
        };
        protos.push((strategy, hops, weth, eth));
    }
    let mut route_table = RouteTable::new();
    let mut specs = Vec::new();
    for (idx, (strategy, hops, start_token, amount)) in protos.into_iter().enumerate() {
        let mut hop_vec = arrayvec::ArrayVec::<Hop, { engine_routes::MAX_HOPS }>::new();
        for (pool, dir) in &hops {
            hop_vec.push(Hop::new(*pool, *dir));
        }
        let mut tokens = arrayvec::ArrayVec::<TokenId, { engine_routes::MAX_HOPS + 1 }>::new();
        let mut current = start_token;
        tokens.push(current);
        for (pool, dir) in &hops {
            let meta = universe.pools.meta(*pool);
            current = if *dir == 0 { meta.token1 } else { meta.token0 };
            tokens.push(current);
        }
        assert_eq!(tokens[0], current, "capital fixture spec {idx} must close its cycle");
        let route_id = route_table.insert(Route {
            id: RouteId(idx as u32),
            strategy,
            hop_count: hops.len() as u8,
            hops: hop_vec,
            tokens,
            family_signature: 0,
            calldata_template_id: 0,
            gas_template_id: 0,
            l1_size_bucket_template: 0,
            amount_grid_id: 0,
            risk_flags: 0,
            readiness_mask: 0,
            route_heat: 0,
            hot: true,
        });
        specs.push((strategy, route_id, amount));
    }
    let n_pools = universe.pools.len();
    let mut csr = CsrBuilder::new(n_pools as u32);
    for r in route_table.all() {
        for h in r.hops.iter().take(r.hop_count as usize) {
            csr.push(h.pool.0, r.id);
        }
    }
    let detection = Arc::new(DetectionContext {
        universe: Arc::new(universe),
        route_table,
        pool_to_hot_routes: csr.build(),
        selectors: engine_detect::SelectorRegistry::canonical(),
        telemetry: Arc::new(engine_discovery::PoolTelemetry::new(vec![0; n_pools])),
        family_quote_active: engine::runtime_policy::default_active_families().dense(),
    });
    (detection, specs)
}

// ─────────────────────── the per-family fail-closed activation tests ───────────────────────

/// WI-02b (codex blocker 1): the discovery-CACHE seed is policy-gated BEFORE
/// validation/scoring — an inactive cached pool must never occupy a `hot_pool_cap` slot and
/// displace active pools (the boundary-3 registration gate runs only after truncation).
#[test]
fn discovery_cache_seed_is_policy_gated_before_scoring() {
    let policy = resolve(&[("ACTIVE_FAMILIES", "uniswap_v3")]).expect("explicit dev profile");
    let mk = |i: u8, family: PoolKind| engine_discovery::DiscoveredPool {
        address: [i; 20],
        family,
        token0: [0x82; 20],
        token1: [0xC0; 20],
        fee_pips: 100,
        fee_bps: 30,
        tick_spacing: 1,
        created_block: 1,
    };
    // Many high-position INACTIVE cached pools around a few ACTIVE ones — the displacement
    // shape: without the seed gate these all enter scoring and can consume the hot cap.
    let mut cached = Vec::new();
    for i in 0..40u8 {
        cached.push(mk(i, PoolKind::UniswapV2));
    }
    cached.push(mk(40, PoolKind::UniswapV3));
    cached.push(mk(41, PoolKind::KyberSwapElastic));
    cached.push(mk(42, PoolKind::UniswapV3));
    let (admitted, excluded) = engine::startup::policy_filtered_cache_seed(&policy, &cached);
    // PRIMARY (RED): only ACTIVE-family pools survive the cache seed.
    assert_eq!(
        admitted.len(),
        2,
        "the cache seed admitted {} pools under an active set of exactly uniswap_v3 — \
         inactive cached pools would enter scoring and displace active pools at the hot cap",
        admitted.len()
    );
    assert!(
        admitted.iter().all(|p| p.family == PoolKind::UniswapV3),
        "a non-active family survived the cache seed"
    );
    assert_eq!(excluded, 41, "every inactive cached pool must be counted excluded");
    // Order of the surviving active pools is preserved (no reordering side effects).
    assert_eq!(admitted[0].address, [40u8; 20]);
    assert_eq!(admitted[1].address, [42u8; 20]);
    // All-active cache: byte-identical pass-through.
    let policy_all = resolve(&[]).expect("dev default");
    let v3_only: Vec<engine_discovery::DiscoveredPool> =
        (0..3u8).map(|i| mk(i, PoolKind::UniswapV3)).collect();
    let (admitted, excluded) = engine::startup::policy_filtered_cache_seed(&policy_all, &v3_only);
    assert_eq!(admitted.len(), 3);
    assert_eq!(excluded, 0);
    // The REAL cache-hit path consults this exact seam (not just the venue-scan gate).
    let startup = source_of("engine/crates/engine/src/startup.rs");
    assert!(
        startup.contains("policy_filtered_cache_seed(policy"),
        "startup.rs discovery-cache hit does not route its seed through \
         policy_filtered_cache_seed — boundary 1 is cache-blind"
    );
}

/// WI-02c (codex WI02B_VERIFY blocker 1): the discovery cache is persisted back to disk
/// after the working-view filter, so the cache KEY must be policy-aware — a narrow-family
/// boot's (filtered, head-advanced) cache lineage must never serve or overwrite a broader
/// boot's. A broadened boot must MISS the narrow cache and full-rescan; coverage can never
/// be permanently lost through cache attrition.
#[test]
fn narrow_boot_cache_cannot_serve_or_overwrite_a_broad_boot() {
    let allowlist: ahash::AHashSet<[u8; 20]> = [[0x82u8; 20], [0xC0u8; 20]].into_iter().collect();
    let narrow = resolve(&[("ACTIVE_FAMILIES", "uniswap_v3")]).expect("narrow profile");
    let broad = resolve(&[]).expect("broad default profile");
    let narrow_key = engine::discovery_cache::policy_aware_config_hash(
        &[],
        &allowlist,
        &narrow.active_families.tokens(),
    );
    let broad_key = engine::discovery_cache::policy_aware_config_hash(
        &[],
        &allowlist,
        &broad.active_families.tokens(),
    );
    // PRIMARY (RED): different active-family sets = different cache lineages.
    assert_ne!(
        narrow_key, broad_key,
        "the cache key ignores ACTIVE_FAMILIES — a narrow boot's filtered, head-advanced \
         cache would overwrite and later SERVE a broadened boot, permanently losing the \
         deleted historical pools and the inactive families' scan window"
    );
    // Same policy ⇒ stable key (the cache still works within one lineage).
    assert_eq!(
        narrow_key,
        engine::discovery_cache::policy_aware_config_hash(
            &[],
            &allowlist,
            &narrow.active_families.tokens(),
        ),
        "the policy-aware key must be deterministic for one profile"
    );
    // Value-level narrow→broad transition over the REAL save/load path: persist a narrow
    // (filtered) cache, then attempt to load it as the broad boot would.
    let dir = std::env::temp_dir().join(format!("wi02c_cache_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("discovery_cache.json");
    let filtered_pools = vec![engine_discovery::DiscoveredPool {
        address: [0x11; 20],
        family: PoolKind::UniswapV3,
        token0: [0x82; 20],
        token1: [0xC0; 20],
        fee_pips: 100,
        fee_bps: 0,
        tick_spacing: 1,
        created_block: 100,
    }];
    let narrow_cache = engine::discovery_cache::DiscoveryCache::new(
        narrow_key,
        200, // the narrow boot advanced the head past the inactive families' scan window
        filtered_pools,
        Vec::new(),
    );
    narrow_cache.save(&path).expect("save narrow cache");
    let broad_load = engine::discovery_cache::DiscoveryCache::load(&path, broad_key);
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        broad_load.is_none(),
        "a BROAD boot loaded the NARROW boot's filtered cache (head already advanced to \
         200) — the broadened boot would resume after the advanced head and never recover \
         the dropped families' pools; it must MISS and full-rescan"
    );
    // The real boot path derives its cache key through the policy-aware seam.
    let startup = source_of("engine/crates/engine/src/startup.rs");
    assert!(
        startup.contains("policy_aware_config_hash("),
        "startup.rs does not derive the discovery-cache key through policy_aware_config_hash \
         — the persisted cache stays policy-blind"
    );
}

/// Uniswap V3 is promotable only while its full authority bundle holds: active (route/state
/// authority), executor exchange id present, measured gas profile present.
#[test]
fn u3_is_promotable_only_with_current_state_gas_route_and_executor_authority() {
    // PRIMARY (RED): removing U3 from the ACTIVE set removes its route/state authority, so it
    // may not stay proven/promotable.
    let policy = resolve(&[("ACTIVE_FAMILIES", "pancake_v3")]).expect("dev profile");
    assert!(
        !policy.active_families.contains(PoolKind::UniswapV3),
        "ACTIVE_FAMILIES=pancake_v3 must leave U3 inactive, got {:?}",
        policy.active_families.tokens()
    );
    assert!(
        !policy.proven_families.contains(PoolKind::UniswapV3),
        "U3 without route/state authority (inactive) may not remain promotable"
    );
    // With the default profile the full authority bundle holds and U3 IS promotable.
    let policy = resolve(&[]).expect("dev default");
    assert!(policy.proven_families.contains(PoolKind::UniswapV3));
    // Executor + gas authority are structural for every proven family.
    for k in engine::fastlane_policy::ALL_FAMILIES {
        if policy.proven_families.contains(k) {
            assert!(
                k.to_mantis_exchange_id().is_some(),
                "proven family {k:?} lacks a Mantis executor exchange id"
            );
            assert!(
                k.swap_gas_estimate() > 0,
                "proven family {k:?} lacks a gas profile"
            );
        }
    }
}

/// A Pancake V3 pool with the same pair+fee as a Uniswap V3 pool must never encode to the
/// Uniswap V3 executor exchange id (P1-09 family binding).
#[test]
fn pancake_v3_same_pair_fee_never_resolves_to_uniswap_v3() {
    // PRIMARY (RED): the executor table is content-hashed into the manifest binding, so a
    // family-id swap cannot survive unnoticed.
    let h = runtime_policy::executor_exchange_id_hash();
    assert_eq!(
        h.len(),
        64,
        "the executor exchange-id table must be sha256-bound into the manifest, got `{h}`"
    );
    // The ids themselves are distinct (Pancake=6 vs U3=0) and route family signatures differ
    // even at identical (pair, fee).
    let pancake = PoolKind::PancakeV3.to_mantis_exchange_id().expect("pancake id");
    let u3 = PoolKind::UniswapV3.to_mantis_exchange_id().expect("u3 id");
    assert_ne!(pancake, u3, "PancakeV3 and UniswapV3 must have distinct executor ids");
    assert_eq!(pancake, 6);
    assert_eq!(u3, 0);
}

/// Uniswap V4 (PoolManager singleton) may not enter the PROVEN/promotable set until its
/// transaction-order authority is proven; detection stays active.
#[test]
fn u4_remains_inactive_until_pool_manager_transaction_order_authority_is_proven() {
    let policy = resolve(&[]).expect("dev default");
    // PRIMARY (RED): despite being a legacy byte-exact candidate, U4 is a shared-state
    // singleton whose transaction-order authority is unproven — it must NOT be promotable.
    assert!(
        !policy.proven_families.contains(PoolKind::UniswapV4),
        "U4 entered the proven set without FASTLANE_SINGLETON_LIVE_PROVEN — the PoolManager \
         transaction-order authority is unproven"
    );
    // Detection stays active (byte-identical universe).
    assert!(policy.active_families.contains(PoolKind::UniswapV4));
    // With the singleton authority explicitly attested, the byte-exact candidate may promote.
    let policy = resolve(&[("FASTLANE_SINGLETON_LIVE_PROVEN", "1")]).expect("dev profile");
    assert!(policy.proven_families.contains(PoolKind::UniswapV4));
    // A live/capital request for U4 refuses.
    assert_live_family_refused("uniswap_v4");
}

/// The Algebra promotion gate requires zero disagreement: today's 34-of-36 report must not
/// promote.
#[test]
fn algebra_zero_disagreement_gate_rejects_current_34_of_36_report() {
    // PRIMARY (RED): an authoritative resolution whose strict proof rejects Algebra (the
    // 34/36 report ⇒ proven=false) may not leave Algebra promoted.
    let pairs = [
        ("FASTLANE_AUTHORITATIVE", "1"),
        ("FASTLANE_PROMOTED_FAMILIES", "algebra_v19"),
    ];
    let var = env_of(&pairs);
    let mut proven = [true; engine::fastlane_policy::N_FAMILIES];
    proven[engine::fastlane_policy::family_index(PoolKind::AlgebraV19)] = false; // 34/36 ⇒ NOT proven
    let verdict = runtime_policy::resolve_runtime_policy_with(&var, || Ok(proven));
    match verdict {
        Ok(policy) => assert!(
            !policy.proven_families.contains(PoolKind::AlgebraV19),
            "a 34-of-36 (disagree=2) Algebra report promoted the family — the gate must \
             require disagree==0"
        ),
        Err(_) => {} // refusing the whole authoritative boot is also fail-closed
    }
    // The pure gate itself: 34/36 with 2 disagreements is NOT promotion-ready.
    assert!(!engine::fastlane_policy::family_promotion_ready(34, 2, 36));
    assert!(engine::fastlane_policy::family_promotion_ready(36, 0, 36));
}

/// Uniswap V2: zero current strict-bar proof ⇒ cannot enter the live active or proven set.
#[test]
fn uniswap_v2_zero_current_proof_cannot_enter_active_or_proven_set() {
    // PRIMARY (RED): live request refuses (capital = U3+Pancake only).
    assert_live_family_refused("uniswap_v2");
    // The dev default is UNCHANGED (byte-identical detection).
    let policy = resolve(&[]).expect("dev default");
    assert!(policy.active_families.contains(PoolKind::UniswapV2));
}

/// Sushi V2: zero current strict-bar proof ⇒ cannot enter the live active or proven set.
#[test]
fn sushi_v2_zero_current_proof_cannot_enter_active_or_proven_set() {
    assert_live_family_refused("sushi_v2");
    let policy = resolve(&[]).expect("dev default");
    assert!(policy.active_families.contains(PoolKind::SushiV2));
}

/// Camelot V2 carries admin-mutable fork fees: live activation requires reviewed fork fee
/// vectors, which do not exist.
#[test]
fn camelot_v2_requires_fork_fee_vectors_before_activation() {
    assert_live_family_refused("camelot_v2");
    let policy = resolve(&[]).expect("dev default");
    assert!(policy.active_families.contains(PoolKind::CamelotV2));
}

/// Sushi V3: zero current strict-bar proof ⇒ cannot enter the live active or proven set.
#[test]
fn sushi_v3_zero_current_proof_cannot_enter_active_or_proven_set() {
    assert_live_family_refused("sushi_v3");
    let policy = resolve(&[]).expect("dev default");
    assert!(policy.active_families.contains(PoolKind::SushiV3));
}

/// Curve StableSwap-NG is a shared-state family: absent from the fast-lane graph regardless
/// of env promotion requests.
#[test]
fn curve_ssng_shared_state_family_is_absent_from_fastlane_graph() {
    // PRIMARY (RED): live request refuses.
    assert_live_family_refused("curve_stableswap");
    // Never promotable — even a casual promoted=all + operator ack cannot put the
    // shared-state, non-byte-exact family on the zero-RPC lane.
    let policy = resolve(&[
        ("FASTLANE_PROMOTED_FAMILIES", "all"),
        ("FASTLANE_ACK_AGREEMENT_PROVEN", "1"),
    ])
    .expect("dev profile");
    assert!(
        !policy.proven_families.contains(PoolKind::CurveStableSwapNg),
        "Curve SSNG entered the fast-lane graph — shared-state families are never promotable \
         without singleton authority + byte parity"
    );
}

/// Curve CryptoSwap variants require individual state + executor proof before live
/// activation.
#[test]
fn curve_crypto_variants_require_individual_state_and_executor_proof() {
    assert_live_family_refused("curve_cryptoswap");
    let policy = resolve(&[
        ("FASTLANE_PROMOTED_FAMILIES", "all"),
        ("FASTLANE_ACK_AGREEMENT_PROVEN", "1"),
    ])
    .expect("dev profile");
    assert!(!policy.proven_families.contains(PoolKind::CurveCryptoSwap));
}

/// Trader Joe LB requires a complete bin window + a measured gas profile before live
/// activation.
#[test]
fn trader_joe_lb_requires_complete_bin_window_and_measured_gas_profile() {
    assert_live_family_refused("trader_joe_lb");
    let policy = resolve(&[]).expect("dev default");
    assert!(policy.active_families.contains(PoolKind::TraderJoeLb));
    assert!(!policy.proven_families.contains(PoolKind::TraderJoeLb));
}

/// Maverick V2 with empty live-reference readiness vectors stays inactive even when the
/// operator flag requests it.
#[test]
fn maverick_empty_readiness_vectors_keep_family_inactive() {
    // Precondition (documented reality): the reviewed live reference vectors are EMPTY.
    assert!(
        !engine_amm::maverick_v2::live_reference_vectors_ready(),
        "precondition changed: Maverick live reference vectors exist now — re-derive this test"
    );
    // PRIMARY (RED): the operator flag ALONE cannot activate Maverick while readiness is
    // empty.
    let policy = resolve(&[("MAVERICK_ENABLE", "1")]).expect("dev profile");
    assert!(
        !policy.active_families.contains(PoolKind::MaverickV2),
        "MAVERICK_ENABLE=1 activated Maverick despite EMPTY live-reference readiness vectors"
    );
    assert!(!policy.proven_families.contains(PoolKind::MaverickV2));
    assert_live_family_refused("maverick_v2");
}

/// Fluid V1/V2 shared-state workers must not start when the family is inactive.
#[test]
fn fluid_v1_v2_shared_state_workers_do_not_start_when_family_inactive() {
    let policy =
        resolve(&[("ACTIVE_FAMILIES", "uniswap_v3,pancake_v3")]).expect("capital family set");
    // PRIMARY (RED): the poller spawn decision refuses when FluidDex is inactive.
    assert!(
        !runtime_policy::fluid_workers_allowed(&policy),
        "the Fluid pollers would start under a policy where FluidDex is INACTIVE"
    );
    let policy = resolve(&[]).expect("dev default");
    assert!(
        runtime_policy::fluid_workers_allowed(&policy),
        "FluidDex is active by default — the pollers stay allowed (byte-identical)"
    );
    let main_src = source_of("engine/crates/engine/src/main.rs");
    assert!(
        main_src.contains("fluid_workers_allowed("),
        "main.rs fluid_poller spawn does not consult fluid_workers_allowed"
    );
    assert_live_family_refused("fluid");
}

/// DODO requires PMM variant state, route, executor, and gas proof before live activation.
#[test]
fn dodo_requires_pmm_variant_state_route_executor_and_gas_proof() {
    assert_live_family_refused("dodo");
    let policy = resolve(&[]).expect("dev default");
    assert!(policy.active_families.contains(PoolKind::DodoV2));
    assert!(!policy.proven_families.contains(PoolKind::DodoV2));
}

/// Kyber's activation must derive from evidence, not a silent default: unset ⇒ inactive;
/// explicit dev opt-in ⇒ active; a live request ⇒ boot refusal (the strict parity artifact
/// lists Kyber unsupported and production startup drops Kyber pools).
#[test]
fn kyber_artifact_source_drift_fails_boot_instead_of_defaulting_on() {
    // PRIMARY (RED): the WI-01-found drift — Kyber must NOT default active.
    let policy = resolve(&[]).expect("dev default");
    assert!(
        !policy.active_families.contains(PoolKind::KyberSwapElastic),
        "KYBER_ENABLE unset still defaults Kyber ACTIVE — the artifact-source drift (stale \
         'default off' comment vs 2026-07-02 default-on flip) must fail closed, not open"
    );
    // Explicit dev opt-in is honored (structurally-disabled submission only).
    let policy = resolve(&[("KYBER_ENABLE", "1")]).expect("dev opt-in");
    assert!(policy.active_families.contains(PoolKind::KyberSwapElastic));
    assert!(
        !policy.proven_families.contains(PoolKind::KyberSwapElastic),
        "Kyber is never fast-lane promotable"
    );
    // A live/capital request fails boot instead of defaulting on.
    assert_live_family_refused("kyber_elastic");
}

/// Solidly V3 has NO Mantis executor exchange id: a live profile requesting it is rejected
/// before any route is built.
#[test]
fn solidly_v3_without_executor_exchange_id_is_rejected_before_route_build() {
    // PRIMARY (RED): the live refusal.
    assert_live_family_refused("solidly_v3");
    // The structural fact the refusal rests on.
    assert!(
        PoolKind::SolidlyV3.to_mantis_exchange_id().is_none(),
        "SolidlyV3 acquired an executor exchange id — re-derive its activation policy"
    );
    let policy = resolve(&[]).expect("dev default");
    assert!(
        !policy.proven_families.contains(PoolKind::SolidlyV3),
        "an executor-less family can never be promotable"
    );
    // Dev default keeps Solidly active for quote/rank (byte-identical detection).
    assert!(policy.active_families.contains(PoolKind::SolidlyV3));
}

/// Balancer stable (Vault family) remains absent from live activation without full variant
/// proof. WI-02c 4C: the DEV promotion contract is the fastlane_policy one — a bare
/// `promoted=all` request (no operator ACK) never promotes it, an explicit
/// FASTLANE_ACK_AGREEMENT_PROVEN attestation does (supported promotion state; the live
/// capital refusal above is what protects capital).
#[test]
fn balancer_stable_vault_family_remains_absent_without_full_variant_proof() {
    assert_live_family_refused("balancer_stable");
    // No ACK: a casual `all` cannot blind-promote the Vault family (fail-safe default).
    let policy = resolve(&[("FASTLANE_PROMOTED_FAMILIES", "all")]).expect("dev profile");
    assert!(!policy.proven_families.contains(PoolKind::BalancerStable));
    // Operator-ACKed: the promotion contract admits it (codex WI02B_VERIFY 4C — stripping
    // ACKed exotics was coverage loss under STRICT_NO_HOT_RPC, not tightening).
    let policy = resolve(&[
        ("FASTLANE_PROMOTED_FAMILIES", "all"),
        ("FASTLANE_ACK_AGREEMENT_PROVEN", "1"),
    ])
    .expect("dev profile");
    assert!(
        policy.proven_families.contains(PoolKind::BalancerStable),
        "an operator-ACKed BalancerStable promotion was stripped from proven_families — \
         the byte_exact-only filter is a coverage regression"
    );
}

/// Balancer weighted is a Tier-2 power-approximation kernel: it is never promoted by
/// DEFAULT or by a bare `promoted=all` request, and a live/capital request always refuses.
/// WI-02c 4C: an explicit operator ACK is the promotion contract's supported attestation
/// path and is honored in the DEV declaration (capital stays protected by the live refusal
/// + the on-chain minNetProfit revert).
#[test]
fn balancer_weighted_approximation_cannot_be_fastlane_promoted() {
    // PRIMARY (RED): live request refuses.
    assert_live_family_refused("balancer_weighted");
    // Default + bare `all` (no ACK): never promoted (fail-safe).
    let policy = resolve(&[]).expect("dev default");
    assert!(!policy.proven_families.contains(PoolKind::BalancerWeighted));
    let policy = resolve(&[("FASTLANE_PROMOTED_FAMILIES", "all")]).expect("dev profile");
    assert!(
        !policy.proven_families.contains(PoolKind::BalancerWeighted),
        "a casual promoted=all without the operator ACK blind-promoted the approximation \
         kernel"
    );
    // Operator-ACKed: the supported attestation path (codex WI02B_VERIFY 4C).
    let policy = resolve(&[
        ("FASTLANE_PROMOTED_FAMILIES", "all"),
        ("FASTLANE_ACK_AGREEMENT_PROVEN", "1"),
    ])
    .expect("dev profile");
    assert!(
        policy.proven_families.contains(PoolKind::BalancerWeighted),
        "an operator-ACKed BalancerWeighted promotion was stripped — byte_exact-only \
         filter regression (codex WI02B_VERIFY 4C)"
    );
}
