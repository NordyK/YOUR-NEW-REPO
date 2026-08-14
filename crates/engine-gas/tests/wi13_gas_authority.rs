//! WI-13 RED suite — one-block, component-complete gas authority (codex §3.4, closes FL-14).
//!
//! Every test drives the REAL production refresh entry point
//! (`engine_gas::authority::refresh_gas_authority_once`, called by the engine's
//! `gas_refresher` with the real RPC transport) through a mock [`GasAuthorityTransport`]
//! (money firewall: zero network, zero paid RPC). The RED run of this suite demonstrates
//! the CONFIRMED legacy failure modes (FL-14 fresh-on-failure floor, hardcoded
//! tokenGasUnits=16, unbound block, plain non-WithAggregator selectors, saturating
//! conversions) on the transplanted legacy semantics; GREEN fixes the failure path.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use alloy_primitives::{keccak256, U256};
use engine_gas::authority::{
    build_authority_calls, refresh_gas_authority_once, selector, AuthorityCall, CanonicalHeader,
    GasAuthorityConfig, GasAuthorityError, GasAuthorityTransport, GasComponentStatus,
    GasTransportError, RefreshOutcome, RpcErrorClass, COMPLETE_COMPONENT_MASK,
    COMPONENT_L1_BASE_FEE_ESTIMATE, COMPONENT_MINIMUM_GAS_PRICE, COMPONENT_PRICES_IN_ARB_GAS,
    COMPONENT_PRICES_IN_WEI, FORBIDDEN_SIG_PRICES_IN_ARB_GAS, FORBIDDEN_SIG_PRICES_IN_WEI,
    SIG_L1_BASE_FEE_ESTIMATE, SIG_MINIMUM_GAS_PRICE, SIG_MULTICALL3_GET_BLOCK_NUMBER,
    SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR, SIG_PRICES_IN_WEI_WITH_AGGREGATOR,
};
use engine_gas::{GasOracle, GasSnapshot};

// -------------------------------------------------------------------------------------------
// Fixture values (arbitrary but realistic; NOT the legacy floor).
// -------------------------------------------------------------------------------------------

const AGGREGATOR: [u8; 20] = [0xA6; 20];
const PROVIDER_A: u16 = 7;
const PROVIDER_B: u16 = 9;

const PER_L2_TX_WEI: u64 = 1_400_000_000_000; // 7e4 gas * 2e7 wei/gas
const PER_L1_CALLDATA_UNIT_WEI: u64 = 189_100_000; // l1 estimate * 61
const PER_STORAGE_ALLOC_WEI: u64 = 2_000_000_000;
const PER_ARB_GAS_BASE_WEI: u64 = 20_000_000; // 0.02 gwei ArbOS floor
const PER_ARB_GAS_CONGESTION_WEI: u64 = 3_000_000;
const PER_ARB_GAS_TOTAL_WEI: u64 = PER_ARB_GAS_BASE_WEI + PER_ARB_GAS_CONGESTION_WEI;
const PER_L2_TX_ARB_GAS: u64 = 70_000;
/// Live tokenGasUnits — deliberately NOT 16 so a hardcode is caught.
const TOKEN_GAS_UNITS_LIVE: u64 = 61;
const PER_STORAGE_ALLOC_ARB_GAS: u64 = 100_000;
const MINIMUM_L2_GAS_PRICE_WEI: u64 = 20_000_000;
const L1_BASE_FEE_ESTIMATE_WEI: u64 = 3_100_000;
const LEGACY_FLOOR_WEI: u128 = 100_000;

fn words(vals: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vals.len() * 32);
    for v in vals {
        let mut w = [0u8; 32];
        w[24..32].copy_from_slice(&v.to_be_bytes());
        out.extend_from_slice(&w);
    }
    out
}

fn u256_word(v: U256) -> Vec<u8> {
    v.to_be_bytes::<32>().to_vec()
}

fn header_a() -> CanonicalHeader {
    CanonicalHeader {
        number: 361_552_001,
        hash: [0xAB; 32],
    }
}

fn header_b() -> CanonicalHeader {
    CanonicalHeader {
        number: 361_552_007,
        hash: [0xBC; 32],
    }
}

/// Canned per-(target, selector) response map for a healthy provider.
fn healthy_responses(cfg: &GasAuthorityConfig) -> HashMap<([u8; 20], [u8; 4]), Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        (cfg.arb_gas_info, selector(SIG_PRICES_IN_WEI_WITH_AGGREGATOR)),
        words(&[
            PER_L2_TX_WEI,
            PER_L1_CALLDATA_UNIT_WEI,
            PER_STORAGE_ALLOC_WEI,
            PER_ARB_GAS_BASE_WEI,
            PER_ARB_GAS_CONGESTION_WEI,
            PER_ARB_GAS_TOTAL_WEI,
        ]),
    );
    m.insert(
        (
            cfg.arb_gas_info,
            selector(SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR),
        ),
        words(&[PER_L2_TX_ARB_GAS, TOKEN_GAS_UNITS_LIVE, PER_STORAGE_ALLOC_ARB_GAS]),
    );
    m.insert(
        (cfg.arb_gas_info, selector(SIG_MINIMUM_GAS_PRICE)),
        words(&[MINIMUM_L2_GAS_PRICE_WEI]),
    );
    m.insert(
        (cfg.arb_gas_info, selector(SIG_L1_BASE_FEE_ESTIMATE)),
        words(&[L1_BASE_FEE_ESTIMATE_WEI]),
    );
    m
}

