//! WI-02 — the ONE immutable boot-resolved `RuntimePolicy` (codex plan §5 WI-02).
//!
//! Resolved ONCE before discovery; enforced at all six family boundaries:
//! (1) config/discovery, (2) worker construction, (3) universe/state stores,
//! (4) route graph, (5) prediction/simulation, (6) submission/encoding.
//!
//! `ACTIVE_FAMILIES` unset in development = all currently-supported families =
//! byte-identical behavior against the WI-01 golden baseline, EXCEPT the one reviewed
//! correction: Kyber's activation no longer silently defaults on (the 2026-07-02 drift —
//! the strict parity artifact lists Kyber unsupported and production startup drops Kyber
//! pools; the WI-01 Kyber positives carry the proof-backed candidate→typed-decline
//! correction in `codex/campaign_baseline_wi01/wi02_corrections.json`). Maverick keeps its
//! existing evidence gate (operator flag AND reviewed live reference vectors).
//!
//! A live/capital profile must explicitly set EVERY policy field or the boot is refused;
//! the capital policy is Uniswap V3 + Pancake V3 only. No family code is compiled out;
//! REVM stays compiled and its runtime validation mode is declared in the run manifest.
//!
//! Hot-path contract: resolution happens once at boot; the quote/submit boundaries read a
//! boot-built dense `[bool; N_FAMILIES]` array (one indexed load — no env parse, no branch
//! chain, no allocation in the hot loop).

use std::num::NonZeroU64;

use engine_state::PoolKind;

use crate::fastlane_policy::{
    family_from_token, family_index, family_token, FastlanePromotion, ALL_FAMILIES,
    N_FAMILIES,
};

/// Dense per-family membership set (bit per `PoolKind` discriminant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyBitSet {
    bits: u32,
}

impl FamilyBitSet {
    /// Empty set.
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// All 18 families.
    pub fn all() -> Self {
        let mut s = Self::empty();
        for k in ALL_FAMILIES {
            s.insert(k);
        }
        s
    }

    /// Insert a family.
    pub fn insert(&mut self, kind: PoolKind) {
        self.bits |= 1 << family_index(kind);
    }

    /// Remove a family.
    pub fn remove(&mut self, kind: PoolKind) {
        self.bits &= !(1 << family_index(kind));
    }

    /// Membership test.
    #[inline]
    pub fn contains(&self, kind: PoolKind) -> bool {
        self.bits & (1 << family_index(kind)) != 0
    }

    /// Member count.
    pub fn count(&self) -> usize {
        self.bits.count_ones() as usize
    }

    /// `self ⊆ other`.
    pub fn is_subset_of(&self, other: &FamilyBitSet) -> bool {
        self.bits & !other.bits == 0
    }

    /// Intersection.
    pub fn intersect(&self, other: &FamilyBitSet) -> FamilyBitSet {
        FamilyBitSet {
            bits: self.bits & other.bits,
        }
    }

    /// Dense `[bool; N_FAMILIES]` projection (the boot-built hot-path array).
    pub fn dense(&self) -> [bool; N_FAMILIES] {
        let mut out = [false; N_FAMILIES];
        for k in ALL_FAMILIES {
            out[family_index(k)] = self.contains(k);
        }
        out
    }

    /// Canonical token list (enum order — stable for hashing/manifest).
    pub fn tokens(&self) -> Vec<&'static str> {
        ALL_FAMILIES
            .into_iter()
            .filter(|k| self.contains(*k))
            .map(family_token)
            .collect()
    }
}

/// Dense route-shape membership set over `StrategyKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteShapeBitSet {
    bits: u16,
}

/// Canonical route-shape tokens, in `StrategyKind` catalog order.
pub const ROUTE_SHAPE_TOKENS: [&str; 8] = [
    "two_hop", "triangle", "four_hop", "five_hop", "six_hop", "seven_hop", "eight_hop",
    "self_impact",
];

