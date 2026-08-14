//! WI-13 — one-block, component-complete gas authority (codex plan §3.4; closes FL-14).
//!
//! This is a SURGICAL extension of the existing gas oracle, not a parallel gas subsystem:
//! the [`GasOracle`] keeps its ArcSwap read model, `GasSnapshot` hot-path shape, Brotli L1
//! path, volatility tracker and staleness API unchanged — WI-13 adds the typed
//! [`GasAuthoritySnapshot`] the oracle now also publishes, and FIXES the refresh failure
//! semantics:
//!
//! * FL-14 (CONFIRMED): the legacy `gas_refresher` L1 leg was fail-open — a failed L1-fee
//!   fetch substituted a fresh 100,000-wei floor (~30x under real L1 cost), recorded it as
//!   a real sample and published it with a fresh `captured_at_ms` and no invalidity bit.
//!   Now a failed/partial component keeps the OLD value AND the OLD age and marks the new
//!   generation incomplete (health evidence only); admission ages against the snapshot's
//!   ORIGINAL capture time plus a hard TTL. Failure can never make authority fresh, and
//!   the 100k floor is dead.
//! * Every fee-critical field is fetched at ONE EIP-1898 block hash through a single
//!   Multicall3 `aggregate3` with `allowFailure=false`, using ONLY the `WithAggregator`
//!   price calls — the plain variants are FORBIDDEN through Multicall because `msg.sender`
//!   would be Multicall3, not the configured aggregator.
//! * `U256` conversions are checked — a value that does not fit DECLINES the snapshot
//!   (never clamps/saturates).
//! * `tokenGasUnits` is read live (`getPricesInArbGasWithAggregator` tuple 1) at the
//!   snapshot block.
//! * Fee-critical values are SEMANTICALLY validated (fix round, codex NEEDS-FIX-FL14): an
//!   ABI-valid but implausibly cheap tuple (near-zero prices, a total below the chain's own
//!   minimum, zero tokenGasUnits/L1 estimate) DECLINES publication — a valid-shaped tiny
//!   quote can never become fresh authority.
//! * Publication is SERIALIZED and BUNDLED (fix round): each complete refresh stores ONE
//!   immutable [`GasAuthorityPublication`] (typed snapshot + same-generation legacy bridge)
//!   under the oracle's publish lock, with the legacy hot-path mirror stored FIRST — a
//!   reader can never observe an authority generation whose legacy fee bytes are not yet
//!   published, and concurrent refreshes cannot interleave generations.
//!
//! §3.4 retired-constant rule, AMENDED (fix round; operator-visible): the literal claim
//! "16/140 survive only in diagnostic tests" was FALSE and is replaced by this contract —
//! * `16`/`140` remain in the INERT boot-default [`GasSnapshot`] (its `captured_at_ms == 0`
//!   sentinel is permanently Stale, and with the authority as the engine's SOLE gas writer
//!   nothing can ever stamp it fresh: the first fresh snapshot is a COMPLETE publication
//!   carrying live values);
//! * `140` ([`DEFAULT_LEGACY_STATIC_OVERHEAD_BYTES`]) is the POLICY-BOUND envelope pad: an
//!   explicit config default ([`GasAuthorityConfig::legacy_static_overhead_bytes`]), logged
//!   at refresher boot and carried as evidence in every published snapshot's
//!   `static_overhead_bytes` field — safe by direction (over-states a subtracted cost);
//! * `100_000` (the FL-14 floor) has NO production successor.
//!
//! DEFERRED (critical-path / WI-14): binding `generation` into `PreparedSubmission` at
//! prepare time and the exact-generation recheck before nonce claim / wire write live in
//! `pipeline.rs` and are NOT implemented here. [`GasOracle::submit_authority_view`] and the
//! generation-consistent [`GasOracle::usable_publication`] are the frozen interfaces that
//! integration consumes.

use core::num::NonZeroU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use alloy_primitives::{keccak256, U256};

use crate::{GasOracle, GasSnapshot};

// ---------------------------------------------------------------------------------------------
// ABI surface (official ArbGasInfo interface, OffchainLabs/nitro-precompile-interfaces).
// ---------------------------------------------------------------------------------------------

/// `ArbGasInfo` precompile address (`0x…6C`) on Arbitrum chains.
pub const ARB_GAS_INFO_ADDRESS: [u8; 20] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x6C,
];

/// Canonical Multicall3 deployment on Arbitrum One (mirrors
/// `engine_rpc::multicall::MULTICALL3_ARBITRUM`; the production wiring asserts equality).
pub const MULTICALL3_ARBITRUM_ONE: [u8; 20] = [
    0xcA, 0x11, 0xbd, 0xe0, 0x59, 0x77, 0xb3, 0x63, 0x11, 0x67, 0x02, 0x88, 0x62, 0xbE, 0x2a,
    0x17, 0x39, 0x76, 0xCA, 0x11,
];

/// `getPricesInWeiWithAggregator(address)` — the ONLY permitted wei-price call via Multicall.
pub const SIG_PRICES_IN_WEI_WITH_AGGREGATOR: &str = "getPricesInWeiWithAggregator(address)";
/// `getPricesInArbGasWithAggregator(address)` — the ONLY permitted ArbGas-price call via
/// Multicall. Its second tuple element is the LIVE `tokenGasUnits` input (codex §3.4).
pub const SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR: &str =
    "getPricesInArbGasWithAggregator(address)";