/// Mock one-provider transport with canned responses and full call recording.
struct MockTransport {
    provider_id: u16,
    header: Result<CanonicalHeader, GasTransportError>,
    /// Per-(target, selector) canned return bytes. Missing entry = inner-call failure, which
    /// under `allowFailure=false` fails the WHOLE aggregate.
    responses: HashMap<([u8; 20], [u8; 4]), Vec<u8>>,
    /// Force the whole aggregate to fail regardless of responses.
    fail_aggregate: bool,
    /// Recorded aggregates: (block_hash, calls).
    recorded: RefCell<Vec<([u8; 32], Vec<AuthorityCall>)>>,
}

impl MockTransport {
    fn healthy(provider_id: u16, header: CanonicalHeader, cfg: &GasAuthorityConfig) -> Self {
        Self {
            provider_id,
            header: Ok(header),
            responses: healthy_responses(cfg),
            fail_aggregate: false,
            recorded: RefCell::new(Vec::new()),
        }
    }

    fn failing_aggregate(provider_id: u16, header: CanonicalHeader) -> Self {
        Self {
            provider_id,
            header: Ok(header),
            responses: HashMap::new(),
            fail_aggregate: true,
            recorded: RefCell::new(Vec::new()),
        }
    }

    fn aggregates(&self) -> Vec<([u8; 32], Vec<AuthorityCall>)> {
        self.recorded.borrow().clone()
    }
}

impl GasAuthorityTransport for MockTransport {
    fn provider_id(&self) -> u16 {
        self.provider_id
    }

    fn canonical_header(&self) -> Result<CanonicalHeader, GasTransportError> {
        self.header.clone()
    }

    fn aggregate3_at_hash(
        &self,
        block_hash: [u8; 32],
        calls: &[AuthorityCall],
    ) -> Result<Vec<Vec<u8>>, GasTransportError> {
        self.recorded
            .borrow_mut()
            .push((block_hash, calls.to_vec()));
        if self.fail_aggregate {
            return Err(GasTransportError::Rpc("injected aggregate failure".into()));
        }
        let mut out = Vec::with_capacity(calls.len());
        for c in calls {
            if c.call_data.len() < 4 {
                return Err(GasTransportError::Rpc("short calldata".into()));
            }
            let sel = [c.call_data[0], c.call_data[1], c.call_data[2], c.call_data[3]];
            // The block-binding call answers with the block the aggregate executed at —
            // the requested EIP-1898 hash's number when it matches the provider's known
            // header, else a mismatching number (models a provider on another head).
            if sel == selector(SIG_MULTICALL3_GET_BLOCK_NUMBER) {
                let n = match &self.header {
                    Ok(h) if h.hash == block_hash => h.number,
                    _ => 1, // wrong/unknown anchor: some other block executed the batch
                };
                out.push(words(&[n]));
                continue;
            }
            match self.responses.get(&(c.target, sel)) {
                Some(bytes) => out.push(bytes.clone()),
                None => {
                    return Err(GasTransportError::Rpc(format!(
                        "no canned response for selector 0x{}",
                        sel.iter().map(|b| format!("{b:02x}")).collect::<String>()
                    )))
                }
            }
        }
        Ok(out)
    }
}

fn cfg() -> GasAuthorityConfig {
    GasAuthorityConfig::arbitrum_one(AGGREGATOR)
}

fn oracle() -> GasOracle {
    GasOracle::new(GasSnapshot::default(), 500)
}

/// Seed one complete generation through a healthy provider; returns the transport used.
fn seed_complete(o: &GasOracle, now_mono_ns: u64, now_wall_ms: u64) -> (MockTransport, NonZeroU64) {
    let c = cfg();
    let t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    let out = refresh_gas_authority_once(&t, &c, o, now_mono_ns, now_wall_ms);
    match out {
        RefreshOutcome::Complete { generation } => (t, generation),
        other => panic!("seed refresh must be complete, got {other:?}"),
    }
}

fn find_call<'a>(calls: &'a [AuthorityCall], sig: &str) -> Option<&'a AuthorityCall> {
    calls
        .iter()
        .find(|c| c.call_data.len() >= 4 && c.call_data[0..4] == selector(sig))
}

// ===========================================================================================
// 1. One-block fetch: all fee-critical fields at ONE explicit EIP-1898 block hash.
// ===========================================================================================