impl RouteShapeBitSet {
    /// Empty set.
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// All supported shapes.
    pub const fn all() -> Self {
        Self {
            bits: (1 << ROUTE_SHAPE_TOKENS.len()) - 1,
        }
    }

    /// Insert by catalog index.
    pub fn insert_index(&mut self, idx: usize) {
        self.bits |= 1 << idx;
    }

    /// Membership by catalog index.
    pub fn contains_index(&self, idx: usize) -> bool {
        self.bits & (1 << idx) != 0
    }

    /// Member count.
    pub fn count(&self) -> usize {
        self.bits.count_ones() as usize
    }

    /// Canonical token list.
    pub fn tokens(&self) -> Vec<&'static str> {
        ROUTE_SHAPE_TOKENS
            .iter()
            .enumerate()
            .filter(|(i, _)| self.contains_index(*i))
            .map(|(_, t)| *t)
            .collect()
    }
}

/// REVM validation mode. REVM is ALWAYS compiled (single binary); this is the runtime
/// validation posture, declared in the run manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevmValidationMode {
    /// The current default: the fast-lane sampler diverts a fraction of candidates to the
    /// REVM shadow ring for agreement accounting.
    SampledShadow,
    /// Every candidate REVM-validated (full shadow).
    Full,
    /// REVM validation off (compiled, dormant).
    Off,
}

impl RevmValidationMode {
    /// Canonical wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            RevmValidationMode::SampledShadow => "sampled_shadow",
            RevmValidationMode::Full => "full",
            RevmValidationMode::Off => "off",
        }
    }
}

/// Shadow-state detection posture (observe-only lane; wired by later WIs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowStateDetection {
    /// Observe-only (current default).
    Observe,
    /// Off.
    Off,
}

impl ShadowStateDetection {
    /// Canonical wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            ShadowStateDetection::Observe => "observe",
            ShadowStateDetection::Off => "off",
        }
    }
}

/// Live-submit authority scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSubmitAuthorityMode {
    /// Submission structurally disabled (DRY_RUN=true / SUBMIT_TX=false / live_submit=false).
    StructurallyDisabled,
    /// Live submission explicitly authorized (capital profile).
    LiveExplicit,
}

impl LiveSubmitAuthorityMode {
    /// Canonical wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            LiveSubmitAuthorityMode::StructurallyDisabled => "structurally_disabled",
            LiveSubmitAuthorityMode::LiveExplicit => "live_explicit",
        }
    }
}

/// Typed boot refusal. One missing/unproven field refuses boot — there is no silent default
/// on a live/capital profile.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyRefusal {
    /// A live profile left a required policy field unset.
    #[error("live profile refuses boot: required policy field `{field}` is unset")]
    LiveFieldUnset {
        /// The unset field.
        field: &'static str,
    },
    /// An `ACTIVE_FAMILIES`/`PROVEN_FAMILIES` token did not parse to a family.
    #[error("unknown ACTIVE_FAMILIES token `{token}` (fail-closed)")]
    UnknownFamilyToken {
        /// The unparseable token.
        token: String,
    },
    /// An env value did not parse to a known mode.
    #[error("unknown value `{value}` for `{field}` (fail-closed)")]
    UnknownModeValue {
        /// The field.
        field: &'static str,
        /// The unparseable value.
        value: String,
    },
    /// A live profile requested a family without live activation evidence.
    #[error("live profile refuses boot: family `{family}` — {reason}")]
    FamilyActivationRefused {
        /// The refused family token.
        family: &'static str,
        /// Typed per-family reason.
        reason: &'static str,
    },
    /// Authoritative fast-lane mode was requested but the strict parity proof was rejected.
    #[error("authoritative fast-lane parity proof rejected: {detail}")]
    ParityProofRejected {
        /// The proof rejection detail.
        detail: String,
    },
}