/// `getMinimumGasPrice()`.
pub const SIG_MINIMUM_GAS_PRICE: &str = "getMinimumGasPrice()";
/// `getL1BaseFeeEstimate()`.
pub const SIG_L1_BASE_FEE_ESTIMATE: &str = "getL1BaseFeeEstimate()";
/// Multicall3 `getBlockNumber()` — appended to the same aggregate so the response proves the
/// batch executed at the requested canonical block (one-block binding evidence).
pub const SIG_MULTICALL3_GET_BLOCK_NUMBER: &str = "getBlockNumber()";
/// FORBIDDEN through Multicall: `msg.sender` would be Multicall3, not our aggregator.
pub const FORBIDDEN_SIG_PRICES_IN_WEI: &str = "getPricesInWei()";
/// FORBIDDEN through Multicall: `msg.sender` would be Multicall3, not our aggregator.
pub const FORBIDDEN_SIG_PRICES_IN_ARB_GAS: &str = "getPricesInArbGas()";

/// 4-byte function selector of a canonical signature string.
pub fn selector(signature: &str) -> [u8; 4] {
    let h = keccak256(signature.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

// ---------------------------------------------------------------------------------------------
// Component identity.
// ---------------------------------------------------------------------------------------------

/// Component 0: `getPricesInWeiWithAggregator` (6-tuple, wei).
pub const COMPONENT_PRICES_IN_WEI: usize = 0;
/// Component 1: `getPricesInArbGasWithAggregator` (3-tuple, ArbGas; live tokenGasUnits).
pub const COMPONENT_PRICES_IN_ARB_GAS: usize = 1;
/// Component 2: `getMinimumGasPrice`.
pub const COMPONENT_MINIMUM_GAS_PRICE: usize = 2;
/// Component 3: `getL1BaseFeeEstimate`.
pub const COMPONENT_L1_BASE_FEE_ESTIMATE: usize = 3;
/// All four fee-critical components present.
pub const COMPLETE_COMPONENT_MASK: u16 = 0b1111;

// ---------------------------------------------------------------------------------------------
// Typed contract (codex §3.4).
// ---------------------------------------------------------------------------------------------

/// Classification of an RPC-level component failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcErrorClass {
    /// Transport / IO / HTTP failure.
    Transport,
    /// Provider-reported revert or error envelope.
    Revert,
    /// Anything else.
    Other,
}

/// Per-component fetch/validation status. A snapshot publishes ONLY when all four are `Valid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GasComponentStatus {
    /// Component fetched, decoded, cross-checked and range-checked.
    Valid,
    /// RPC-level failure (whole-aggregate failures stamp all components).
    RpcError(RpcErrorClass),
    /// Return bytes did not decode to the expected tuple shape.
    DecodeError,
    /// The aggregate did not execute at the requested canonical block.
    WrongBlock,
    /// Response attributed to a different provider than the attempt's.
    WrongProvider,
    /// Response bound to a different fee aggregator than the frozen one.
    WrongAggregator,
    /// A checked `U256` conversion did not fit the target width (declines — never clamps).
    Overflow,
    /// A documented tuple relationship failed (e.g. `total != base + congestion`).
    CrossCheckMismatch,
    /// Fix round (codex NEEDS-FIX-FL14): the value is ABI-valid but semantically impossible
    /// on the target chain (near-zero fee, total below the fetched chain minimum, zero
    /// tokenGasUnits / L1 estimate). Declines — a valid-shaped tiny quote must never become
    /// fresh authority.
    ImplausibleValue,
}

/// Raw `ArbGasInfo` fields, kept as `U256` until the checked boundary. Field names preserve
/// the official ABI tuple order/semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArbGasInfoRaw {
    /// Wei per L2 transaction (tuple 0 of `getPricesInWeiWithAggregator`).
    pub per_l2_tx_wei: U256,
    /// Wei per L1 calldata unit (tuple 1).
    pub per_l1_calldata_unit_wei: U256,
    /// Wei per storage allocation (tuple 2).
    pub per_storage_allocation_wei: U256,
    /// Wei per ArbGas, base component (tuple 3).
    pub per_arb_gas_base_wei: U256,
    /// Wei per ArbGas, congestion component (tuple 4).
    pub per_arb_gas_congestion_wei: U256,
    /// Wei per ArbGas, total (tuple 5). Cross-checked = base + congestion.
    pub per_arb_gas_total_wei: U256,
    /// ArbGas per L2 transaction (tuple 0 of `getPricesInArbGasWithAggregator`).
    pub per_l2_tx_arb_gas: U256,
    /// ArbGas per L1 calldata unit (tuple 1) — the LIVE `tokenGasUnits` input.
    pub per_l1_calldata_unit_arb_gas: U256,
    /// ArbGas per storage allocation (tuple 2).
    pub per_storage_allocation_arb_gas: U256,
    /// `getMinimumGasPrice()`.
    pub minimum_l2_gas_price_wei: U256,
    /// `getL1BaseFeeEstimate()` — typed cross-check / volatility input, NOT an excuse to
    /// reconstruct admission truth as `basefee * 16`.
    pub l1_base_fee_estimate_wei: U256,
}