#[test]
fn gas_snapshot_fetches_all_fee_critical_fields_at_one_explicit_block_hash() {
    let c = cfg();
    let o = oracle();
    let t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);

    // Exactly ONE aggregate, anchored at the verified canonical header's hash.
    let aggs = t.aggregates();
    assert_eq!(aggs.len(), 1, "exactly one aggregate3 per attempt, got {}", aggs.len());
    let (anchor, calls) = &aggs[0];
    assert_eq!(
        *anchor,
        header_a().hash,
        "aggregate must be anchored at the canonical header hash (EIP-1898)"
    );

    // All four fee-critical components + the block-binding call, all allowFailure=false.
    assert_eq!(calls.len(), 5, "4 fee components + block binding, got {}", calls.len());
    for call in calls {
        assert!(
            !call.allow_failure,
            "authority calls must use allowFailure=false (all-or-nothing)"
        );
    }
    assert!(
        find_call(calls, SIG_PRICES_IN_WEI_WITH_AGGREGATOR).is_some(),
        "missing getPricesInWeiWithAggregator"
    );
    assert!(
        find_call(calls, SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR).is_some(),
        "missing getPricesInArbGasWithAggregator"
    );
    assert!(
        find_call(calls, SIG_MINIMUM_GAS_PRICE).is_some(),
        "missing getMinimumGasPrice"
    );
    assert!(
        find_call(calls, SIG_L1_BASE_FEE_ESTIMATE).is_some(),
        "missing getL1BaseFeeEstimate"
    );
    let bind = find_call(calls, SIG_MULTICALL3_GET_BLOCK_NUMBER)
        .expect("missing Multicall3.getBlockNumber block-binding call");
    assert_eq!(
        bind.target, c.multicall3,
        "block-binding call must target Multicall3"
    );

    // The published snapshot is complete and bound to the SAME header.
    let RefreshOutcome::Complete { generation } = out else {
        panic!("healthy fetch must publish a complete snapshot, got {out:?}");
    };
    let snap = o.authority().expect("complete snapshot published");
    assert_eq!(snap.generation, generation);
    assert_eq!(snap.canonical_block_number, header_a().number);
    assert_eq!(snap.canonical_block_hash, header_a().hash);
    assert_eq!(snap.complete_component_mask, COMPLETE_COMPONENT_MASK);
    assert_eq!(snap.component_status, [GasComponentStatus::Valid; 4]);
    // Raw fields carried verbatim as U256.
    assert_eq!(snap.raw.per_l2_tx_wei, U256::from(PER_L2_TX_WEI));
    assert_eq!(snap.raw.per_arb_gas_total_wei, U256::from(PER_ARB_GAS_TOTAL_WEI));
    assert_eq!(
        snap.raw.per_l1_calldata_unit_arb_gas,
        U256::from(TOKEN_GAS_UNITS_LIVE)
    );
    assert_eq!(
        snap.raw.l1_base_fee_estimate_wei,
        U256::from(L1_BASE_FEE_ESTIMATE_WEI)
    );
    assert_eq!(snap.raw.minimum_l2_gas_price_wei, U256::from(MINIMUM_L2_GAS_PRICE_WEI));
}

// ===========================================================================================
// 2. Frozen WithAggregator identity — plain price selectors are FORBIDDEN via Multicall.
// ===========================================================================================

#[test]
fn multicall_prices_use_frozen_with_aggregator_identity_not_multicall_sender() {
    let c = cfg();
    let calls = build_authority_calls(&c);

    let mut agg_arg = [0u8; 32];
    agg_arg[12..32].copy_from_slice(&AGGREGATOR);

    let wei = find_call(&calls, SIG_PRICES_IN_WEI_WITH_AGGREGATOR)
        .expect("wei prices must use getPricesInWeiWithAggregator");
    assert_eq!(
        &wei.call_data[4..36],
        agg_arg.as_slice(),
        "wei price call must carry the FROZEN aggregator argument"
    );
    let ag = find_call(&calls, SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR)
        .expect("arbgas prices must use getPricesInArbGasWithAggregator");
    assert_eq!(
        &ag.call_data[4..36],
        agg_arg.as_slice(),
        "arbgas price call must carry the FROZEN aggregator argument"
    );

    // The non-WithAggregator variants would resolve msg.sender to Multicall3 — forbidden.
    assert!(
        find_call(&calls, FORBIDDEN_SIG_PRICES_IN_WEI).is_none(),
        "getPricesInWei() is FORBIDDEN through Multicall3 (msg.sender = Multicall3)"
    );
    assert!(
        find_call(&calls, FORBIDDEN_SIG_PRICES_IN_ARB_GAS).is_none(),
        "getPricesInArbGas() is FORBIDDEN through Multicall3 (msg.sender = Multicall3)"
    );
}

// ===========================================================================================
// 3. FL-14 core: a failed component can NEVER publish a fresh complete generation.
// ===========================================================================================

#[test]
fn failed_gas_component_cannot_publish_fresh_complete_generation() {
    let c = cfg();
    let o = oracle();
    let (_t, gen1) = seed_complete(&o, 1_000, 10_000);

    // Second refresh: PricesInWei decodes short (5 words instead of 6).
    let mut t2 = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    t2.responses.insert(
        (c.arb_gas_info, selector(SIG_PRICES_IN_WEI_WITH_AGGREGATOR)),
        words(&[1, 2, 3, 4, 5]),
    );
    let out = refresh_gas_authority_once(&t2, &c, &o, 2_000, 20_000);

    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "a partial/undecodable component must FAIL the attempt, got {out:?}"
    );
    let snap = o.authority().expect("last-good snapshot survives");
    assert_eq!(
        snap.generation, gen1,
        "a failed component must NOT advance the complete generation"
    );
    assert_eq!(
        snap.captured_at_mono_ns, 1_000,
        "a failed component must NOT renew the fee snapshot's capture time"
    );
    // The attempt evidence pinpoints the failed component.
    let health = o.authority_health();
    let attempt = health.last_attempt.expect("failed attempt recorded");
    assert_eq!(
        attempt.component_status[COMPONENT_PRICES_IN_WEI],
        GasComponentStatus::DecodeError,
        "component 0 must be recorded as DecodeError"
    );
    assert_eq!(attempt.component_status[COMPONENT_PRICES_IN_ARB_GAS], GasComponentStatus::Valid);
    assert_eq!(attempt.component_status[COMPONENT_MINIMUM_GAS_PRICE], GasComponentStatus::Valid);
    assert_eq!(
        attempt.component_status[COMPONENT_L1_BASE_FEE_ESTIMATE],
        GasComponentStatus::Valid
    );
    assert_eq!(health.last_complete_generation, Some(gen1));
}