/// The ONE immutable boot policy record.
#[derive(Debug, Clone)]
pub struct RuntimePolicy {
    /// Families active at all six boundaries.
    pub active_families: FamilyBitSet,
    /// Families promotable onto the authoritative zero-RPC fast lane (⊆ active).
    pub proven_families: FamilyBitSet,
    /// Route shapes the enumerator may build.
    pub active_route_shapes: RouteShapeBitSet,
    /// REVM runtime validation posture (REVM itself is always compiled).
    pub revm_validation_mode: RevmValidationMode,
    /// Shadow-state detection posture.
    pub shadow_state_detection: ShadowStateDetection,
    /// Live-submission authority scope.
    pub live_submit_authority: LiveSubmitAuthorityMode,
    /// Boot generation (nonzero; consumed by later state-authority WIs).
    pub generation: NonZeroU64,
    /// SHA-256 over the canonical policy record (generation excluded, so identical
    /// configuration ⇒ identical hash across boots).
    pub record_hash: [u8; 32],
}

/// The ONE strict truthy vocabulary for policy-bearing switches: only `1/true/on/yes`
/// enables; anything else (including garbage like `2`) is FALSE. WI-02c 4A: every parser of
/// `FASTLANE_AUTHORITATIVE`/`FASTLANE_EXPRESS` must use exactly this predicate — a
/// permissive outer parse combined with this strict inner parse let a garbage value boot an
/// authoritative lane while the strict parity proof was silently skipped.
pub fn strict_flag_truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// Internal alias (historical name).
fn is_truthy(v: &str) -> bool {
    strict_flag_truthy(v)
}

/// Falsey env vocabulary (for default-on switches like `DRY_RUN`).
fn is_falsey(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// The development default active set with NO explicit `ACTIVE_FAMILIES`: all
/// currently-supported families — all 18 minus the evidence-gated opt-ins (Kyber: explicit
/// `KYBER_ENABLE`; Maverick: `MAVERICK_ENABLE` AND reviewed live reference vectors). This is
/// byte-identical to current structural behavior except the reviewed Kyber correction.
pub fn default_active_families() -> FamilyBitSet {
    let mut s = FamilyBitSet::all();
    s.remove(PoolKind::KyberSwapElastic);
    s.remove(PoolKind::MaverickV2);
    s
}

/// Per-family LIVE activation evidence. The capital policy is U3 + Pancake V3 only; every
/// other family carries its typed refusal reason (fail-closed, named — never a silent drop).
fn live_activation_refusal(kind: PoolKind) -> Option<&'static str> {
    match kind {
        PoolKind::UniswapV3 | PoolKind::PancakeV3 => None,
        PoolKind::UniswapV2 | PoolKind::SushiV2 => {
            Some("zero current strict-bar parity proof for the live capital profile")
        }
        PoolKind::SushiV3 => {
            Some("zero current strict-bar parity proof for the live capital profile")
        }
        PoolKind::CamelotV2 => {
            Some("admin-mutable fork fees; reviewed fork fee vectors do not exist")
        }
        PoolKind::AlgebraV19 => {
            Some("zero-disagreement promotion gate unmet (current report is 34 of 36)")
        }
        PoolKind::UniswapV4 => {
            Some("PoolManager singleton transaction-order authority is unproven")
        }
        PoolKind::CurveStableSwapNg => {
            Some("shared-state Vault/registry family; individual state + executor proof missing")
        }
        PoolKind::CurveCryptoSwap => {
            Some("CryptoSwap variants require individual state and executor proof")
        }
        PoolKind::TraderJoeLb => {
            Some("complete bin window + measured gas profile do not exist")
        }
        PoolKind::MaverickV2 => Some("live reference readiness vectors are empty"),
        PoolKind::FluidDex => {
            Some("shared-state Fluid V1/V2 family; live activation proof missing")
        }
        PoolKind::DodoV2 => {
            Some("PMM variant state/route/executor/gas proof missing")
        }
        PoolKind::KyberSwapElastic => Some(
            "the strict parity artifact lists Kyber unsupported (the default-on drift is \
             corrected; activation requires proof, not a flag)",
        ),
        PoolKind::SolidlyV3 => Some("no Mantis executor exchange id (submit-ineligible)"),
        PoolKind::BalancerStable => {
            Some("Balancer Vault family full variant proof missing")
        }
        PoolKind::BalancerWeighted => {
            Some("power-approximation kernel; never byte-exact, never capital-approved")
        }
    }
}