/// Checked (non-saturating) view of [`ArbGasInfoRaw`]. Exists ONLY on a complete snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GasPricingView {
    /// Wei per L2 transaction.
    pub per_l2_tx_wei: u128,
    /// Wei per L1 calldata unit.
    pub per_l1_calldata_unit_wei: u128,
    /// Wei per ArbGas, total — the L2 gas price the legacy `GasSnapshot` bridge publishes.
    /// Additive to the codex §3.4 view (the existing `l2_gas_price_wei` consumer needs it
    /// checked).
    pub per_arb_gas_total_wei: u128,
    /// Live `tokenGasUnits` input (ArbGas per L1 calldata unit).
    pub per_l1_calldata_unit_arb_gas: u64,
    /// Wei per storage allocation.
    pub per_storage_allocation_wei: u128,
    /// Minimum L2 gas price in wei.
    pub minimum_l2_gas_price_wei: u128,
    /// L1 base-fee estimate in wei per L1 gas.
    pub l1_base_fee_estimate_wei: u128,
}

/// Complete-only, immutable, generation-stamped gas authority (codex §3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GasAuthoritySnapshot {
    /// Schema version of this struct.
    pub schema_version: u16,
    /// Monotonic generation, advanced ONLY by a complete publication.
    pub generation: NonZeroU64,
    /// Canonical block number the whole snapshot was fetched at.
    pub canonical_block_number: u64,
    /// Canonical block hash (the EIP-1898 anchor of the one-block fetch).
    pub canonical_block_hash: [u8; 32],
    /// Provider that served the WHOLE attempt (never mixed).
    pub provider_id: u16,
    /// Frozen fee-aggregator address used in the `WithAggregator` calls.
    pub fee_aggregator: [u8; 20],
    /// keccak256 of [`Self::fee_aggregator`] — the aggregator identity bound into evidence.
    pub fee_aggregator_identity_hash: [u8; 32],
    /// Raw `ArbGasInfo` fields (U256 until the checked boundary).
    pub raw: ArbGasInfoRaw,
    /// Checked view (exists only because the snapshot is complete).
    pub checked: GasPricingView,
    /// Per-component status — all `Valid` on a published snapshot.
    pub component_status: [GasComponentStatus; 4],
    /// Present-component mask — [`COMPLETE_COMPONENT_MASK`] on a published snapshot.
    pub complete_component_mask: u16,
    /// Monotonic capture time of THIS snapshot. Admission ages against this; a failed later
    /// refresh never renews it.
    pub captured_at_mono_ns: u64,
    /// Monotonic time of the most recent COMPLETE publication (== `captured_at_mono_ns`).
    pub last_complete_at_mono_ns: u64,
}

/// Evidence record of one refresh attempt (success or failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GasRefreshAttempt {
    /// Canonical block number the attempt requested (0 if header selection itself failed).
    pub requested_block_number: u64,
    /// Canonical block hash the attempt requested (zeroed if header selection failed).
    pub requested_block_hash: [u8; 32],
    /// Provider used for the whole attempt.
    pub provider_id: u16,
    /// Identity hash of the frozen aggregator the attempt used.
    pub fee_aggregator_identity_hash: [u8; 32],
    /// Per-component outcome of the attempt.
    pub component_status: [GasComponentStatus; 4],
    /// Monotonic time of the attempt.
    pub attempted_at_mono_ns: u64,
}

/// Coarse authority stream state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GasStreamState {
    /// No complete snapshot has ever been published.
    Seeding,
    /// At least one complete snapshot exists.
    Live,
}

/// Health/evidence record — separate from fee bytes; updated on EVERY attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GasHealth {
    /// Monotonic health generation (advances on every attempt, success or failure).
    pub health_generation: NonZeroU64,
    /// Generation of the last COMPLETE publication, if any.
    pub last_complete_generation: Option<NonZeroU64>,
    /// The most recent attempt record.
    pub last_attempt: Option<GasRefreshAttempt>,
    /// Stream state.
    pub stream_state: GasStreamState,
    /// Sticky hard-conflict bit (e.g. provider disagreement); declines admission.
    pub hard_conflict: bool,
}

impl GasHealth {
    /// Never-seeded initial health.
    pub(crate) fn seeding() -> Self {
        Self {
            health_generation: NonZeroU64::MIN,
            last_complete_generation: None,
            last_attempt: None,
            stream_state: GasStreamState::Seeding,
            hard_conflict: false,
        }
    }
}

/// Admission errors for [`GasOracle::usable_authority`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GasAuthorityError {
    /// No complete snapshot has ever been published.
    #[error("gas authority never seeded")]
    NeverSeeded,
    /// The last complete snapshot's ORIGINAL age exceeds the hard TTL.
    #[error("gas authority expired: age {age_ns}ns > ttl {ttl_ns}ns")]
    Expired {
        /// Age of the snapshot (vs its original capture time).
        age_ns: u64,
        /// The hard TTL that was exceeded.
        ttl_ns: u64,
    },
    /// The sticky hard-conflict bit is set.
    #[error("gas authority hard conflict")]
    HardConflict,
}