// ===========================================================================================
// 4. Failure changes health ONLY — fee bytes, generation and freshness stay byte-identical.
// ===========================================================================================

#[test]
fn failed_refresh_changes_gas_health_but_not_fee_bytes_generation_or_freshness_timestamp() {
    let c = cfg();
    let o = oracle();
    let (_t, _gen1) = seed_complete(&o, 1_000, 10_000);
    let before = o.authority().expect("seeded");
    let health_before = o.authority_health();

    let t2 = MockTransport::failing_aggregate(PROVIDER_A, header_a());
    let out = refresh_gas_authority_once(&t2, &c, &o, 5_000, 50_000);
    assert!(matches!(out, RefreshOutcome::Failed { .. }), "got {out:?}");

    let after = o.authority().expect("still seeded");
    assert!(
        Arc::ptr_eq(&before, &after),
        "fee snapshot must be byte-for-byte (pointer) unchanged by a failed refresh"
    );
    let health_after = o.authority_health();
    assert!(
        health_after.health_generation > health_before.health_generation,
        "health generation must advance on a failed attempt"
    );
    let attempt = health_after.last_attempt.expect("attempt recorded");
    assert_eq!(attempt.attempted_at_mono_ns, 5_000);
    assert_eq!(
        attempt.component_status,
        [GasComponentStatus::RpcError(RpcErrorClass::Transport); 4],
        "whole-aggregate failure stamps all four components"
    );
    assert_eq!(
        health_after.last_complete_generation,
        Some(before.generation),
        "last complete generation is unchanged"
    );
}

// ===========================================================================================
// 5. FL-14 legacy surface: failure keeps the OLD oracle value AND age — the 100k floor is dead.
// ===========================================================================================

#[test]
fn failed_l1_component_keeps_last_good_value_and_age_never_fresh_floor() {
    let c = cfg();
    let o = oracle();

    // Seed: complete refresh publishes the live L1 estimate at wall time 10_000.
    let (_t, _gen) = seed_complete(&o, 1_000, 10_000);
    let seeded = o.load();
    assert_eq!(seeded.l1_base_fee_per_gas_wei, L1_BASE_FEE_ESTIMATE_WEI as u128);
    assert_eq!(seeded.captured_at_ms, 10_000);
    assert_eq!(o.l1_sigma_bps(), 0, "single live sample: sigma 0");

    // Failed refresh at wall time 20_000: transport down.
    let t2 = MockTransport::failing_aggregate(PROVIDER_A, header_a());
    let out = refresh_gas_authority_once(&t2, &c, &o, 2_000, 20_000);
    assert!(matches!(out, RefreshOutcome::Failed { .. }), "got {out:?}");

    let after = o.load();
    assert_eq!(
        after.l1_base_fee_per_gas_wei, L1_BASE_FEE_ESTIMATE_WEI as u128,
        "failed L1 leg must keep the LAST-GOOD value, never a substitute floor"
    );
    assert_ne!(
        after.l1_base_fee_per_gas_wei, LEGACY_FLOOR_WEI,
        "the 100k-wei fresh floor (FL-14) must be dead"
    );
    assert_eq!(
        after.captured_at_ms, 10_000,
        "failed refresh must keep the ORIGINAL capture time (age keeps growing; the \
         existing load_fresh staleness gate declines it past max_age) — a failure can \
         never make the snapshot fresh"
    );
    assert_eq!(
        o.l1_sigma_bps(),
        0,
        "a failed fetch must not pollute the volatility tracker with a synthetic sample"
    );
}

// ===========================================================================================
// 6. Checked conversions decline on overflow — never clamp/saturate.
// ===========================================================================================

#[test]
fn gas_snapshot_checked_conversion_rejects_u256_overflow() {
    let c = cfg();
    let o = oracle();
    let mut t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    // per-L2-tx wei = 2^200: legal U256, does not fit u128.
    let huge = U256::from(1u8) << 200;
    let mut wei_words = Vec::new();
    wei_words.extend_from_slice(&u256_word(huge));
    wei_words.extend_from_slice(&words(&[
        PER_L1_CALLDATA_UNIT_WEI,
        PER_STORAGE_ALLOC_WEI,
        PER_ARB_GAS_BASE_WEI,
        PER_ARB_GAS_CONGESTION_WEI,
        PER_ARB_GAS_TOTAL_WEI,
    ]));
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_PRICES_IN_WEI_WITH_AGGREGATOR)),
        wei_words,
    );

    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);
    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "a U256 that does not fit the checked view must DECLINE the snapshot, got {out:?}"
    );
    assert!(
        o.authority().is_none(),
        "no snapshot may publish from an overflowing component (no clamp-to-MAX)"
    );
    let health = o.authority_health();
    let attempt = health.last_attempt.expect("attempt recorded");
    assert_eq!(
        attempt.component_status[COMPONENT_PRICES_IN_WEI],
        GasComponentStatus::Overflow,
        "the overflowing component must be recorded as Overflow"
    );
}

// ===========================================================================================
// 7. tokenGasUnits is LIVE at the snapshot block — the hardcoded 16 is dead.
// ===========================================================================================