/// Shared-state singleton families (their state can move via OTHER txs in the same block);
/// they never enter the proven/promotable set without `FASTLANE_SINGLETON_LIVE_PROVEN=1`.
/// Mirrors `route_has_singleton_family` in the pipeline.
fn is_singleton_family(kind: PoolKind) -> bool {
    matches!(
        kind,
        PoolKind::UniswapV4
            | PoolKind::CurveStableSwapNg
            | PoolKind::CurveCryptoSwap
            | PoolKind::FluidDex
    )
}

/// Parse a comma/space-separated family token list (fail-closed on unknown tokens).
/// `all` maps to the development default-all semantics (evidence gates still apply).
fn parse_family_list(
    raw: &str,
    var: &dyn Fn(&str) -> Option<String>,
) -> Result<FamilyBitSet, PolicyRefusal> {
    let mut set = FamilyBitSet::empty();
    for tok in raw.split([',', ' ', '\t']).filter(|s| !s.is_empty()) {
        let t = tok.trim().to_ascii_lowercase();
        if t == "all" {
            let defaults = default_active_with_optins(var);
            for k in ALL_FAMILIES {
                if defaults.contains(k) {
                    set.insert(k);
                }
            }
            continue;
        }
        match family_from_token(&t) {
            Some(k) => set.insert(k),
            None => {
                return Err(PolicyRefusal::UnknownFamilyToken { token: t });
            }
        }
    }
    Ok(set)
}

/// The default active set PLUS any explicitly-enabled evidence-gated opt-ins.
fn default_active_with_optins(var: &dyn Fn(&str) -> Option<String>) -> FamilyBitSet {
    let mut set = default_active_families();
    // Kyber: explicit opt-in only (the corrected drift). The pure decision fn is the same
    // one the quote-layer gate historically used — ONE vocabulary.
    if var("KYBER_ENABLE").as_deref().is_some_and(is_truthy) {
        set.insert(PoolKind::KyberSwapElastic);
    }
    // Maverick: operator flag AND the in-code reviewed live-reference readiness gate. The
    // flag alone can never activate unvalidated quote math.
    if var("MAVERICK_ENABLE").as_deref().is_some_and(is_truthy)
        && engine_amm::maverick_v2::live_reference_vectors_ready()
    {
        set.insert(PoolKind::MaverickV2);
    }
    set
}