/// One COMPLETE publication as a single immutable unit (fix round, codex NEEDS-FIX-FL14):
/// the typed authority snapshot and the legacy [`GasSnapshot`] bridged from the SAME
/// generation, stored via ONE ArcSwap store. A paired reader (the WI-14 submit binding)
/// loads this bundle once and can therefore never cross-bind an authority snapshot with
/// another generation's legacy fee bytes. The hot path keeps reading the uncorrelated
/// legacy mirror via [`GasOracle::load`]; generation-sensitive consumers MUST read the
/// bundle ([`GasOracle::usable_publication`]).
#[derive(Debug, Clone)]
pub struct GasAuthorityPublication {
    /// The complete typed authority snapshot.
    pub authority: Arc<GasAuthoritySnapshot>,
    /// The legacy bridge published FROM this exact snapshot (same generation).
    pub legacy: GasSnapshot,
}

/// The frozen view the submit path binds into `PreparedSubmission` (WI-14 / critical-path
/// integration — NOT wired here; see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GasSubmitAuthorityView {
    /// Generation that must match exactly at submit-time recheck.
    pub snapshot_generation: NonZeroU64,
    /// Canonical block hash the fee authority was fetched at.
    pub canonical_block_hash: [u8; 32],
    /// Frozen aggregator identity.
    pub fee_aggregator_identity_hash: [u8; 32],
    /// Original capture time (admission ages against this).
    pub captured_at_mono_ns: u64,
}

// ---------------------------------------------------------------------------------------------
// Transport abstraction (mock-first; the production impl wraps `engine-rpc`).
// ---------------------------------------------------------------------------------------------

/// A canonical `(number, hash)` header selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalHeader {
    /// Block number.
    pub number: u64,
    /// Block hash.
    pub hash: [u8; 32],
}

/// Transport-level failure for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GasTransportError {
    /// RPC / IO failure with a human-readable reason.
    #[error("gas transport error: {0}")]
    Rpc(String),
}

/// One call inside the single authority aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityCall {
    /// Target contract.
    pub target: [u8; 20],
    /// Multicall3 `allowFailure` flag — MUST be `false` for authority calls.
    pub allow_failure: bool,
    /// Selector + ABI-encoded arguments.
    pub call_data: Vec<u8>,
}

/// One-provider, one-block transport. Provider failover constructs a NEW transport and
/// restarts the WHOLE snapshot — components and heads are never mixed.
pub trait GasAuthorityTransport {
    /// Stable id of the provider serving this whole attempt.
    fn provider_id(&self) -> u16;
    /// Select and verify the canonical header for this attempt.
    fn canonical_header(&self) -> Result<CanonicalHeader, GasTransportError>;
    /// Execute ONE Multicall3 `aggregate3` at the EIP-1898 `block_hash`, returning one
    /// return-data blob per call (in order). `allowFailure=false` semantics: an inner
    /// failure fails the whole aggregate.
    fn aggregate3_at_hash(
        &self,
        block_hash: [u8; 32],
        calls: &[AuthorityCall],
    ) -> Result<Vec<Vec<u8>>, GasTransportError>;
}

// ---------------------------------------------------------------------------------------------
// Config.
// ---------------------------------------------------------------------------------------------

/// Legacy empirical per-tx envelope pad (bytes) carried into the legacy [`GasSnapshot`]
/// bridge. Sized to over-state (never under-state) signature/envelope overhead; safe by
/// direction (the L1 fee is a subtracted cost). It has no on-chain source, so it is an
/// explicit configured value here — not a hardcode buried in the refresher.
///
/// §3.4 AMENDED policy binding (fix round): this pad is a LIVE production default — it flows
/// through [`GasAuthorityConfig::legacy_static_overhead_bytes`] onto every complete
/// publication, is logged at refresher boot, and every published snapshot carries the value
/// actually used in its `static_overhead_bytes` field (per-publication evidence). See the
/// module docs for the full amended retired-constant contract.
pub const DEFAULT_LEGACY_STATIC_OVERHEAD_BYTES: u32 = 140;

/// ArbOS 51 "Dia" protocol minimum L2 base fee (0.02 gwei, activated 2026-01-08) — the
/// absolute plausibility anchor for fee-critical values: a `getMinimumGasPrice` below the
/// protocol's own enforced floor is semantically impossible on Arbitrum One, so a quote
/// carrying one DECLINES (fail-closed; a governance change lowering the floor would surface
/// as declined refreshes + health evidence, never as silently-accepted cheap authority).
pub const ARBOS_MIN_L2_BASE_FEE_FLOOR_WEI: u128 = 20_000_000;

/// Static configuration of the authority fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GasAuthorityConfig {
    /// `ArbGasInfo` precompile address.
    pub arb_gas_info: [u8; 20],
    /// Multicall3 deployment address.
    pub multicall3: [u8; 20],
    /// Frozen fee-aggregator identity used in ALL `WithAggregator` calls. Bound into the
    /// snapshot; a different aggregator can never reuse this authority.
    pub frozen_aggregator: [u8; 20],
    /// Envelope pad for the legacy bridge (see [`DEFAULT_LEGACY_STATIC_OVERHEAD_BYTES`]).
    pub legacy_static_overhead_bytes: u32,
}

impl GasAuthorityConfig {
    /// Arbitrum One canonical addresses with the given frozen aggregator.
    pub fn arbitrum_one(frozen_aggregator: [u8; 20]) -> Self {
        Self {
            arb_gas_info: ARB_GAS_INFO_ADDRESS,
            multicall3: MULTICALL3_ARBITRUM_ONE,
            frozen_aggregator,
            legacy_static_overhead_bytes: DEFAULT_LEGACY_STATIC_OVERHEAD_BYTES,
        }
    }