#[test]
fn token_gas_units_is_fetched_at_snapshot_block_not_hardcoded_16() {
    let o = oracle();
    let (_t, _gen) = seed_complete(&o, 1_000, 10_000);

    let snap = o.authority().expect("seeded");
    assert_eq!(
        snap.checked.per_l1_calldata_unit_arb_gas, TOKEN_GAS_UNITS_LIVE,
        "checked view must carry the LIVE tokenGasUnits from getPricesInArbGasWithAggregator"
    );
    let legacy = o.load();
    assert_eq!(
        legacy.l1_gas_per_data_byte, TOKEN_GAS_UNITS_LIVE as u32,
        "legacy bridge must carry the LIVE tokenGasUnits, not a hardcoded 16"
    );
    // §3.4 amended: the envelope pad actually used is carried on the published snapshot
    // (per-publication evidence of the policy-bound config value).
    assert_eq!(
        legacy.static_overhead_bytes,
        cfg().legacy_static_overhead_bytes,
        "every published snapshot must carry the policy-bound envelope pad as evidence"
    );
}

// ===========================================================================================
// 8. Provider failover restarts the WHOLE snapshot — no component/head mixing.
// ===========================================================================================

#[test]
fn provider_failover_restarts_complete_snapshot() {
    let c = cfg();
    let o = oracle();

    // Provider A: whole aggregate fails. NOTHING may publish.
    let ta = MockTransport::failing_aggregate(PROVIDER_A, header_a());
    let out_a = refresh_gas_authority_once(&ta, &c, &o, 1_000, 10_000);
    assert!(matches!(out_a, RefreshOutcome::Failed { .. }), "got {out_a:?}");
    assert!(
        o.authority().is_none(),
        "a failed provider must not publish anything (no partial/floor snapshot)"
    );

    // Provider B: fresh transport, fresh header, complete restart.
    let tb = MockTransport::healthy(PROVIDER_B, header_b(), &c);
    let out_b = refresh_gas_authority_once(&tb, &c, &o, 2_000, 20_000);
    assert!(matches!(out_b, RefreshOutcome::Complete { .. }), "got {out_b:?}");

    let aggs = tb.aggregates();
    assert_eq!(aggs.len(), 1, "failover reissues ONE whole aggregate");
    assert_eq!(
        aggs[0].0,
        header_b().hash,
        "failover must anchor at provider B's OWN verified header"
    );
    assert_eq!(aggs[0].1.len(), 5, "failover refetches ALL components");

    let snap = o.authority().expect("B published");
    assert_eq!(snap.provider_id, PROVIDER_B, "snapshot provider = the provider that served it");
    assert_eq!(snap.canonical_block_number, header_b().number);
    assert_eq!(snap.canonical_block_hash, header_b().hash);
}

// ===========================================================================================
// 9. Admission ages against the ORIGINAL capture time; incomplete-past-TTL declines.
// ===========================================================================================

#[test]
fn incomplete_authority_past_hard_ttl_declines_admission() {
    let c = cfg();
    let o = oracle();
    let (_t, _gen) = seed_complete(&o, 1_000, 10_000);
    const TTL_NS: u64 = 10_000;

    // A failed attempt AFTER the seed must not renew the age.
    let t2 = MockTransport::failing_aggregate(PROVIDER_A, header_a());
    let out = refresh_gas_authority_once(&t2, &c, &o, 9_000, 90_000);
    assert!(matches!(out, RefreshOutcome::Failed { .. }), "got {out:?}");

    // Within the ORIGINAL TTL window (capture 1_000): usable.
    assert!(
        o.usable_authority(1_000 + TTL_NS - 1, TTL_NS).is_ok(),
        "last-good within its original TTL must remain usable"
    );
    // Past the ORIGINAL TTL: declined — even though a (failed) attempt happened at 9_000.
    match o.usable_authority(1_000 + TTL_NS + 1, TTL_NS) {
        Err(GasAuthorityError::Expired { age_ns, ttl_ns }) => {
            assert_eq!(age_ns, TTL_NS + 1, "age must be measured from ORIGINAL capture");
            assert_eq!(ttl_ns, TTL_NS);
        }
        other => panic!(
            "incomplete-past-TTL must decline with Expired (failure never renews freshness), \
             got {other:?}"
        ),
    }
}

// ===========================================================================================
// 10. Documented tuple relationship cross-check: total = base + congestion.
// ===========================================================================================

#[test]
fn cross_check_mismatch_declines_publication() {
    let c = cfg();
    let o = oracle();
    let mut t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_PRICES_IN_WEI_WITH_AGGREGATOR)),
        words(&[
            PER_L2_TX_WEI,
            PER_L1_CALLDATA_UNIT_WEI,
            PER_STORAGE_ALLOC_WEI,
            PER_ARB_GAS_BASE_WEI,
            PER_ARB_GAS_CONGESTION_WEI,
            PER_ARB_GAS_TOTAL_WEI + 1, // violates total = base + congestion
        ]),
    );
    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);
    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "a tuple-relationship violation must decline publication, got {out:?}"
    );
    assert!(o.authority().is_none(), "no snapshot may publish on a cross-check mismatch");
    let health = o.authority_health();
    let attempt = health.last_attempt.expect("attempt recorded");
    assert_eq!(
        attempt.component_status[COMPONENT_PRICES_IN_WEI],
        GasComponentStatus::CrossCheckMismatch
    );
}

// ===========================================================================================
// 11. Wrong-block execution evidence declines the attempt.
// ===========================================================================================