/// Resolve the ONE immutable policy from an injectable env reader + strict fast-lane parity
/// proof (the same hermetic seam shape as `FastlaneRuntimePolicy::from_env_with`; the proof
/// is consulted ONLY when `FASTLANE_AUTHORITATIVE` is truthy).
pub fn resolve_runtime_policy_with(
    var: &dyn Fn(&str) -> Option<String>,
    parity_proof: impl FnOnce() -> std::result::Result<
        [bool; N_FAMILIES],
        crate::fastlane_policy::ParityProofError,
    >,
) -> Result<RuntimePolicy, PolicyRefusal> {
    // ── live/capital detection ──
    let dry_run = !var("DRY_RUN").as_deref().is_some_and(is_falsey);
    let submit_tx = var("SUBMIT_TX").as_deref().is_some_and(is_truthy);
    let live_submit = var("LIVE_SUBMIT").as_deref().is_some_and(is_truthy);
    let live_requested = !dry_run || submit_tx || live_submit;

    if live_requested {
        // A live/capital profile must set EVERY policy field explicitly — one missing field
        // refuses boot.
        for field in [
            "ACTIVE_FAMILIES",
            "PROVEN_FAMILIES",
            "ACTIVE_ROUTE_SHAPES",
            "REVM_VALIDATION_MODE",
            "SHADOW_STATE_DETECTION",
            "LIVE_SUBMIT_AUTHORITY",
        ] {
            if var(field).as_deref().map(str::trim).filter(|s| !s.is_empty()).is_none() {
                return Err(PolicyRefusal::LiveFieldUnset { field });
            }
        }
        let authority_raw = var("LIVE_SUBMIT_AUTHORITY").unwrap_or_default();
        if authority_raw.trim() != "live_explicit" {
            return Err(PolicyRefusal::UnknownModeValue {
                field: "LIVE_SUBMIT_AUTHORITY",
                value: authority_raw,
            });
        }
    }

    // ── active families ──
    let active_families = match var("ACTIVE_FAMILIES") {
        Some(raw) => {
            let mut set = parse_family_list(&raw, var)?;
            // Dev profile: the request alone can never activate the Maverick kernel while
            // the reviewed live reference vectors are empty (readiness is code-level
            // evidence) — the family silently stays inactive. On a LIVE profile the same
            // request must instead REFUSE boot (handled by the evidence loop below), never
            // be silently dropped.
            if !live_requested
                && set.contains(PoolKind::MaverickV2)
                && !engine_amm::maverick_v2::live_reference_vectors_ready()
            {
                set.remove(PoolKind::MaverickV2);
            }
            set
        }
        None => default_active_with_optins(var),
    };

    // ── live per-family activation evidence (capital = U3 + Pancake V3 only) ──
    if live_requested {
        for k in ALL_FAMILIES {
            if active_families.contains(k) {
                if let Some(reason) = live_activation_refusal(k) {
                    return Err(PolicyRefusal::FamilyActivationRefused {
                        family: family_token(k),
                        reason,
                    });
                }
            }
        }
    }

    // ── proven (fast-lane promotable) families ──
    // Base: the same env-configured promotion set production resolves (byte-exact defaults +
    // FASTLANE_PROMOTED_FAMILIES/ACK), then the strict parity gate when authoritative.
    let authoritative = var("FASTLANE_AUTHORITATIVE").as_deref().is_some_and(is_truthy);
    let mut promotion = FastlanePromotion::from_env_with(var);
    if authoritative {
        let proven = parity_proof().map_err(|e| PolicyRefusal::ParityProofRejected {
            detail: e.to_string(),
        })?;
        promotion.apply_parity_gate(&proven);
    }
    let singletons_attested = var("FASTLANE_SINGLETON_LIVE_PROVEN")
        .as_deref()
        .is_some_and(is_truthy);
    let mut proven_families = FamilyBitSet::empty();
    for k in ALL_FAMILIES {
        // WI-02c 4C: promotable = the PROMOTION CONTRACT's result ∧ executor id ∧
        // (¬shared-state singleton ∨ explicit singleton attestation) ∧ active. The
        // promotion contract (fastlane_policy): byte-exact candidates enter by default;
        // non-byte-exact exotics require the operator's FASTLANE_ACK_AGREEMENT_PROVEN
        // attestation; authoritative mode intersects everything with the strict parity
        // proof. There is NO byte-exact-only conjunct here — stripping ACKed/strictly-
        // proven exotics (DODO/Balancer/…, which carry executor ids) was coverage loss
        // under STRICT_NO_HOT_RPC (codex WI02B_VERIFY 4C), not tightening.
        if promotion.is_promoted(k)
            && k.to_mantis_exchange_id().is_some()
            && (!is_singleton_family(k) || singletons_attested)
            && active_families.contains(k)
        {
            proven_families.insert(k);
        }
    }
    // Live profile: the explicit PROVEN_FAMILIES request must not exceed the derived set.
    if live_requested {
        let requested = parse_family_list(&var("PROVEN_FAMILIES").unwrap_or_default(), var)?;
        proven_families = proven_families.intersect(&requested);
    }
    debug_assert!(proven_families.is_subset_of(&active_families));

    // ── route shapes ──
    let active_route_shapes = match var("ACTIVE_ROUTE_SHAPES") {
        Some(raw) => {
            let mut set = RouteShapeBitSet::empty();
            for tok in raw.split([',', ' ', '\t']).filter(|s| !s.is_empty()) {
                let t = tok.trim().to_ascii_lowercase();
                if t == "all" {
                    set = RouteShapeBitSet::all();
                    continue;
                }
                match ROUTE_SHAPE_TOKENS.iter().position(|s| *s == t) {
                    Some(i) => set.insert_index(i),
                    None => {
                        return Err(PolicyRefusal::UnknownModeValue {
                            field: "ACTIVE_ROUTE_SHAPES",
                            value: t,
                        });
                    }
                }
            }
            set
        }
        None => RouteShapeBitSet::all(),
    };

    // ── modes ──
    let revm_validation_mode = match var("REVM_VALIDATION_MODE").as_deref().map(str::trim) {
        None | Some("") => RevmValidationMode::SampledShadow,
        Some("sampled_shadow") => RevmValidationMode::SampledShadow,
        Some("full") => RevmValidationMode::Full,
        Some("off") => RevmValidationMode::Off,
        Some(other) => {
            return Err(PolicyRefusal::UnknownModeValue {
                field: "REVM_VALIDATION_MODE",
                value: other.to_string(),
            });
        }
    };
    let shadow_state_detection = match var("SHADOW_STATE_DETECTION").as_deref().map(str::trim) {
        None | Some("") => ShadowStateDetection::Observe,
        Some("observe") => ShadowStateDetection::Observe,
        Some("off") => ShadowStateDetection::Off,
        Some(other) => {
            return Err(PolicyRefusal::UnknownModeValue {
                field: "SHADOW_STATE_DETECTION",
                value: other.to_string(),
            });
        }
    };
    let live_submit_authority = if live_requested {
        LiveSubmitAuthorityMode::LiveExplicit
    } else {
        LiveSubmitAuthorityMode::StructurallyDisabled
    };

    // ── record hash + generation ──
    let record_hash = policy_record_hash(
        &active_families,
        &proven_families,
        &active_route_shapes,
        revm_validation_mode,
        shadow_state_detection,
        live_submit_authority,
    );
    let generation = NonZeroU64::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            .max(1),
    )
    .expect("max(1) is nonzero");

    Ok(RuntimePolicy {
        active_families,
        proven_families,
        active_route_shapes,
        revm_validation_mode,
        shadow_state_detection,
        live_submit_authority,
        generation,
        record_hash,
    })
}