    /// keccak256 identity hash of the frozen aggregator.
    pub fn aggregator_identity_hash(&self) -> [u8; 32] {
        keccak256(self.frozen_aggregator).0
    }
}

// ---------------------------------------------------------------------------------------------
// Oracle extension: authority read model (reuses the existing GasOracle object).
// ---------------------------------------------------------------------------------------------

impl GasOracle {
    /// Wait-free authority read. `None` until the first complete publication.
    pub fn authority(&self) -> Option<Arc<GasAuthoritySnapshot>> {
        self.authority.load_full().map(|p| p.authority.clone())
    }

    /// Wait-free authority health read.
    pub fn authority_health(&self) -> Arc<GasHealth> {
        self.authority_health.load_full()
    }

    /// Admission: the last complete snapshot, usable ONLY within its ORIGINAL hard TTL and
    /// with no hard conflict. A failed refresh never renews the age.
    pub fn usable_authority(
        &self,
        now_mono_ns: u64,
        hard_ttl_ns: u64,
    ) -> Result<Arc<GasAuthoritySnapshot>, GasAuthorityError> {
        self.usable_publication(now_mono_ns, hard_ttl_ns)
            .map(|p| p.authority.clone())
    }

    /// Admission over the BUNDLED publication (fix round): one ArcSwap load returns the
    /// typed authority snapshot AND the legacy fee bytes of the SAME generation — the read
    /// WI-14's prepare/submit binding must use so it can never pair an authority generation
    /// with another generation's legacy snapshot. Same TTL/conflict semantics as
    /// [`Self::usable_authority`].
    pub fn usable_publication(
        &self,
        now_mono_ns: u64,
        hard_ttl_ns: u64,
    ) -> Result<Arc<GasAuthorityPublication>, GasAuthorityError> {
        if self.authority_health.load().hard_conflict {
            return Err(GasAuthorityError::HardConflict);
        }
        let publication = self
            .authority
            .load_full()
            .ok_or(GasAuthorityError::NeverSeeded)?;
        let age_ns = now_mono_ns.saturating_sub(publication.authority.captured_at_mono_ns);
        if age_ns > hard_ttl_ns {
            return Err(GasAuthorityError::Expired {
                age_ns,
                ttl_ns: hard_ttl_ns,
            });
        }
        Ok(publication)
    }

    /// The frozen submit-binding view (WI-14 integration consumes this — see module docs).
    pub fn submit_authority_view(&self) -> Option<GasSubmitAuthorityView> {
        self.authority.load_full().map(|p| GasSubmitAuthorityView {
            snapshot_generation: p.authority.generation,
            canonical_block_hash: p.authority.canonical_block_hash,
            fee_aggregator_identity_hash: p.authority.fee_aggregator_identity_hash,
            captured_at_mono_ns: p.authority.captured_at_mono_ns,
        })
    }

    fn bump_authority_health_generation(&self) -> NonZeroU64 {
        let g = self.next_health_generation.fetch_add(1, Ordering::AcqRel);
        NonZeroU64::new(g).unwrap_or(NonZeroU64::MIN)
    }

    /// Record a failed/partial refresh attempt: health evidence ONLY. The authority
    /// snapshot, its generation and capture time, and the legacy `GasSnapshot` stay
    /// byte-for-byte unchanged (the FL-14 fix). Serialized under the publish lock so a
    /// failed attempt's health record cannot interleave with a concurrent complete
    /// publication's stores.
    fn record_failed_gas_attempt(&self, attempt: GasRefreshAttempt) {
        let _publish = self
            .publish_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev = self.authority_health.load_full();
        let health_generation = self.bump_authority_health_generation();
        self.authority_health.store(Arc::new(GasHealth {
            health_generation,
            last_complete_generation: prev.last_complete_generation,
            last_attempt: Some(attempt),
            stream_state: prev.stream_state,
            hard_conflict: prev.hard_conflict,
        }));
    }
}

/// Outcome of one refresh attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// A complete snapshot was published under `generation`.
    Complete {
        /// The new fee generation.
        generation: NonZeroU64,
    },
    /// The attempt failed/was partial; ONLY health evidence changed.
    Failed {
        /// Per-component outcome of the failed attempt.
        component_status: [GasComponentStatus; 4],
    },
}

// ---------------------------------------------------------------------------------------------
// Call building / decoding.
// ---------------------------------------------------------------------------------------------

/// ABI-encode `selector(address)` calldata.
fn encode_address_call(sig: &str, address: [u8; 20]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32);
    out.extend_from_slice(&selector(sig));
    let mut word = [0u8; 32];
    word[12..32].copy_from_slice(&address);
    out.extend_from_slice(&word);
    out
}