#[test]
fn aggregate_executed_at_wrong_block_declines_publication() {
    let c = cfg();
    let o = oracle();
    // Model a provider that ADVERTISES header_a but actually executes the aggregate at a
    // different head: the wrapper advertises header_a via canonical_header(), while the
    // inner mock's true head has a different hash, so its block-binding answer (block 1
    // for an unknown anchor) mismatches the advertised number.
    struct WrongBlockTransport {
        inner: MockTransport,
    }
    impl GasAuthorityTransport for WrongBlockTransport {
        fn provider_id(&self) -> u16 {
            self.inner.provider_id()
        }
        fn canonical_header(&self) -> Result<CanonicalHeader, GasTransportError> {
            Ok(header_a()) // advertised head
        }
        fn aggregate3_at_hash(
            &self,
            block_hash: [u8; 32],
            calls: &[AuthorityCall],
        ) -> Result<Vec<Vec<u8>>, GasTransportError> {
            self.inner.aggregate3_at_hash(block_hash, calls)
        }
    }
    let true_head = CanonicalHeader {
        number: header_a().number,
        hash: [0xEE; 32], // differs from the advertised header_a().hash
    };
    let t = WrongBlockTransport {
        inner: MockTransport::healthy(PROVIDER_A, true_head, &c),
    };

    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);
    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "an aggregate that provably executed at a different block must decline, got {out:?}"
    );
    assert!(o.authority().is_none());
    let health = o.authority_health();
    let attempt = health.last_attempt.expect("attempt recorded");
    assert_eq!(
        attempt.component_status,
        [GasComponentStatus::WrongBlock; 4],
        "wrong-block execution stamps all components WrongBlock"
    );
}

// ===========================================================================================
// 12. Deferred submit-binding interface (WI-14 consumes this; pipeline.rs NOT wired here).
// ===========================================================================================

// ===========================================================================================
// 13. Semantic plausibility (fix round, codex NEEDS-FIX-FL14 §correction 4): an ABI-valid,
//     positive, near-zero fee tuple must NEVER publish a fresh authority. Shape-validity and
//     `total == base + congestion` are not enough — a tiny quote that passes the legacy
//     `l1 > 0` broadcast filter would understate cost and inflate net profit.
// ===========================================================================================

/// All-ones tuple: every field positive, cross-check holds (1 = 1 + 0), every conversion
/// fits — yet 1 wei per ArbGas / 1 wei L1 base fee is semantically impossible on Arbitrum
/// One (ArbOS Dia enforces a 0.02 gwei L2 minimum). Publishing it as FRESH authority is the
/// same defect class as FL-14: a too-cheap cost basis with a fresh timestamp.
#[test]
fn near_zero_positive_fee_tuple_cannot_publish_fresh_authority() {
    let c = cfg();
    let o = oracle();
    let mut t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_PRICES_IN_WEI_WITH_AGGREGATOR)),
        words(&[1, 1, 1, 1, 0, 1]), // total(1) = base(1) + congestion(0): cross-check passes
    );
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR)),
        words(&[1, 1, 1]),
    );
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_MINIMUM_GAS_PRICE)),
        words(&[1]),
    );
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_L1_BASE_FEE_ESTIMATE)),
        words(&[1]),
    );

    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);
    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "a positive near-zero fee tuple must DECLINE publication (implausible), got {out:?}"
    );
    assert!(
        o.authority().is_none(),
        "no fresh authority may publish from an implausibly cheap quote"
    );
    assert_eq!(
        o.load().captured_at_ms,
        0,
        "the legacy snapshot must stay the untouched boot sentinel"
    );
    // Evidence pinpoints the implausible minimum (1 wei < the ArbOS Dia protocol floor).
    let attempt = o.authority_health().last_attempt.expect("attempt recorded");
    assert_eq!(
        attempt.component_status[COMPONENT_MINIMUM_GAS_PRICE],
        GasComponentStatus::ImplausibleValue
    );
}

/// Live tokenGasUnits == 0 would zero the entire L1 data fee. It is semantically impossible
/// (post-Dia real value ≈ 16-61) and must decline the component, not publish.
#[test]
fn zero_token_gas_units_declines_publication() {
    let c = cfg();
    let o = oracle();
    let mut t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    t.responses.insert(
        (
            c.arb_gas_info,
            selector(SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR),
        ),
        words(&[PER_L2_TX_ARB_GAS, 0, PER_STORAGE_ALLOC_ARB_GAS]),
    );
    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);
    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "tokenGasUnits == 0 must decline publication, got {out:?}"
    );
    assert!(o.authority().is_none());
    let attempt = o.authority_health().last_attempt.expect("attempt recorded");
    assert_eq!(
        attempt.component_status[COMPONENT_PRICES_IN_ARB_GAS],
        GasComponentStatus::ImplausibleValue
    );
}

/// L1 base-fee estimate == 0 must decline AT THE AUTHORITY, not lean on the downstream
/// legacy `l1 > 0` broadcast filter: a zero-L1 snapshot must never become fresh authority.
#[test]
fn zero_l1_base_fee_estimate_declines_publication() {
    let c = cfg();
    let o = oracle();
    let mut t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_L1_BASE_FEE_ESTIMATE)),
        words(&[0]),
    );
    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);
    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "a zero L1 base-fee estimate must decline publication, got {out:?}"
    );
    assert!(o.authority().is_none());
    assert_eq!(o.load().captured_at_ms, 0, "legacy snapshot untouched");
    let attempt = o.authority_health().last_attempt.expect("attempt recorded");
    assert_eq!(
        attempt.component_status[COMPONENT_L1_BASE_FEE_ESTIMATE],
        GasComponentStatus::ImplausibleValue
    );
}