/// Resolve from the process environment (production entry; delegates to the seam).
pub fn resolve_runtime_policy() -> Result<RuntimePolicy, PolicyRefusal> {
    resolve_runtime_policy_with(&|name| std::env::var(name).ok(), || {
        crate::fastlane_policy::parity_proven_from_env_strict()
    })
}

/// Canonical policy record string → SHA-256 (generation excluded).
fn policy_record_hash(
    active: &FamilyBitSet,
    proven: &FamilyBitSet,
    shapes: &RouteShapeBitSet,
    revm: RevmValidationMode,
    shadow: ShadowStateDetection,
    authority: LiveSubmitAuthorityMode,
) -> [u8; 32] {
    let canonical = format!(
        "wi02_runtime_policy.v1|active={}|proven={}|shapes={}|revm={}|shadow={}|authority={}|executor={}|gas={}",
        active.tokens().join(","),
        proven.tokens().join(","),
        shapes.tokens().join(","),
        revm.as_str(),
        shadow.as_str(),
        authority.as_str(),
        executor_exchange_id_hash(),
        gas_profile_hash(),
    );
    sha256_bytes(canonical.as_bytes())
}

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let hex = crate::campaign_oracle::sha256_hex(data);
    let mut out = [0u8; 32];
    faster_hex::hex_decode(hex.as_bytes(), &mut out).expect("sha256_hex yields 64 hex chars");
    out
}

fn sha256_hex_str(data: &str) -> String {
    crate::campaign_oracle::sha256_hex(data.as_bytes())
}

/// Boundary 1 (config/discovery): may `kind` enter venue discovery under `policy`?
#[inline]
pub fn family_admitted_at_discovery(policy: &RuntimePolicy, kind: PoolKind) -> bool {
    policy.active_families.contains(kind)
}