/// Build the single authority aggregate: exactly 5 calls, all `allow_failure=false` —
/// `[getPricesInWeiWithAggregator(frozen), getPricesInArbGasWithAggregator(frozen),
/// getMinimumGasPrice(), getL1BaseFeeEstimate(), Multicall3.getBlockNumber()]`.
///
/// The plain (aggregator-less) price selectors NEVER appear here: through Multicall3 their
/// `msg.sender` would be Multicall3, not the configured aggregator.
pub fn build_authority_calls(cfg: &GasAuthorityConfig) -> Vec<AuthorityCall> {
    vec![
        AuthorityCall {
            target: cfg.arb_gas_info,
            allow_failure: false,
            call_data: encode_address_call(
                SIG_PRICES_IN_WEI_WITH_AGGREGATOR,
                cfg.frozen_aggregator,
            ),
        },
        AuthorityCall {
            target: cfg.arb_gas_info,
            allow_failure: false,
            call_data: encode_address_call(
                SIG_PRICES_IN_ARB_GAS_WITH_AGGREGATOR,
                cfg.frozen_aggregator,
            ),
        },
        AuthorityCall {
            target: cfg.arb_gas_info,
            allow_failure: false,
            call_data: selector(SIG_MINIMUM_GAS_PRICE).to_vec(),
        },
        AuthorityCall {
            target: cfg.arb_gas_info,
            allow_failure: false,
            call_data: selector(SIG_L1_BASE_FEE_ESTIMATE).to_vec(),
        },
        AuthorityCall {
            target: cfg.multicall3,
            allow_failure: false,
            call_data: selector(SIG_MULTICALL3_GET_BLOCK_NUMBER).to_vec(),
        },
    ]
}

/// Decode `N` consecutive 32-byte words as `U256`s. `None` on short/oversized payloads.
fn decode_u256_words<const N: usize>(bytes: &[u8]) -> Option<[U256; N]> {
    if bytes.len() != 32 * N {
        return None;
    }
    let mut out = [U256::ZERO; N];
    for (i, slot) in out.iter_mut().enumerate() {
        let mut w = [0u8; 32];
        w.copy_from_slice(&bytes[i * 32..(i + 1) * 32]);
        *slot = U256::from_be_bytes(w);
    }
    Some(out)
}

/// Checked `U256 → u128`. `None` on overflow — the caller DECLINES (never clamps).
fn u256_to_u128(v: U256) -> Option<u128> {
    v.try_into().ok()
}

/// Checked `U256 → u64`. `None` on overflow — the caller DECLINES (never clamps).
fn u256_to_u64(v: U256) -> Option<u64> {
    v.try_into().ok()
}

/// Bridge a complete authority snapshot into the EXISTING legacy [`GasSnapshot`] shape (the
/// hot path, Brotli L1 path and volatility margin consume it unchanged). The live
/// `tokenGasUnits` replaces the retired hardcoded `16`; the retired fresh-on-failure
/// 100k-wei floor has no successor here — this bridge is reachable ONLY from a complete
/// snapshot.
fn bridge_to_legacy(
    snapshot: &GasAuthoritySnapshot,
    cfg: &GasAuthorityConfig,
    token_gas_units_u32: u32,
    l1_sigma_bps: u64,
    now_wall_ms: u64,
) -> GasSnapshot {
    GasSnapshot {
        l2_gas_price_wei: snapshot.checked.per_arb_gas_total_wei,
        l1_base_fee_per_gas_wei: snapshot.checked.l1_base_fee_estimate_wei,
        l1_gas_per_data_byte: token_gas_units_u32,
        min_l2_base_fee_wei: snapshot.checked.minimum_l2_gas_price_wei,
        static_overhead_bytes: cfg.legacy_static_overhead_bytes,
        balancer_flash_fee_1e18: U256::ZERO,
        captured_at_ms: now_wall_ms,
        l1_sigma_bps,
    }
}

// ---------------------------------------------------------------------------------------------
// The one refresh entry point (called by `engine`'s gas_refresher with the real transport).
// ---------------------------------------------------------------------------------------------