/// The chain-derived relationship codex names: the per-ArbGas TOTAL price can never be below
/// the chain's own enforced minimum (`getMinimumGasPrice`). A quote violating it is
/// impossible under a compliant ArbOS and must decline.
#[test]
fn l2_total_below_chain_minimum_declines_publication() {
    let c = cfg();
    let o = oracle();
    let mut t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_PRICES_IN_WEI_WITH_AGGREGATOR)),
        words(&[
            PER_L2_TX_WEI,
            PER_L1_CALLDATA_UNIT_WEI,
            PER_STORAGE_ALLOC_WEI,
            1_000_000, // base far below the 20_000_000-wei minimum the same batch returns
            0,
            1_000_000, // total = base + congestion: cross-check passes
        ]),
    );
    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);
    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "per-ArbGas total below the fetched chain minimum must decline, got {out:?}"
    );
    assert!(o.authority().is_none());
    let attempt = o.authority_health().last_attempt.expect("attempt recorded");
    assert_eq!(
        attempt.component_status[COMPONENT_PRICES_IN_WEI],
        GasComponentStatus::ImplausibleValue
    );
}

/// Wei-per-L1-calldata-unit of 0 alongside a positive L1 base-fee estimate is internally
/// inconsistent (the wei price is basefee × token units, token units >= 1) — decline.
#[test]
fn zero_l1_calldata_unit_wei_with_positive_estimate_declines_publication() {
    let c = cfg();
    let o = oracle();
    let mut t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
    t.responses.insert(
        (c.arb_gas_info, selector(SIG_PRICES_IN_WEI_WITH_AGGREGATOR)),
        words(&[
            PER_L2_TX_WEI,
            0, // zero wei per L1 calldata unit — impossible with l1 estimate 3_100_000
            PER_STORAGE_ALLOC_WEI,
            PER_ARB_GAS_BASE_WEI,
            PER_ARB_GAS_CONGESTION_WEI,
            PER_ARB_GAS_TOTAL_WEI,
        ]),
    );
    let out = refresh_gas_authority_once(&t, &c, &o, 1_000, 10_000);
    assert!(
        matches!(out, RefreshOutcome::Failed { .. }),
        "zero per-L1-calldata-unit wei with a positive L1 estimate must decline, got {out:?}"
    );
    assert!(o.authority().is_none());
    let attempt = o.authority_health().last_attempt.expect("attempt recorded");
    assert_eq!(
        attempt.component_status[COMPONENT_PRICES_IN_WEI],
        GasComponentStatus::ImplausibleValue
    );
}

// ===========================================================================================
// 14. Publication bundling (fix round, codex NEEDS-FIX-FL14 §correction 3): a reader must
//     never observe an authority generation whose legacy fee bytes are not yet published —
//     the separate ArcSwap stores must not cross-bind generations.
// ===========================================================================================