/// Boundary 3 (universe/state stores): may a validated pool of `kind` register a store slot?
#[inline]
pub fn pool_admitted_to_universe(policy: &RuntimePolicy, kind: PoolKind) -> bool {
    policy.active_families.contains(kind)
}

/// Boundary 2 (workers): may the Fluid shared-state pollers start under `policy`?
#[inline]
pub fn fluid_workers_allowed(policy: &RuntimePolicy) -> bool {
    policy.active_families.contains(PoolKind::FluidDex)
}

/// Boundary 4 (route graph): may a pool of `kind` enter route enumeration? An unpriced
/// token excludes the pool BEFORE routes are built (P1-08: no route may be sized or valued
/// through a token with no derived price anchor).
#[inline]
pub fn route_pool_eligible(
    policy: &RuntimePolicy,
    kind: PoolKind,
    token0_priced: bool,
    token1_priced: bool,
) -> bool {
    policy.active_families.contains(kind) && token0_priced && token1_priced
}

/// Price-anchor surface (P1-08): may a pool of `kind` contribute a token price anchor?
/// Anchors are rebuilt from ACTIVE families only.
#[inline]
pub fn anchor_pool_admitted(policy: &RuntimePolicy, kind: PoolKind) -> bool {
    policy.active_families.contains(kind)
}

/// Canonical family→Mantis-executor-exchange-id table string.
fn executor_table_canonical() -> String {
    let rows: Vec<String> = ALL_FAMILIES
        .into_iter()
        .map(|k| {
            let id = k
                .to_mantis_exchange_id()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "none".to_string());
            format!("{}={}", family_token(k), id)
        })
        .collect();
    format!("wi02_executor_exchange_ids.v1|{}", rows.join("|"))
}

/// Canonical family→swap-gas-estimate profile table string.
fn gas_table_canonical() -> String {
    let rows: Vec<String> = ALL_FAMILIES
        .into_iter()
        .map(|k| format!("{}={}", family_token(k), k.swap_gas_estimate()))
        .collect();
    format!("wi02_gas_profile.v1|{}", rows.join("|"))
}

/// SHA-256 (hex) over the canonical family→Mantis-executor-exchange-id table.
pub fn executor_exchange_id_hash() -> String {
    sha256_hex_str(&executor_table_canonical())
}

/// SHA-256 (hex) over the canonical family→swap-gas-estimate profile table.
pub fn gas_profile_hash() -> String {
    sha256_hex_str(&gas_table_canonical())
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The run-manifest policy binding: active/proven/route-shape/executor/gas-profile hashes +
/// the REVM compiled/mode declaration, embedded into `network_run_manifest.json` so every
/// run's evidence is bound to the exact resolved policy.
pub fn manifest_binding(policy: &RuntimePolicy) -> serde_json::Value {
    let active_tokens = policy.active_families.tokens();
    let proven_tokens = policy.proven_families.tokens();
    let shape_tokens = policy.active_route_shapes.tokens();
    serde_json::json!({
        "schema": "wi02_runtime_policy_binding.v1",
        "active_families": active_tokens,
        "active_families_hash": sha256_hex_str(&active_tokens.join(",")),
        "proven_families": proven_tokens,
        "proven_families_hash": sha256_hex_str(&proven_tokens.join(",")),
        "route_shapes": shape_tokens,
        "route_shapes_hash": sha256_hex_str(&shape_tokens.join(",")),
        "executor_exchange_id_hash": executor_exchange_id_hash(),
        "gas_profile_hash": gas_profile_hash(),
        "record_hash": hex32(&policy.record_hash),
        "generation": policy.generation.get(),
        "revm_compiled": true,
        "revm_validation_mode": policy.revm_validation_mode.as_str(),
        "shadow_state_detection": policy.shadow_state_detection.as_str(),
        "live_submit_authority": policy.live_submit_authority.as_str(),
    })
}