/// Run ONE whole-snapshot refresh attempt through `transport` and publish the outcome onto
/// the EXISTING oracle.
///
/// GREEN contract:
/// * complete → publish the typed authority snapshot (new generation) + health, bridge it
///   into the legacy [`GasSnapshot`] via [`GasOracle::store`], and feed the volatility
///   tracker exactly one real sample;
/// * failed/partial → record ONLY health evidence; the authority snapshot, its generation,
///   its capture time AND the legacy snapshot stay byte-for-byte unchanged; NO floor
///   substitution, NO synthetic volatility sample.
pub fn refresh_gas_authority_once(
    transport: &dyn GasAuthorityTransport,
    cfg: &GasAuthorityConfig,
    oracle: &GasOracle,
    now_mono_ns: u64,
    now_wall_ms: u64,
) -> RefreshOutcome {
    let identity = cfg.aggregator_identity_hash();
    let fail = |header: Option<CanonicalHeader>, status: [GasComponentStatus; 4]| {
        let attempt = GasRefreshAttempt {
            requested_block_number: header.map(|h| h.number).unwrap_or(0),
            requested_block_hash: header.map(|h| h.hash).unwrap_or([0u8; 32]),
            provider_id: transport.provider_id(),
            fee_aggregator_identity_hash: identity,
            component_status: status,
            attempted_at_mono_ns: now_mono_ns,
        };
        oracle.record_failed_gas_attempt(attempt);
        RefreshOutcome::Failed {
            component_status: status,
        }
    };
    let all = |s: GasComponentStatus| [s; 4];

    // 1. Select the canonical header for this attempt (one provider, one head).
    let header = match transport.canonical_header() {
        Ok(h) => h,
        Err(_) => return fail(None, all(GasComponentStatus::RpcError(RpcErrorClass::Transport))),
    };

    // 2. ONE Multicall3 aggregate at the EIP-1898 hash, allowFailure=false.
    let calls = build_authority_calls(cfg);
    let payloads = match transport.aggregate3_at_hash(header.hash, &calls) {
        Ok(p) => p,
        Err(_) => {
            return fail(
                Some(header),
                all(GasComponentStatus::RpcError(RpcErrorClass::Transport)),
            )
        }
    };
    if payloads.len() != calls.len() {
        return fail(Some(header), all(GasComponentStatus::DecodeError));
    }

    // 3. One-block binding evidence: the batch's own getBlockNumber() must equal the
    //    requested canonical header's number (the batch executed AT that block).
    let executed_block = decode_u256_words::<1>(&payloads[4]).and_then(|[v]| u256_to_u64(v));
    match executed_block {
        Some(n) if n == header.number => {}
        _ => return fail(Some(header), all(GasComponentStatus::WrongBlock)),
    }

    // 4. Per-component decode → raw U256 (no defaults, no floors: any hole declines).
    let mut status = [GasComponentStatus::Valid; 4];
    let wei6 = decode_u256_words::<6>(&payloads[COMPONENT_PRICES_IN_WEI]);
    if wei6.is_none() {
        status[COMPONENT_PRICES_IN_WEI] = GasComponentStatus::DecodeError;
    }
    let ag3 = decode_u256_words::<3>(&payloads[COMPONENT_PRICES_IN_ARB_GAS]);
    if ag3.is_none() {
        status[COMPONENT_PRICES_IN_ARB_GAS] = GasComponentStatus::DecodeError;
    }
    let min1 = decode_u256_words::<1>(&payloads[COMPONENT_MINIMUM_GAS_PRICE]);
    if min1.is_none() {
        status[COMPONENT_MINIMUM_GAS_PRICE] = GasComponentStatus::DecodeError;
    }
    let l1e = decode_u256_words::<1>(&payloads[COMPONENT_L1_BASE_FEE_ESTIMATE]);
    if l1e.is_none() {
        status[COMPONENT_L1_BASE_FEE_ESTIMATE] = GasComponentStatus::DecodeError;
    }
    let (Some(wei6), Some(ag3), Some([min_raw]), Some([l1_raw])) = (wei6, ag3, min1, l1e) else {
        return fail(Some(header), status);
    };

    // 5. Documented tuple relationship: total = base + congestion (checked add).
    let base_plus_congestion = wei6[3].checked_add(wei6[4]);
    if base_plus_congestion != Some(wei6[5]) {
        status[COMPONENT_PRICES_IN_WEI] = GasComponentStatus::CrossCheckMismatch;
        return fail(Some(header), status);
    }

    let raw = ArbGasInfoRaw {
        per_l2_tx_wei: wei6[0],
        per_l1_calldata_unit_wei: wei6[1],
        per_storage_allocation_wei: wei6[2],
        per_arb_gas_base_wei: wei6[3],
        per_arb_gas_congestion_wei: wei6[4],
        per_arb_gas_total_wei: wei6[5],
        per_l2_tx_arb_gas: ag3[0],
        per_l1_calldata_unit_arb_gas: ag3[1],
        per_storage_allocation_arb_gas: ag3[2],
        minimum_l2_gas_price_wei: min_raw,
        l1_base_fee_estimate_wei: l1_raw,
    };

    // 6. Checked conversions — an overflow DECLINES its component (never clamps).
    let per_l2_tx_wei = u256_to_u128(raw.per_l2_tx_wei);
    let per_l1_calldata_unit_wei = u256_to_u128(raw.per_l1_calldata_unit_wei);
    let per_arb_gas_total_wei = u256_to_u128(raw.per_arb_gas_total_wei);
    let per_storage_allocation_wei = u256_to_u128(raw.per_storage_allocation_wei);
    if per_l2_tx_wei.is_none()
        || per_l1_calldata_unit_wei.is_none()
        || per_arb_gas_total_wei.is_none()
        || per_storage_allocation_wei.is_none()
    {
        status[COMPONENT_PRICES_IN_WEI] = GasComponentStatus::Overflow;
    }
    // Live tokenGasUnits must fit u64 (checked view) AND u32 (legacy bridge field).
    let token_gas_units = u256_to_u64(raw.per_l1_calldata_unit_arb_gas)
        .filter(|v| u32::try_from(*v).is_ok());
    if token_gas_units.is_none() {
        status[COMPONENT_PRICES_IN_ARB_GAS] = GasComponentStatus::Overflow;
    }
    let minimum_l2_gas_price_wei = u256_to_u128(raw.minimum_l2_gas_price_wei);
    if minimum_l2_gas_price_wei.is_none() {
        status[COMPONENT_MINIMUM_GAS_PRICE] = GasComponentStatus::Overflow;
    }
    let l1_base_fee_estimate_wei = u256_to_u128(raw.l1_base_fee_estimate_wei);
    if l1_base_fee_estimate_wei.is_none() {
        status[COMPONENT_L1_BASE_FEE_ESTIMATE] = GasComponentStatus::Overflow;
    }
    let (
        Some(per_l2_tx_wei),
        Some(per_l1_calldata_unit_wei),
        Some(per_arb_gas_total_wei),
        Some(per_storage_allocation_wei),
        Some(token_gas_units),
        Some(minimum_l2_gas_price_wei),
        Some(l1_base_fee_estimate_wei),
    ) = (
        per_l2_tx_wei,
        per_l1_calldata_unit_wei,
        per_arb_gas_total_wei,
        per_storage_allocation_wei,
        token_gas_units,
        minimum_l2_gas_price_wei,
        l1_base_fee_estimate_wei,
    )
    else {
        return fail(Some(header), status);
    };

    let checked = GasPricingView {
        per_l2_tx_wei,
        per_l1_calldata_unit_wei,
        per_arb_gas_total_wei,
        per_l1_calldata_unit_arb_gas: token_gas_units,
        per_storage_allocation_wei,
        minimum_l2_gas_price_wei,
        l1_base_fee_estimate_wei,
    };

    // 6.5 Semantic plausibility (fix round, codex NEEDS-FIX-FL14): an ABI-valid tuple whose
    // values are impossible on the target chain DECLINES — a valid-shaped near-zero quote
    // must never become fresh authority. Every rule fails CLOSED (a legitimate chain change
    // surfaces as declined refreshes + health evidence, never as accepted cheap authority).
    if token_gas_units == 0 {
        // Zero tokenGasUnits would zero the entire L1 data fee (real value ≈ 16–61).
        status[COMPONENT_PRICES_IN_ARB_GAS] = GasComponentStatus::ImplausibleValue;
    }
    if minimum_l2_gas_price_wei < ARBOS_MIN_L2_BASE_FEE_FLOOR_WEI {
        // The chain minimum below the ArbOS Dia protocol floor is impossible.
        status[COMPONENT_MINIMUM_GAS_PRICE] = GasComponentStatus::ImplausibleValue;
    }
    if l1_base_fee_estimate_wei == 0 {
        // A zero L1 base fee would zero the L1 cost leg; decline AT the authority instead
        // of leaning on the downstream legacy `l1 > 0` broadcast filter.
        status[COMPONENT_L1_BASE_FEE_ESTIMATE] = GasComponentStatus::ImplausibleValue;
    }
    if per_arb_gas_total_wei < minimum_l2_gas_price_wei
        || per_l1_calldata_unit_wei < l1_base_fee_estimate_wei
    {
        // Chain-derived relationships: the per-ArbGas TOTAL can never be below the chain's
        // own enforced minimum, and the wei-per-L1-calldata-unit price (basefee × token
        // units, units >= 1) can never be below the L1 base-fee estimate.
        status[COMPONENT_PRICES_IN_WEI] = GasComponentStatus::ImplausibleValue;
    }
    if status != [GasComponentStatus::Valid; 4] {
        return fail(Some(header), status);
    }

    // 7. Complete — publish ONE new generation. Serialized under the publish lock (writer
    // order == generation order; concurrent refreshes cannot interleave) and ordered so an
    // observable authority generation ALWAYS implies its legacy fee bytes are already
    // published: volatility sample → legacy hot-path mirror → bundled publication (typed
    // snapshot + same-generation legacy in ONE ArcSwap store) → health.
    let _publish = oracle
        .publish_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = {
        let g = oracle.next_generation.fetch_add(1, Ordering::AcqRel);
        NonZeroU64::new(g).unwrap_or(NonZeroU64::MIN)
    };
    let snapshot = Arc::new(GasAuthoritySnapshot {
        schema_version: 1,
        generation,
        canonical_block_number: header.number,
        canonical_block_hash: header.hash,
        provider_id: transport.provider_id(),
        fee_aggregator: cfg.frozen_aggregator,
        fee_aggregator_identity_hash: identity,
        raw,
        checked,
        component_status: [GasComponentStatus::Valid; 4],
        complete_component_mask: COMPLETE_COMPONENT_MASK,
        captured_at_mono_ns: now_mono_ns,
        last_complete_at_mono_ns: now_mono_ns,
    });

    // Legacy bridge: EXACTLY one real volatility sample per COMPLETE refresh (a failed
    // fetch records nothing), then the existing GasSnapshot publication — with the LIVE
    // tokenGasUnits and NO floor. The hot-path mirror is stored BEFORE the bundle so no
    // reader can observe generation `g` while the legacy mirror still carries `g-1`.
    oracle.record_l1_sample(l1_base_fee_estimate_wei);
    let legacy_snap = bridge_to_legacy(
        &snapshot,
        cfg,
        token_gas_units as u32,
        oracle.l1_sigma_bps(),
        now_wall_ms,
    );
    oracle.store(legacy_snap);
    oracle
        .authority
        .store(Some(Arc::new(GasAuthorityPublication {
            authority: snapshot.clone(),
            legacy: legacy_snap,
        })));

    let health_generation = oracle.bump_authority_health_generation();
    let prev_health = oracle.authority_health.load_full();
    oracle.authority_health.store(Arc::new(GasHealth {
        health_generation,
        last_complete_generation: Some(generation),
        last_attempt: Some(GasRefreshAttempt {
            requested_block_number: header.number,
            requested_block_hash: header.hash,
            provider_id: snapshot.provider_id,
            fee_aggregator_identity_hash: identity,
            component_status: [GasComponentStatus::Valid; 4],
            attempted_at_mono_ns: now_mono_ns,
        }),
        stream_state: GasStreamState::Live,
        hard_conflict: prev_health.hard_conflict,
    }));

    RefreshOutcome::Complete { generation }
}