/// Single sequential writer publishes N complete generations; `now_wall_ms` encodes the
/// generation (WALL_BASE + i), so a reader can detect a torn pair: if the SAME authority
/// generation `g` is observed immediately before and after loading the legacy snapshot, the
/// legacy bytes must already be at least generation `g`'s (captured_at_ms >= WALL_BASE +
/// (g - g0)). On a publication order that stores the authority before the legacy bridge,
/// readers observe `g` while the legacy snapshot still carries generation `g-1`'s bytes —
/// exactly the cross-bound pair WI-14's submit gate must never consume.
#[test]
fn reader_never_observes_authority_generation_ahead_of_its_legacy_bytes() {
    const WALL_BASE: u64 = 1_000_000;
    const REFRESHES: u64 = 20_000;

    let c = cfg();
    let o = Arc::new(oracle());
    let done = AtomicBool::new(false);
    let violations = AtomicU64::new(0);
    let g0 = AtomicU64::new(0);

    std::thread::scope(|s| {
        // Writer: seed + sequential complete refreshes, one generation per iteration.
        s.spawn(|| {
            let (_t, gen0) = seed_complete(&o, 1_000, WALL_BASE);
            g0.store(gen0.get(), Ordering::Release);
            for i in 1..=REFRESHES {
                let t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
                let out = refresh_gas_authority_once(&t, &c, &o, 1_000 + i, WALL_BASE + i);
                assert!(
                    matches!(out, RefreshOutcome::Complete { .. }),
                    "writer refresh {i} must be complete, got {out:?}"
                );
            }
            done.store(true, Ordering::Release);
        });
        // Readers: hammer (authority, legacy, authority) triples looking for a torn pair.
        for _ in 0..2 {
            s.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    let Some(a1) = o.authority() else { continue };
                    let legacy = o.load();
                    let Some(a2) = o.authority() else { continue };
                    if a1.generation != a2.generation {
                        continue; // a publication raced the triple — not a stable observation
                    }
                    let base = g0.load(Ordering::Acquire);
                    if base == 0 {
                        continue; // seed not yet recorded by the writer
                    }
                    let expected_wall = WALL_BASE + (a1.generation.get() - base);
                    if legacy.captured_at_ms < expected_wall {
                        violations.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    assert_eq!(
        violations.load(Ordering::Relaxed),
        0,
        "observed authority generation(s) whose legacy fee bytes were not yet published — \
         complete publication must be bundled/ordered so a reader can never cross-bind \
         an authority snapshot with another generation's legacy snapshot"
    );
}

/// Fix round (codex correction 3, concurrent half): two writers refreshing the SAME oracle
/// with different value sets. Publications must serialize (generation order == publication
/// order) and every bundle a reader loads must be internally generation-consistent — the
/// legacy fee bytes always belong to the exact authority snapshot they were loaded with,
/// never a mix of the two writers' values. Final quiescent state: bundle, legacy mirror
/// and health all agree on the last generation.
#[test]
fn concurrent_complete_refreshes_serialize_and_bundles_stay_coherent() {
    const PER_WRITER: u64 = 500;
    // Writer B's value set (must pass plausibility; distinct from the fixture on every
    // fee-critical field so a cross-bound pair is detectable).
    const B_TOKEN_GAS_UNITS: u64 = 40;
    const B_L1_ESTIMATE: u64 = 5_000_000;
    const B_CONGESTION: u64 = 5_000_000;
    const B_TOTAL: u64 = PER_ARB_GAS_BASE_WEI + B_CONGESTION;

    let c = cfg();
    let o = Arc::new(oracle());
    let done = AtomicBool::new(false);
    let torn = AtomicU64::new(0);

    let transport_b = || {
        let mut t = MockTransport::healthy(PROVIDER_B, header_b(), &c);
        t.responses.insert(
            (c.arb_gas_info, selector(SIG_PRICES_IN_WEI_WITH_AGGREGATOR)),
            words(&[
                PER_L2_TX_WEI,
                B_L1_ESTIMATE * B_TOKEN_GAS_UNITS,
                PER_STORAGE_ALLOC_WEI,
                PER_ARB_GAS_BASE_WEI,
                B_CONGESTION,
                B_TOTAL,
            ]),
        );
        t.responses.insert(
            (
                c.arb_gas_info,
                selector(SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR),
            ),
            words(&[PER_L2_TX_ARB_GAS, B_TOKEN_GAS_UNITS, PER_STORAGE_ALLOC_ARB_GAS]),
        );
        t.responses.insert(
            (c.arb_gas_info, selector(SIG_L1_BASE_FEE_ESTIMATE)),
            words(&[B_L1_ESTIMATE]),
        );
        t
    };

    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..PER_WRITER {
                let t = MockTransport::healthy(PROVIDER_A, header_a(), &c);
                let out = refresh_gas_authority_once(&t, &c, &o, 10_000 + i, 100_000 + i);
                assert!(matches!(out, RefreshOutcome::Complete { .. }), "A: {out:?}");
            }
        });
        s.spawn(|| {
            for i in 0..PER_WRITER {
                let t = transport_b();
                let out = refresh_gas_authority_once(&t, &c, &o, 20_000 + i, 200_000 + i);
                assert!(matches!(out, RefreshOutcome::Complete { .. }), "B: {out:?}");
            }
            done.store(true, Ordering::Release);
        });
        // Reader: every bundle must bind legacy bytes to ITS OWN authority snapshot.
        s.spawn(|| {
            while !done.load(Ordering::Acquire) {
                let Ok(p) = o.usable_publication(0, u64::MAX) else {
                    continue;
                };
                let a = &p.authority.checked;
                let coherent = p.legacy.l2_gas_price_wei == a.per_arb_gas_total_wei
                    && p.legacy.l1_base_fee_per_gas_wei == a.l1_base_fee_estimate_wei
                    && p.legacy.l1_gas_per_data_byte as u64 == a.per_l1_calldata_unit_arb_gas;
                // The pair must also be one of the two writers' EXACT value sets.
                let is_a = a.l1_base_fee_estimate_wei == L1_BASE_FEE_ESTIMATE_WEI as u128
                    && a.per_l1_calldata_unit_arb_gas == TOKEN_GAS_UNITS_LIVE;
                let is_b = a.l1_base_fee_estimate_wei == B_L1_ESTIMATE as u128
                    && a.per_l1_calldata_unit_arb_gas == B_TOKEN_GAS_UNITS;
                if !(coherent && (is_a || is_b)) {
                    torn.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    });

    assert_eq!(
        torn.load(Ordering::Relaxed),
        0,
        "a reader observed a bundle mixing values/generations across concurrent refreshes"
    );
    // Quiescent agreement: publication order == generation order, and bundle, legacy
    // mirror and health all describe the same final complete generation.
    let last = o.usable_publication(0, u64::MAX).expect("seeded");
    assert_eq!(
        last.authority.generation.get(),
        2 * PER_WRITER,
        "serialized writers must consume exactly one generation per publication"
    );
    assert_eq!(
        o.authority_health().last_complete_generation,
        Some(last.authority.generation),
        "health must agree with the final published generation"
    );
    let mirror = o.load();
    assert_eq!(mirror.l2_gas_price_wei, last.legacy.l2_gas_price_wei);
    assert_eq!(mirror.l1_base_fee_per_gas_wei, last.legacy.l1_base_fee_per_gas_wei);
    assert_eq!(mirror.l1_gas_per_data_byte, last.legacy.l1_gas_per_data_byte);
    assert_eq!(mirror.captured_at_ms, last.legacy.captured_at_ms);
}

#[test]
fn submit_authority_view_exposes_generation_for_prepared_submission_binding() {
    let o = oracle();
    assert!(
        o.submit_authority_view().is_none(),
        "no submit authority before the first complete snapshot"
    );
    let (_t, generation) = seed_complete(&o, 1_000, 10_000);
    let view = o
        .submit_authority_view()
        .expect("complete snapshot must expose the submit-binding view");
    assert_eq!(view.snapshot_generation, generation);
    assert_eq!(view.canonical_block_hash, header_a().hash);
    assert_eq!(view.fee_aggregator_identity_hash, keccak256(AGGREGATOR).0);
    assert_eq!(view.captured_at_mono_ns, 1_000);
}
