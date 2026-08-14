//! Per-family fast-lane promotion policy (TAIL_LATENCY_PLAN §3 B1, ALCHEMY_OFF_HOT_PATH §5).
//!
//! The authoritative zero-RPC fast lane started as a hardcoded byte-exact set (V2/V3/V4/Algebra).
//! To remove Alchemy from the hot path for the WHOLE route surface without losing coverage, each
//! exotic family is promoted onto the same kernel lane **one at a time, only after a strict,
//! state-pinned report proves zero-wei disagreement with an authoritative reference**. Runtime
//! shadow counters remain useful diagnostics, but boolean profit agreement is not byte parity.
//! This module is the pure, tested policy that validates the report and decides eligibility.
//!
//! Capital is safe regardless of the lane (the on-chain executor reverts atomically + `minNetProfit`
//! backstop), so a promotion only ever trades gas/EV, never principal — but the discipline is to
//! promote on evidence, never on faith.

use std::collections::{BTreeMap, BTreeSet};

use engine_state::{PoolKind, RiskClass};
use serde::Deserialize;

/// Number of pool families (matches the `PoolKind` discriminant range 0..=17).
pub const N_FAMILIES: usize = 18;

/// Stable per-family index for the promoted set and per-family agreement counters.
#[inline]
pub fn family_index(kind: PoolKind) -> usize {
    kind as u8 as usize
}

/// Canonical lowercase token for a family, matching the engine config's `family` strings — used to
/// parse `FASTLANE_PROMOTED_FAMILIES`.
pub fn family_token(kind: PoolKind) -> &'static str {
    match kind {
        PoolKind::UniswapV2 => "uniswap_v2",
        PoolKind::SushiV2 => "sushi_v2",
        PoolKind::CamelotV2 => "camelot_v2",
        PoolKind::UniswapV3 => "uniswap_v3",
        PoolKind::SushiV3 => "sushi_v3",
        PoolKind::PancakeV3 => "pancake_v3",
        PoolKind::AlgebraV19 => "algebra_v19",
        PoolKind::CurveStableSwapNg => "curve_stableswap",
        PoolKind::CurveCryptoSwap => "curve_cryptoswap",
        PoolKind::TraderJoeLb => "trader_joe_lb",
        PoolKind::MaverickV2 => "maverick_v2",
        PoolKind::FluidDex => "fluid",
        PoolKind::DodoV2 => "dodo",
        PoolKind::KyberSwapElastic => "kyber_elastic",
        PoolKind::SolidlyV3 => "solidly_v3",
        PoolKind::UniswapV4 => "uniswap_v4",
        PoolKind::BalancerStable => "balancer_stable",
        PoolKind::BalancerWeighted => "balancer_weighted",
    }
}

/// All families, for env parsing / iteration.
pub const ALL_FAMILIES: [PoolKind; N_FAMILIES] = [
    PoolKind::UniswapV2,
    PoolKind::SushiV2,
    PoolKind::CamelotV2,
    PoolKind::UniswapV3,
    PoolKind::SushiV3,
    PoolKind::PancakeV3,
    PoolKind::AlgebraV19,
    PoolKind::CurveStableSwapNg,
    PoolKind::CurveCryptoSwap,
    PoolKind::TraderJoeLb,
    PoolKind::MaverickV2,
    PoolKind::FluidDex,
    PoolKind::DodoV2,
    PoolKind::KyberSwapElastic,
    PoolKind::SolidlyV3,
    PoolKind::UniswapV4,
    PoolKind::BalancerStable,
    PoolKind::BalancerWeighted,
];

/// Legacy policy-default byte-exact candidates: the V2/V3/V4/Algebra class. Membership is not
/// proof and cannot authorize the lane by itself; [`parity_proven_from_env`] must independently
/// accept current-build, promotion-grade evidence for every effective family.
pub const BYTE_EXACT_FAMILIES: [PoolKind; 8] = [
    PoolKind::UniswapV2,
    PoolKind::SushiV2,
    PoolKind::CamelotV2,
    PoolKind::UniswapV3,
    PoolKind::SushiV3,
    PoolKind::PancakeV3,
    PoolKind::AlgebraV19,
    PoolKind::UniswapV4,
];

/// Resolve a config/env family token to its `PoolKind` (also accepts the group alias handling in
/// [`FastlanePromotion::from_env`]).
pub fn family_from_token(s: &str) -> Option<PoolKind> {
    let s = s.trim().to_ascii_lowercase();
    ALL_FAMILIES.into_iter().find(|k| family_token(*k) == s)
}

/// A token is fast-lane-safe iff it's a standard ERC-20 — not fee-on-transfer / rebasing / honeypot.
/// The §3c guard against a non-standard token entering the universe and silently mis-pricing a hop.
#[inline]
pub fn token_is_standard_erc20(risk: RiskClass) -> bool {
    !matches!(risk, RiskClass::Excluded)
}

/// The §5 promotion gate as a checked predicate: a family is ready to add to the authoritative lane
/// once its shadow agreement with REVM has `agree >= min_samples` AND `disagree == 0`. Codifies the
/// decision so it's not eyeballed from logs — a single disagreement keeps a family off the lane.
#[inline]
pub fn family_promotion_ready(agree: u64, disagree: u64, min_samples: u64) -> bool {
    disagree == 0 && agree >= min_samples.max(1)
}

/// STAGE 4 (ULTRALOW_LATENCY_PLAN §STAGE 4): the per-family REVERT RATE, in parts-per-million of the
/// SETTLED submission population (`landed + reverted`), as a pure integer readout for the promotion
/// dashboard. Returns 0 when nothing has settled yet (empty denominator ⇒ no evidence).
///
/// This is the number the §5 evidence machinery finally makes measurable: the on-chain *cost* of
/// promoting a Tier-2 (kernel-priced, not byte-exact) family to the zero-RPC express lane. When a
/// promoted family's kernel mis-prices, the express submission lands at the sequencer and REVERTS on
/// the executor's `minNetProfit` backstop (gas burned, **capital always safe** — the flash-loan
/// arb is atomic), so its revert-rate is exactly the Tier-2 kernel-error EV. A byte-exact family's
/// baseline is ~0. `u128` intermediate so the ×1e6 scale-up cannot overflow at any `u64` count.
#[inline]
pub fn revert_rate_ppm(landed: u64, reverted: u64) -> u64 {
    let settled = landed as u128 + reverted as u128;
    if settled == 0 {
        return 0;
    }
    ((reverted as u128 * 1_000_000) / settled) as u64
}

/// True iff `kind` is in the legacy byte-exact candidate allowlist. This is a policy
/// classification, not proof; the runtime parity-report intersection remains mandatory.
fn is_byte_exact(kind: PoolKind) -> bool {
    let i = family_index(kind);
    BYTE_EXACT_FAMILIES.iter().any(|&k| family_index(k) == i)
}

/// May `kind` enter the pre-report promoted set via env config? Legacy candidates may enter without
/// the exotic-family acknowledgement, but still require the strict report intersection. A
/// non-candidate family requires `evidence_ack` — the operator's explicit attestation
/// (`FASTLANE_ACK_AGREEMENT_PROVEN=1`) that its runtime shadow agreement is proven (`disagree==0`; see
/// `family_promotion_ready`), since the env var alone cannot read the live counters. [Codex] Fail-safe:
/// a casual `FASTLANE_PROMOTED_FAMILIES=all` no longer blind-promotes unproven families onto the
/// zero-RPC lane (a bad promotion burns gas/EV; capital stays protected by the on-chain minNetProfit
/// revert, but this prevents the wasted submissions).
pub fn env_promotion_allowed(kind: PoolKind, evidence_ack: bool) -> bool {
    is_byte_exact(kind) || evidence_ack
}

/// The set of families currently promoted to the authoritative zero-RPC fast lane, plus the hop
/// bound. A route is family-eligible only if every hop's family is promoted and the hop count is in
/// `1..=max_hops`.
#[derive(Debug, Clone)]
pub struct FastlanePromotion {
    promoted: [bool; N_FAMILIES],
    max_hops: usize,
}

impl FastlanePromotion {
    /// Nothing promoted; `max_hops` hop bound.
    pub fn empty(max_hops: usize) -> Self {
        Self {
            promoted: [false; N_FAMILIES],
            max_hops: max_hops.max(1),
        }
    }

    /// The legacy byte-exact candidate default (before the mandatory report intersection).
    pub fn byte_exact_default() -> Self {
        let mut p = Self::empty(3);
        for k in BYTE_EXACT_FAMILIES {
            p.promote(k);
        }
        p
    }

    /// All families promoted (the §5 end-state, used only when every family is agreement-proven).
    pub fn all(max_hops: usize) -> Self {
        let mut p = Self::empty(max_hops);
        for k in ALL_FAMILIES {
            p.promote(k);
        }
        p
    }

    /// Build from `FASTLANE_PROMOTED_FAMILIES` on top of the byte-exact base. Tokens are
    /// comma/whitespace-separated family names ([`family_token`]); the special token `all` promotes
    /// everything, `none` starts from only the byte-exact base (no additions). Unknown tokens are
    /// ignored (logged by the caller). `FASTLANE_MAX_HOPS` overrides the hop bound.
    pub fn from_env() -> Self {
        Self::from_env_with(&|name| std::env::var(name).ok())
    }

    /// [`FastlanePromotion::from_env`] over an injectable env reader — the hermetic seam the
    /// WI-02 `RuntimePolicy` resolution shares so tests never mutate process env.
    pub fn from_env_with(var: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut p = Self::byte_exact_default();
        if let Some(mh) = var("FASTLANE_MAX_HOPS") {
            if let Ok(n) = mh.trim().parse::<usize>() {
                p.max_hops = n.max(1);
            }
        }
        // [Codex] Non-byte-exact promotions require an explicit operator attestation that the family's
        // runtime shadow agreement is proven (disagree==0) — the env var alone can't read the live
        // counters. Without FASTLANE_ACK_AGREEMENT_PROVEN=1, only the legacy candidate set is honored
        // before the report intersection; exotic requests (incl. `all`) are ignored (fail-safe;
        // capital is independently
        // protected by the on-chain minNetProfit revert, this just prevents wasted gas/EV).
        let evidence_ack = var("FASTLANE_ACK_AGREEMENT_PROVEN")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            })
            .unwrap_or(false);
        if let Some(list) = var("FASTLANE_PROMOTED_FAMILIES") {
            for tok in list.split([',', ' ', '\t']).filter(|s| !s.is_empty()) {
                match tok.trim().to_ascii_lowercase().as_str() {
                    "all" => {
                        for k in ALL_FAMILIES {
                            if env_promotion_allowed(k, evidence_ack) {
                                p.promote(k);
                            }
                        }
                    }
                    "none" | "byte_exact" => {}
                    other => {
                        if let Some(k) = family_from_token(other) {
                            if env_promotion_allowed(k, evidence_ack) {
                                p.promote(k);
                            }
                        }
                    }
                }
            }
        }
        p
    }

    /// Promote a family onto the authoritative lane.
    pub fn promote(&mut self, kind: PoolKind) {
        self.promoted[family_index(kind)] = true;
    }

    /// True if `kind` is promoted.
    #[inline]
    pub fn is_promoted(&self, kind: PoolKind) -> bool {
        self.promoted[family_index(kind)]
    }

    /// The configured hop bound.
    #[inline]
    pub fn max_hops(&self) -> usize {
        self.max_hops
    }

    /// Count of promoted families (telemetry/tests).
    pub fn promoted_count(&self) -> usize {
        self.promoted.iter().filter(|&&b| b).count()
    }

    /// True if every hop family is promoted and the hop count is within `1..=max_hops`.
    pub fn route_families_eligible(&self, hop_kinds: &[PoolKind]) -> bool {
        let hc = hop_kinds.len();
        hc >= 1 && hc <= self.max_hops && hop_kinds.iter().all(|k| self.is_promoted(*k))
    }

    /// W1.2 (FIX_PLAN_2026-07-10): intersect the promoted set with the OFFLINE state-matched
    /// parity proof. The authoritative flip is gated on `parity-mismatch = 0` per family from
    /// the `state_matched_parity` harness (operator copies the passing families into
    /// `FASTLANE_PARITY_PROVEN_FAMILIES`), NOT on fl_disagree — which conflates kernel error
    /// with state staleness and, at REVM≤15s, measures marginal-opportunity mortality, not
    /// correctness. Returns the demoted family tokens (for the caller's log line).
    pub fn apply_parity_gate(&mut self, proven: &[bool; N_FAMILIES]) -> Vec<&'static str> {
        let mut demoted = Vec::new();
        for (i, p) in self.promoted.iter_mut().enumerate() {
            if *p && !proven[i] {
                *p = false;
                demoted.push(family_token(ALL_FAMILIES[i]));
            }
        }
        demoted
    }
}

/// Minimum distinct real pools required before a family-wide parity result can authorize the lane.
pub const PARITY_MIN_POOLS: u64 = 3;
/// Minimum distinct `(pool, direction, input)` samples (six sizes in both directions on 3 pools).
pub const PARITY_MIN_INPUTS: u64 = 36;
/// Minimum output comparisons required (the minimum input ladder in both swap directions).
pub const PARITY_MIN_COMPARISONS: u64 = 36;

fn required_parity_edges(kind: PoolKind) -> &'static [&'static str] {
    const BASE: &[&str] = &["dust", "nominal", "extreme", "near_empty_reserve"];
    const CL: &[&str] = &[
        "dust",
        "nominal",
        "extreme",
        "near_tick_boundary",
        "multi_tick_crossing",
    ];
    const LB: &[&str] = &[
        "dust",
        "nominal",
        "extreme",
        "near_bin_boundary",
        "multi_bin_crossing",
    ];
    match kind {
        PoolKind::UniswapV3
        | PoolKind::SushiV3
        | PoolKind::PancakeV3
        | PoolKind::AlgebraV19
        | PoolKind::KyberSwapElastic
        | PoolKind::MaverickV2
        | PoolKind::UniswapV4 => CL,
        PoolKind::TraderJoeLb => LB,
        _ => BASE,
    }
}

fn required_parity_variants(kind: PoolKind) -> &'static [&'static str] {
    match kind {
        PoolKind::CurveStableSwapNg => &["plain_ng"],
        PoolKind::CurveCryptoSwap => &["twocrypto", "tricrypto"],
        PoolKind::TraderJoeLb => &["lb_v21", "lb_v22"],
        PoolKind::FluidDex => &["v1", "v2"],
        PoolKind::DodoV2 => &["dvm", "dpp", "dsp"],
        PoolKind::UniswapV4 => &["hookless_erc20"],
        PoolKind::BalancerStable => {
            &["stable", "composable_stable", "meta_stable", "rate_provider"]
        }
        PoolKind::BalancerWeighted => {
            &["equal_weight", "unequal_weight", "canonical_codehash"]
        }
        _ => &[],
    }
}

/// A structurally valid report may still prove zero families; this error type keeps malformed or
/// stale evidence distinct from a valid negative result so authoritative startup can fail loudly.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParityProofError {
    /// `FASTLANE_PARITY_REPORT` is unset in authoritative mode.
    #[error("FASTLANE_PARITY_REPORT is required for authoritative fast-lane mode")]
    MissingReport,
    /// `FASTLANE_PARITY_PROVEN_FAMILIES` named zero families.
    #[error("FASTLANE_PARITY_PROVEN_FAMILIES must name at least one requested family")]
    MissingRequestedFamilies,
    /// A requested family token did not parse.
    #[error("unknown fast-lane family token: {0}")]
    UnknownFamily(String),
    /// The report file could not be read.
    #[error("could not read parity report {path}: {error}")]
    ReadReport {
        /// Report path.
        path: String,
        /// I/O error detail.
        error: String,
    },
    /// The report failed JSON/schema validation.
    #[error("invalid parity report JSON/schema: {0}")]
    InvalidReport(String),
    /// The report was produced from different source than this build.
    #[error("parity report source identity does not match this build: {0}")]
    SourceMismatch(String),
    /// Authoritative mode requires a clean build for the source binding.
    #[error("authoritative fast lane without an exact source-manifest binding requires a clean build; build status is {0}")]
    UncleanBuild(String),
    /// A referenced evidence file could not be read.
    #[error("{kind} evidence file {path} could not be read: {error}")]
    EvidenceFile {
        /// Evidence kind (corpus/state-pin/transcript).
        kind: &'static str,
        /// Evidence path.
        path: String,
        /// I/O error detail.
        error: String,
    },
    /// A referenced evidence file hash did not match the report.
    #[error("{kind} evidence hash mismatch for {path}: expected {expected}, got {actual}")]
    EvidenceHashMismatch {
        /// Evidence kind (corpus/state-pin/transcript).
        kind: &'static str,
        /// Evidence path.
        path: String,
        /// Hash pinned in the report.
        expected: String,
        /// Hash recomputed from the file.
        actual: String,
    },
    /// The report is valid but does not prove the requested families.
    #[error("requested families are not promotion-grade in the strict report: {0:?}")]
    RequestedFamiliesUnproven(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct StrictParityReport {
    schema_version: u64,
    strict_byte_exact: bool,
    source_head: String,
    source_tree_status: String,
    source_manifest_sha256: Option<String>,
    corpus: String,
    corpus_keccak256: String,
    state_pin_source: String,
    state_pin_source_keccak256: String,
    comparison_transcript: Option<String>,
    comparison_transcript_sha256: Option<String>,
    comparison_transcript_keccak256: Option<String>,
    comparison_transcript_rows: Option<u64>,
    chain: Option<StrictParityChain>,
    families: BTreeMap<String, StrictParityRow>,
}

#[derive(Debug, Deserialize)]
struct StrictParityChain {
    chain_id: u64,
    pinned_blocks: Vec<StrictParityPinnedBlock>,
    quoters: Vec<StrictParityQuoter>,
}

#[derive(Debug, Deserialize)]
struct StrictParityPinnedBlock {
    number: u64,
    hash: String,
}

#[derive(Debug, Deserialize)]
struct StrictParityQuoter {
    family: String,
    pin_block: u64,
    address: String,
}

#[derive(Debug, Deserialize)]
struct StrictParityRow {
    authoritative_reference: String,
    reference_kind: String,
    state_pinned: bool,
    pools_checked: u64,
    inputs_checked: u64,
    pools_with_bidirectional_input_breadth: u64,
    min_inputs_per_pool_direction: u64,
    comparisons: u64,
    directions_checked: u64,
    max_abs_disagreement_wei: serde_json::Value,
    disagree: serde_json::Value,
    unresolved_errors: serde_json::Value,
    byte_exact: bool,
    verdict: String,
    edge_cases: Vec<String>,
    variants: Vec<String>,
}

struct ValidatedParityProof {
    proven: [bool; N_FAMILIES],
    manifest_bound: bool,
    corpus: String,
    corpus_keccak256: String,
    state_pin_source: String,
    state_pin_source_keccak256: String,
    comparison_transcript: Option<String>,
    comparison_transcript_sha256: Option<String>,
    comparison_transcript_keccak256: Option<String>,
    comparison_transcript_rows: Option<u64>,
    chain: Option<StrictParityChain>,
    families: BTreeMap<String, StrictParityRow>,
}

fn json_zero(value: &serde_json::Value) -> bool {
    value.as_u64() == Some(0) || value.as_str() == Some("0")
}

fn is_hex_digest(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2 && value.as_bytes().iter().all(u8::is_ascii_hexdigit)
}

fn row_is_promotion_grade(kind: PoolKind, row: &StrictParityRow) -> bool {
    let authoritative_reference = !row.authoritative_reference.trim().is_empty();
    let authoritative_kind = matches!(
        row.reference_kind.as_str(),
        "official_onchain_quoter"
            | "pool_quote_view"
            | "revm_swap_execution"
            | "vendor_implementation"
    );
    let has_edges = required_parity_edges(kind).iter().all(|required| {
        row.edge_cases
            .iter()
            .any(|edge| edge.as_str() == *required)
    });
    let has_variants = required_parity_variants(kind).iter().all(|required| {
        row.variants
            .iter()
            .any(|variant| variant.as_str() == *required)
    });

    authoritative_reference
        && authoritative_kind
        && row.state_pinned
        && row.pools_checked >= PARITY_MIN_POOLS
        && row.inputs_checked >= PARITY_MIN_INPUTS
        && row.pools_with_bidirectional_input_breadth >= PARITY_MIN_POOLS
        && row.min_inputs_per_pool_direction >= 6
        && row.comparisons >= PARITY_MIN_COMPARISONS
        && row.directions_checked >= 2
        && json_zero(&row.max_abs_disagreement_wei)
        && json_zero(&row.disagree)
        && json_zero(&row.unresolved_errors)
        && row.byte_exact
        && row.verdict == "PROMOTE-ELIGIBLE"
        && has_edges
        && has_variants
}

fn validate_parity_report_json(raw: &str) -> Result<ValidatedParityProof, ParityProofError> {
    let report: StrictParityReport = serde_json::from_str(raw)
        .map_err(|error| ParityProofError::InvalidReport(error.to_string()))?;
    if report.schema_version != 2 {
        return Err(ParityProofError::InvalidReport(format!(
            "schema_version={} (required 2)",
            report.schema_version
        )));
    }
    if !report.strict_byte_exact {
        return Err(ParityProofError::InvalidReport(
            "strict_byte_exact must be true".into(),
        ));
    }

    let build_head = env!("ENGINE_GIT_COMMIT");
    if !is_hex_digest(&report.source_head, 20)
        || !report.source_head.eq_ignore_ascii_case(build_head)
    {
        return Err(ParityProofError::SourceMismatch(format!(
            "report head {} != build head {build_head}",
            report.source_head
        )));
    }
    let build_tree = env!("ENGINE_GIT_DIRTY");
    if report.source_tree_status != build_tree {
        return Err(ParityProofError::SourceMismatch(format!(
            "report tree status {} != build tree status {build_tree}",
            report.source_tree_status
        )));
    }
    let build_manifest = option_env!("ENGINE_SOURCE_MANIFEST_SHA256")
        .filter(|digest| is_hex_digest(digest, 32));
    let manifest_bound = if let Some(build_manifest) = build_manifest {
        let report_manifest = report
            .source_manifest_sha256
            .as_deref()
            .filter(|digest| is_hex_digest(digest, 32))
            .ok_or_else(|| {
                ParityProofError::SourceMismatch(
                    "build embeds a source manifest but report does not".into(),
                )
            })?;
        if !report_manifest.eq_ignore_ascii_case(build_manifest) {
            return Err(ParityProofError::SourceMismatch(format!(
                "report manifest {report_manifest} != build manifest {build_manifest}"
            )));
        }
        true
    } else {
        false
    };
    if report.corpus.trim().is_empty() || !is_hex_digest(&report.corpus_keccak256, 32) {
        return Err(ParityProofError::InvalidReport(
            "corpus path/hash are missing or malformed".into(),
        ));
    }
    if report.state_pin_source.trim().is_empty()
        || !is_hex_digest(&report.state_pin_source_keccak256, 32)
    {
        return Err(ParityProofError::InvalidReport(
            "state-pin source path/hash are missing or malformed".into(),
        ));
    }

    let mut proven = [false; N_FAMILIES];
    for kind in ALL_FAMILIES {
        if let Some(row) = report.families.get(family_token(kind)) {
            proven[family_index(kind)] = row_is_promotion_grade(kind, row);
        }
    }
    Ok(ValidatedParityProof {
        proven,
        manifest_bound,
        corpus: report.corpus,
        corpus_keccak256: report.corpus_keccak256,
        state_pin_source: report.state_pin_source,
        state_pin_source_keccak256: report.state_pin_source_keccak256,
        comparison_transcript: report.comparison_transcript,
        comparison_transcript_sha256: report.comparison_transcript_sha256,
        comparison_transcript_keccak256: report.comparison_transcript_keccak256,
        comparison_transcript_rows: report.comparison_transcript_rows,
        chain: report.chain,
        families: report.families,
    })
}

fn stream_file_keccak256(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read as _;
    use tiny_keccak::Hasher as _;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = tiny_keccak::Keccak::v256();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut digest = [0u8; 32];
    hasher.finalize(&mut digest);
    Ok(digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn resolve_evidence_path(report_path: &std::path::Path, declared: &str) -> std::path::PathBuf {
    let declared = std::path::Path::new(declared);
    if declared.is_absolute() {
        return declared.to_path_buf();
    }
    report_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(declared)
}

fn verify_evidence_file(
    kind: &'static str,
    report_path: &std::path::Path,
    declared: &str,
    expected: &str,
) -> Result<(), ParityProofError> {
    let path = resolve_evidence_path(report_path, declared);
    let actual = stream_file_keccak256(&path).map_err(|error| ParityProofError::EvidenceFile {
        kind,
        path: path.display().to_string(),
        error: error.to_string(),
    })?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(ParityProofError::EvidenceHashMismatch {
            kind,
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct StrictParityTranscriptRow {
    schema: String,
    candidate_id: String,
    family: String,
    chain_id: u64,
    pin_block: u64,
    pin_block_hash: String,
    pool_identity: String,
    token_in: String,
    token_out: String,
    direction: String,
    amount_in: String,
    authoritative_address: String,
    outcome: String,
    kernel_amount_out: Option<String>,
    authoritative_amount_out: Option<String>,
    absolute_difference_wei: Option<String>,
    exact: Option<bool>,
    #[serde(default)]
    mechanical_edge_cases: Vec<String>,
    #[serde(default)]
    mechanical_variants: Vec<String>,
    error_stage: Option<String>,
    error: Option<String>,
}

#[derive(Default)]
struct TranscriptFamilyTally {
    comparisons: u64,
    disagree: u64,
    unresolved_errors: u64,
    max_abs_disagreement_wei: alloy_primitives::U256,
    pools: BTreeSet<String>,
    inputs: BTreeSet<(String, String, alloy_primitives::U256)>,
    directions: BTreeSet<String>,
    edge_cases: BTreeSet<String>,
    variants: BTreeSet<String>,
}

fn transcript_mismatch(reason: impl Into<String>) -> ParityProofError {
    ParityProofError::InvalidReport(format!(
        "comparison transcript mismatch: {}",
        reason.into()
    ))
}

fn is_prefixed_hex(value: &str, bytes: usize) -> bool {
    value.len() == 2 + bytes * 2
        && value.starts_with("0x")
        && value[2..].as_bytes().iter().all(u8::is_ascii_hexdigit)
}

fn transcript_u256(
    value: &str,
    field: &str,
    row_number: usize,
) -> Result<alloy_primitives::U256, ParityProofError> {
    alloy_primitives::U256::from_str_radix(value, 10).map_err(|error| {
        transcript_mismatch(format!(
            "row {row_number} has invalid decimal {field}={value}: {error}"
        ))
    })
}

fn report_u64(
    value: &serde_json::Value,
    family: &str,
    field: &str,
) -> Result<u64, ParityProofError> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
        .ok_or_else(|| {
            transcript_mismatch(format!(
                "family {family} has non-u64 aggregate {field}={value}"
            ))
        })
}

fn report_u256(
    value: &serde_json::Value,
    family: &str,
    field: &str,
) -> Result<alloy_primitives::U256, ParityProofError> {
    let raw = value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .ok_or_else(|| {
            transcript_mismatch(format!(
                "family {family} has non-decimal aggregate {field}={value}"
            ))
        })?;
    alloy_primitives::U256::from_str_radix(&raw, 10).map_err(|error| {
        transcript_mismatch(format!(
            "family {family} has invalid aggregate {field}={raw}: {error}"
        ))
    })
}

fn require_equal<T: std::fmt::Display + PartialEq>(
    family: &str,
    field: &str,
    claimed: T,
    recomputed: T,
) -> Result<(), ParityProofError> {
    if claimed != recomputed {
        return Err(transcript_mismatch(format!(
            "family {family} {field} claimed {claimed}, recomputed {recomputed}"
        )));
    }
    Ok(())
}

fn recompute_transcript_proof(
    report_path: &std::path::Path,
    proof: &ValidatedParityProof,
) -> Result<[bool; N_FAMILIES], ParityProofError> {
    use std::io::BufRead as _;

    let declared = proof
        .comparison_transcript
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| transcript_mismatch("comparison_transcript path is missing"))?;
    let expected_sha256 = proof
        .comparison_transcript_sha256
        .as_deref()
        .filter(|digest| is_hex_digest(digest, 32))
        .ok_or_else(|| transcript_mismatch("comparison_transcript_sha256 is missing or malformed"))?;
    let expected_keccak256 = proof
        .comparison_transcript_keccak256
        .as_deref()
        .filter(|digest| is_hex_digest(digest, 32))
        .ok_or_else(|| {
            transcript_mismatch("comparison_transcript_keccak256 is missing or malformed")
        })?;
    let expected_rows = proof
        .comparison_transcript_rows
        .filter(|rows| *rows > 0)
        .ok_or_else(|| transcript_mismatch("comparison_transcript_rows must be positive"))?;

    let transcript_path = resolve_evidence_path(report_path, declared);
    let transcript_bytes = std::fs::read(&transcript_path).map_err(|error| {
        ParityProofError::EvidenceFile {
            kind: "comparison_transcript",
            path: transcript_path.display().to_string(),
            error: error.to_string(),
        }
    })?;
    use sha2::Digest as _;
    let actual_sha256: String = sha2::Sha256::digest(&transcript_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(ParityProofError::EvidenceHashMismatch {
            kind: "comparison_transcript_sha256",
            path: transcript_path.display().to_string(),
            expected: expected_sha256.to_string(),
            actual: actual_sha256,
        });
    }
    let actual_keccak256: String = alloy_primitives::keccak256(&transcript_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !actual_keccak256.eq_ignore_ascii_case(expected_keccak256) {
        return Err(ParityProofError::EvidenceHashMismatch {
            kind: "comparison_transcript_keccak256",
            path: transcript_path.display().to_string(),
            expected: expected_keccak256.to_string(),
            actual: actual_keccak256,
        });
    }

    let chain = proof
        .chain
        .as_ref()
        .ok_or_else(|| transcript_mismatch("chain provenance is missing"))?;
    if chain.chain_id == 0 {
        return Err(transcript_mismatch("chain.chain_id must be nonzero"));
    }
    let mut pinned_blocks = BTreeMap::new();
    for block in &chain.pinned_blocks {
        if block.number == 0 || !is_prefixed_hex(&block.hash, 32) {
            return Err(transcript_mismatch(format!(
                "chain has malformed pinned block {} hash {}",
                block.number, block.hash
            )));
        }
        if pinned_blocks
            .insert(block.number, block.hash.to_ascii_lowercase())
            .is_some()
        {
            return Err(transcript_mismatch(format!(
                "chain repeats pinned block {}",
                block.number
            )));
        }
    }
    let mut quoters = BTreeSet::new();
    for quoter in &chain.quoters {
        if family_from_token(&quoter.family).is_none()
            || quoter.pin_block == 0
            || !is_prefixed_hex(&quoter.address, 20)
        {
            return Err(transcript_mismatch(format!(
                "chain has malformed quoter binding family={} pin={} address={}",
                quoter.family, quoter.pin_block, quoter.address
            )));
        }
        quoters.insert((
            quoter.family.to_ascii_lowercase(),
            quoter.pin_block,
            quoter.address.to_ascii_lowercase(),
        ));
    }

    let mut tallies: BTreeMap<String, TranscriptFamilyTally> = BTreeMap::new();
    let mut observed_rows = 0u64;
    for (line_index, line) in std::io::BufReader::new(transcript_bytes.as_slice())
        .lines()
        .enumerate()
    {
        let row_number = line_index + 1;
        let line = line.map_err(|error| transcript_mismatch(format!("read row {row_number}: {error}")))?;
        if line.trim().is_empty() {
            continue;
        }
        observed_rows = observed_rows.saturating_add(1);
        let row: StrictParityTranscriptRow = serde_json::from_str(&line).map_err(|error| {
            transcript_mismatch(format!("row {row_number} is invalid JSON/schema: {error}"))
        })?;
        if row.schema != "mantis_parity_comparison_v1" || row.candidate_id.trim().is_empty() {
            return Err(transcript_mismatch(format!(
                "row {row_number} has invalid schema/candidate_id"
            )));
        }
        let kind = family_from_token(&row.family).ok_or_else(|| {
            transcript_mismatch(format!("row {row_number} has unknown family {}", row.family))
        })?;
        let family = family_token(kind).to_string();
        if !proof.families.contains_key(&family) {
            return Err(transcript_mismatch(format!(
                "row {row_number} family {family} is absent from report aggregates"
            )));
        }
        if row.chain_id != chain.chain_id {
            return Err(transcript_mismatch(format!(
                "row {row_number} chain_id {} != report chain_id {}",
                row.chain_id, chain.chain_id
            )));
        }
        let expected_pin_hash = pinned_blocks.get(&row.pin_block).ok_or_else(|| {
            transcript_mismatch(format!(
                "row {row_number} pin {} is absent from chain provenance",
                row.pin_block
            ))
        })?;
        if !row.pin_block_hash.eq_ignore_ascii_case(expected_pin_hash) {
            return Err(transcript_mismatch(format!(
                "row {row_number} pin hash {} != provenance {}",
                row.pin_block_hash, expected_pin_hash
            )));
        }
        if !is_prefixed_hex(&row.pool_identity, 32)
            || !is_prefixed_hex(&row.token_in, 20)
            || !is_prefixed_hex(&row.token_out, 20)
            || !is_prefixed_hex(&row.authoritative_address, 20)
        {
            return Err(transcript_mismatch(format!(
                "row {row_number} has malformed pool/token/authoritative address"
            )));
        }
        let token_in = row.token_in.to_ascii_lowercase();
        let token_out = row.token_out.to_ascii_lowercase();
        if token_in == token_out {
            return Err(transcript_mismatch(format!(
                "row {row_number} has identical input/output token"
            )));
        }
        let expected_direction = if token_in < token_out {
            "zero_for_one"
        } else {
            "one_for_zero"
        };
        if row.direction != expected_direction {
            return Err(transcript_mismatch(format!(
                "row {row_number} direction {} != mechanically derived {expected_direction}",
                row.direction
            )));
        }
        let quoter_key = (
            family.clone(),
            row.pin_block,
            row.authoritative_address.to_ascii_lowercase(),
        );
        if !quoters.contains(&quoter_key) {
            return Err(transcript_mismatch(format!(
                "row {row_number} authoritative address lacks chain quoter binding"
            )));
        }
        let amount_in = transcript_u256(&row.amount_in, "amount_in", row_number)?;
        let tally = tallies.entry(family).or_default();
        match row.outcome.as_str() {
            "resolved" => {
                let kernel = transcript_u256(
                    row.kernel_amount_out.as_deref().ok_or_else(|| {
                        transcript_mismatch(format!(
                            "row {row_number} resolved outcome lacks kernel_amount_out"
                        ))
                    })?,
                    "kernel_amount_out",
                    row_number,
                )?;
                let authoritative = transcript_u256(
                    row.authoritative_amount_out.as_deref().ok_or_else(|| {
                        transcript_mismatch(format!(
                            "row {row_number} resolved outcome lacks authoritative_amount_out"
                        ))
                    })?,
                    "authoritative_amount_out",
                    row_number,
                )?;
                let declared_difference = transcript_u256(
                    row.absolute_difference_wei.as_deref().ok_or_else(|| {
                        transcript_mismatch(format!(
                            "row {row_number} resolved outcome lacks absolute_difference_wei"
                        ))
                    })?,
                    "absolute_difference_wei",
                    row_number,
                )?;
                let difference = kernel.abs_diff(authoritative);
                if declared_difference != difference
                    || row.exact != Some(difference.is_zero())
                {
                    return Err(transcript_mismatch(format!(
                        "row {row_number} output/difference/exact fields are inconsistent"
                    )));
                }
                tally.comparisons = tally.comparisons.saturating_add(1);
                if !difference.is_zero() {
                    tally.disagree = tally.disagree.saturating_add(1);
                }
                tally.max_abs_disagreement_wei =
                    tally.max_abs_disagreement_wei.max(difference);
                let pool = row.pool_identity.to_ascii_lowercase();
                tally.pools.insert(pool.clone());
                tally
                    .inputs
                    .insert((pool, row.direction.clone(), amount_in));
                tally.directions.insert(row.direction);
                for edge in row.mechanical_edge_cases {
                    if edge.trim().is_empty() {
                        return Err(transcript_mismatch(format!(
                            "row {row_number} has an empty mechanical edge label"
                        )));
                    }
                    tally.edge_cases.insert(edge);
                }
                for variant in row.mechanical_variants {
                    if variant.trim().is_empty() {
                        return Err(transcript_mismatch(format!(
                            "row {row_number} has an empty mechanical variant label"
                        )));
                    }
                    tally.variants.insert(variant);
                }
            }
            "unresolved_error" => {
                if row.error_stage.as_deref().is_none_or(str::is_empty)
                    || row.error.as_deref().is_none_or(str::is_empty)
                {
                    return Err(transcript_mismatch(format!(
                        "row {row_number} unresolved outcome lacks error_stage/error"
                    )));
                }
                tally.unresolved_errors = tally.unresolved_errors.saturating_add(1);
            }
            other => {
                return Err(transcript_mismatch(format!(
                    "row {row_number} has unknown outcome {other}"
                )));
            }
        }
    }
    require_equal(
        "all",
        "comparison_transcript_rows",
        expected_rows,
        observed_rows,
    )?;

    let mut transcript_proven = [false; N_FAMILIES];
    for (family, row) in &proof.families {
        let kind = family_from_token(family).ok_or_else(|| {
            transcript_mismatch(format!("report contains unknown family aggregate {family}"))
        })?;
        let tally = tallies.remove(family).unwrap_or_default();
        let mut counts: BTreeMap<(String, String), u64> = BTreeMap::new();
        for (pool, direction, _) in &tally.inputs {
            *counts.entry((pool.clone(), direction.clone())).or_default() += 1;
        }
        let pools_with_bidirectional_input_breadth = tally
            .pools
            .iter()
            .filter(|pool| {
                ["zero_for_one", "one_for_zero"].iter().all(|direction| {
                    counts
                        .get(&(pool.to_string(), direction.to_string()))
                        .copied()
                        .unwrap_or(0)
                        >= 6
                })
            })
            .count() as u64;
        let min_inputs_per_pool_direction = tally
            .pools
            .iter()
            .flat_map(|pool| {
                ["zero_for_one", "one_for_zero"].map(|direction| {
                    counts
                        .get(&(pool.to_string(), direction.to_string()))
                        .copied()
                        .unwrap_or(0)
                })
            })
            .min()
            .unwrap_or(0);
        let claimed_disagree = report_u64(&row.disagree, family, "disagree")?;
        let claimed_unresolved =
            report_u64(&row.unresolved_errors, family, "unresolved_errors")?;
        let claimed_max = report_u256(
            &row.max_abs_disagreement_wei,
            family,
            "max_abs_disagreement_wei",
        )?;
        require_equal(family, "state_pinned", row.state_pinned, tally.comparisons > 0)?;
        require_equal(
            family,
            "pools_checked",
            row.pools_checked,
            tally.pools.len() as u64,
        )?;
        require_equal(
            family,
            "inputs_checked",
            row.inputs_checked,
            tally.inputs.len() as u64,
        )?;
        require_equal(
            family,
            "pools_with_bidirectional_input_breadth",
            row.pools_with_bidirectional_input_breadth,
            pools_with_bidirectional_input_breadth,
        )?;
        require_equal(
            family,
            "comparisons",
            row.comparisons,
            tally.comparisons,
        )?;
        require_equal(
            family,
            "directions_checked",
            row.directions_checked,
            tally.directions.len() as u64,
        )?;
        require_equal(family, "disagree", claimed_disagree, tally.disagree)?;
        require_equal(
            family,
            "unresolved_errors",
            claimed_unresolved,
            tally.unresolved_errors,
        )?;
        require_equal(
            family,
            "max_abs_disagreement_wei",
            claimed_max,
            tally.max_abs_disagreement_wei,
        )?;
        let claimed_edges = row.edge_cases.iter().cloned().collect::<BTreeSet<_>>();
        if claimed_edges != tally.edge_cases {
            return Err(transcript_mismatch(format!(
                "family {family} edge_cases aggregate differs from transcript"
            )));
        }
        let claimed_variants = row.variants.iter().cloned().collect::<BTreeSet<_>>();
        if claimed_variants != tally.variants {
            return Err(transcript_mismatch(format!(
                "family {family} variants aggregate differs from transcript"
            )));
        }
        let has_required_edges = required_parity_edges(kind)
            .iter()
            .all(|required| tally.edge_cases.contains(*required));
        let has_required_variants = required_parity_variants(kind)
            .iter()
            .all(|required| tally.variants.contains(*required));
        let promotion_grade = tally.comparisons >= PARITY_MIN_COMPARISONS
            && tally.pools.len() as u64 >= PARITY_MIN_POOLS
            && tally.inputs.len() as u64 >= PARITY_MIN_INPUTS
            && pools_with_bidirectional_input_breadth >= PARITY_MIN_POOLS
            && min_inputs_per_pool_direction >= 6
            && tally.directions.len() >= 2
            && tally.disagree == 0
            && tally.unresolved_errors == 0
            && tally.max_abs_disagreement_wei.is_zero()
            && has_required_edges
            && has_required_variants;
        require_equal(family, "byte_exact", row.byte_exact, promotion_grade)?;
        require_equal(
            family,
            "PROMOTE-ELIGIBLE verdict",
            row.verdict == "PROMOTE-ELIGIBLE",
            promotion_grade,
        )?;
        transcript_proven[family_index(kind)] = promotion_grade && row_is_promotion_grade(kind, row);
    }
    if !tallies.is_empty() {
        return Err(transcript_mismatch(format!(
            "transcript contains families absent from report: {:?}",
            tallies.keys().collect::<Vec<_>>()
        )));
    }
    Ok(transcript_proven)
}

/// Validate a strict state-matched parity report and return only families whose evidence is safe
/// to authorize. This is deliberately independent of the report producer: the consumer recomputes
/// the hard gate (`max_abs_disagreement_wei == 0`, no unresolved errors, two directions, at least
/// three pools/six inputs in both directions per pool, and the topology-specific edge cases)
/// instead of trusting a
/// claimed verdict string. Malformed, wrong-source-revision, incomplete, tolerance-based, or
/// no-data rows fail closed.
pub fn parity_proven_from_report_json(raw: &str) -> [bool; N_FAMILIES] {
    validate_parity_report_json(raw)
        .map(|proof| proof.proven)
        .unwrap_or([false; N_FAMILIES])
}

fn requested_parity_families(raw: &str) -> Result<[bool; N_FAMILIES], ParityProofError> {
    let mut requested = [false; N_FAMILIES];
    let mut saw_token = false;
    for token in raw.split([',', ' ', '\t']).filter(|s| !s.is_empty()) {
        saw_token = true;
        let kind = family_from_token(token)
            .ok_or_else(|| ParityProofError::UnknownFamily(token.trim().to_string()))?;
        requested[family_index(kind)] = true;
    }
    if !saw_token {
        return Err(ParityProofError::MissingRequestedFamilies);
    }
    Ok(requested)
}

/// Strict startup API for authoritative/express mode. Unlike the compatibility helper below, this
/// preserves the reason evidence is unusable, rejects unknown tokens, requires every requested
/// family to be promotion-grade, verifies the corpus/state-pin and both transcript hashes, and
/// recomputes the transcript row count plus per-family byte differences, errors, pools, directional
/// inputs, edge cases, and variants. It also requires either an exact compile-time source-manifest
/// binding or (as a legacy fallback) a clean build.
pub(crate) fn parity_proven_from_env_strict() -> Result<[bool; N_FAMILIES], ParityProofError> {
    let report_path = std::env::var("FASTLANE_PARITY_REPORT")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .ok_or(ParityProofError::MissingReport)?;
    let requested_raw = std::env::var("FASTLANE_PARITY_PROVEN_FAMILIES")
        .ok()
        .filter(|families| !families.trim().is_empty())
        .ok_or(ParityProofError::MissingRequestedFamilies)?;
    let requested = requested_parity_families(&requested_raw)?;
    let raw = std::fs::read_to_string(&report_path).map_err(|error| ParityProofError::ReadReport {
        path: report_path.clone(),
        error: error.to_string(),
    })?;
    let report_proof = validate_parity_report_json(&raw)?;
    let report_path = std::path::Path::new(&report_path);
    verify_evidence_file(
        "corpus",
        report_path,
        &report_proof.corpus,
        &report_proof.corpus_keccak256,
    )?;
    verify_evidence_file(
        "state_pin",
        report_path,
        &report_proof.state_pin_source,
        &report_proof.state_pin_source_keccak256,
    )?;

    let mut missing = Vec::new();
    for kind in ALL_FAMILIES {
        let idx = family_index(kind);
        if requested[idx] && !report_proof.proven[idx] {
            missing.push(family_token(kind).to_string());
        }
    }
    if !missing.is_empty() {
        return Err(ParityProofError::RequestedFamiliesUnproven(missing));
    }
    let transcript_proven = recompute_transcript_proof(report_path, &report_proof)?;
    let transcript_missing = ALL_FAMILIES
        .into_iter()
        .filter(|kind| {
            let idx = family_index(*kind);
            requested[idx] && !transcript_proven[idx]
        })
        .map(|kind| family_token(kind).to_string())
        .collect::<Vec<_>>();
    if !transcript_missing.is_empty() {
        return Err(ParityProofError::RequestedFamiliesUnproven(
            transcript_missing,
        ));
    }
    let build_tree = env!("ENGINE_GIT_DIRTY");
    if !report_proof.manifest_bound && build_tree != "clean" {
        return Err(ParityProofError::UncleanBuild(build_tree.to_string()));
    }
    Ok(requested)
}

/// Parse the explicit tokens in `FASTLANE_PARITY_PROVEN_FAMILIES`. When
/// `FASTLANE_PARITY_REPORT` is configured, validate that strict report with
/// [`parity_proven_from_report_json`] and intersect it with the explicit tokens. An unreadable or
/// invalid configured report fails closed. There is intentionally no `all` shortcut.
pub fn parity_proven_from_env() -> [bool; N_FAMILIES] {
    let Ok(list) = std::env::var("FASTLANE_PARITY_PROVEN_FAMILIES") else {
        return [false; N_FAMILIES];
    };
    let Ok(explicitly_named) = requested_parity_families(&list) else {
        return [false; N_FAMILIES];
    };
    let Ok(path) = std::env::var("FASTLANE_PARITY_REPORT") else {
        return [false; N_FAMILIES];
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return [false; N_FAMILIES];
    };
    let Ok(report_proof) = validate_parity_report_json(&raw) else {
        return [false; N_FAMILIES];
    };
    let mut proven = [false; N_FAMILIES];
    for kind in ALL_FAMILIES {
        let idx = family_index(kind);
        proven[idx] = report_proof.proven[idx] && explicitly_named[idx];
    }
    proven
}

#[cfg(test)]
mod parity_gate_tests {
    use super::*;
    use std::sync::Mutex;

    static PARITY_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn parity_proven_with_env(
        report: Option<&str>,
        families: Option<&str>,
    ) -> [bool; N_FAMILIES] {
        let _guard = PARITY_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let old_report = std::env::var_os("FASTLANE_PARITY_REPORT");
        let old_families = std::env::var_os("FASTLANE_PARITY_PROVEN_FAMILIES");
        match report {
            Some(value) => std::env::set_var("FASTLANE_PARITY_REPORT", value),
            None => std::env::remove_var("FASTLANE_PARITY_REPORT"),
        }
        match families {
            Some(value) => std::env::set_var("FASTLANE_PARITY_PROVEN_FAMILIES", value),
            None => std::env::remove_var("FASTLANE_PARITY_PROVEN_FAMILIES"),
        }

        let proven = parity_proven_from_env();

        match old_report {
            Some(value) => std::env::set_var("FASTLANE_PARITY_REPORT", value),
            None => std::env::remove_var("FASTLANE_PARITY_REPORT"),
        }
        match old_families {
            Some(value) => std::env::set_var("FASTLANE_PARITY_PROVEN_FAMILIES", value),
            None => std::env::remove_var("FASTLANE_PARITY_PROVEN_FAMILIES"),
        }
        proven
    }

    fn parity_strict_with_env(
        report: Option<&str>,
        families: Option<&str>,
    ) -> Result<[bool; N_FAMILIES], ParityProofError> {
        let _guard = PARITY_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let old_report = std::env::var_os("FASTLANE_PARITY_REPORT");
        let old_families = std::env::var_os("FASTLANE_PARITY_PROVEN_FAMILIES");
        match report {
            Some(value) => std::env::set_var("FASTLANE_PARITY_REPORT", value),
            None => std::env::remove_var("FASTLANE_PARITY_REPORT"),
        }
        match families {
            Some(value) => std::env::set_var("FASTLANE_PARITY_PROVEN_FAMILIES", value),
            None => std::env::remove_var("FASTLANE_PARITY_PROVEN_FAMILIES"),
        }

        let result = parity_proven_from_env_strict();

        match old_report {
            Some(value) => std::env::set_var("FASTLANE_PARITY_REPORT", value),
            None => std::env::remove_var("FASTLANE_PARITY_REPORT"),
        }
        match old_families {
            Some(value) => std::env::set_var("FASTLANE_PARITY_PROVEN_FAMILIES", value),
            None => std::env::remove_var("FASTLANE_PARITY_PROVEN_FAMILIES"),
        }
        result
    }

    fn strict_report_row(
        family: &str,
        max_abs_disagreement_wei: &str,
        disagree: u64,
        unresolved_errors: u64,
        pools_checked: u64,
        inputs_checked: u64,
        comparisons: u64,
        directions_checked: u64,
        edge_cases: &[&str],
    ) -> String {
        serde_json::json!({
            "schema_version": 2,
            "strict_byte_exact": true,
            "source_head": env!("ENGINE_GIT_COMMIT"),
            "source_tree_status": env!("ENGINE_GIT_DIRTY"),
            "source_manifest_sha256": option_env!("ENGINE_SOURCE_MANIFEST_SHA256"),
            "block": 465686728,
            "corpus": "saved-corpus.jsonl",
            "corpus_keccak256": "11".repeat(32),
            "state_pin_source": "engine.log",
            "state_pin_source_keccak256": "22".repeat(32),
            "families": {
                family: {
                    "authoritative_reference": "official on-chain quoter",
                    "reference_kind": "official_onchain_quoter",
                    "state_pinned": true,
                    "pools_checked": pools_checked,
                    "inputs_checked": inputs_checked,
                    "pools_with_bidirectional_input_breadth": pools_checked,
                    "min_inputs_per_pool_direction": 6,
                    "comparisons": comparisons,
                    "directions_checked": directions_checked,
                    "max_abs_disagreement_wei": max_abs_disagreement_wei,
                    "disagree": disagree,
                    "unresolved_errors": unresolved_errors,
                    "edge_cases": edge_cases,
                    "variants": [],
                    "byte_exact": max_abs_disagreement_wei == "0" && disagree == 0,
                    "verdict": "PROMOTE-ELIGIBLE"
                }
            }
        })
        .to_string()
    }

    fn hex_keccak(bytes: &[u8]) -> String {
        alloy_primitives::keccak256(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn hex_sha256(bytes: &[u8]) -> String {
        use sha2::Digest as _;

        sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn strict_report_file_fixture(
        label: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "engine-fastlane-evidence-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let corpus = dir.join("corpus.jsonl");
        let state_pin = dir.join("engine.log");
        let report_path = dir.join("report.json");
        let corpus_bytes = b"{\"candidate\":1}\n";
        let state_pin_bytes = b"pin=465686728\n";
        std::fs::write(&corpus, corpus_bytes).unwrap();
        std::fs::write(&state_pin, state_pin_bytes).unwrap();
        let mut report: serde_json::Value = serde_json::from_str(&strict_report_row(
            "uniswap_v3",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &[
                "dust",
                "nominal",
                "extreme",
                "near_tick_boundary",
                "multi_tick_crossing",
            ],
        ))
        .unwrap();
        // Relative paths deliberately prove resolution against the report directory, not cwd.
        report["corpus"] = serde_json::Value::String("corpus.jsonl".into());
        report["corpus_keccak256"] = serde_json::Value::String(hex_keccak(corpus_bytes));
        report["state_pin_source"] = serde_json::Value::String("engine.log".into());
        report["state_pin_source_keccak256"] =
            serde_json::Value::String(hex_keccak(state_pin_bytes));
        std::fs::write(&report_path, report.to_string()).unwrap();
        (report_path, corpus, state_pin)
    }

    #[test]
    fn report_gate_accepts_only_coverage_complete_zero_delta_evidence() {
        let raw = strict_report_row(
            "uniswap_v3",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &[
                "dust",
                "nominal",
                "extreme",
                "near_tick_boundary",
                "multi_tick_crossing",
            ],
        );
        let proven = parity_proven_from_report_json(&raw);
        assert!(proven[family_index(PoolKind::UniswapV3)]);
    }

    #[test]
    fn env_gate_without_report_fails_closed() {
        let proven = parity_proven_with_env(
            None,
            Some("uniswap_v4,uniswap_v3,pancake_v3,algebra_v19"),
        );

        assert!(
            proven.into_iter().all(|is_proven| !is_proven),
            "explicit family names are selection policy, not parity proof"
        );
    }

    #[test]
    fn strict_env_gate_preserves_invalid_vs_valid_negative_reasons() {
        assert_eq!(
            parity_strict_with_env(None, Some("uniswap_v3")),
            Err(ParityProofError::MissingReport)
        );
        assert_eq!(
            parity_strict_with_env(Some("unused"), Some("not_a_family")),
            Err(ParityProofError::UnknownFamily("not_a_family".into()))
        );
    }

    #[test]
    fn strict_env_gate_rejects_a_requested_family_missing_from_valid_proof() {
        let (report, _corpus, _state_pin) = strict_report_file_fixture("unproven-family");
        let result = parity_strict_with_env(report.to_str(), Some("pancake_v3"));
        std::fs::remove_dir_all(report.parent().unwrap()).unwrap();
        assert_eq!(
            result,
            Err(ParityProofError::RequestedFamiliesUnproven(vec![
                "pancake_v3".into()
            ]))
        );
    }

    #[test]
    fn strict_env_gate_rejects_missing_or_mutated_evidence_files() {
        for (label, target, remove) in [
            ("missing-corpus", "corpus", true),
            ("mutated-corpus", "corpus", false),
            ("missing-state-pin", "state_pin", true),
            ("mutated-state-pin", "state_pin", false),
        ] {
            let (report, corpus, state_pin) = strict_report_file_fixture(label);
            let path = if target == "corpus" { &corpus } else { &state_pin };
            if remove {
                std::fs::remove_file(path).unwrap();
            } else {
                use std::io::Write as _;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(path)
                    .unwrap()
                    .write_all(b"tampered\n")
                    .unwrap();
            }
            let result = parity_strict_with_env(report.to_str(), Some("uniswap_v3"));
            let message = result.unwrap_err().to_string();
            assert!(
                message.contains(target) && message.contains("evidence"),
                "{label} must fail because authoritative startup verifies evidence content: {message}"
            );
            std::fs::remove_dir_all(report.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn parity_strict_rejects_forged_aggregate_when_transcript_contains_mismatch() {
        let (report, _corpus, _state_pin) = strict_report_file_fixture("forged-aggregate");
        let transcript = report.parent().unwrap().join("comparison_transcript.jsonl");
        let transcript_bytes = concat!(
            r#"{"schema":"mantis_parity_comparison_v1","candidate_id":"forged:1","family":"uniswap_v3","chain_id":42161,"pin_block":465686728,"pin_block_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","pool_identity":"0x2222222222222222222222222222222222222222222222222222222222222222","token_in":"0x0000000000000000000000000000000000000001","token_out":"0x0000000000000000000000000000000000000002","direction":"zero_for_one","amount_in":"1000000000000000000","kernel_amount_out":"999","authoritative_amount_out":"998","absolute_difference_wei":"1","exact":false,"mechanical_edge_cases":["nominal"],"authoritative_address":"0x0000000000000000000000000000000000000003","outcome":"resolved"}"#,
            "\n"
        )
        .as_bytes();
        std::fs::write(&transcript, transcript_bytes).unwrap();

        let mut report_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
        report_json["comparison_transcript"] =
            serde_json::Value::String("comparison_transcript.jsonl".into());
        report_json["comparison_transcript_sha256"] =
            serde_json::Value::String(hex_sha256(transcript_bytes));
        report_json["comparison_transcript_keccak256"] =
            serde_json::Value::String(hex_keccak(transcript_bytes));
        report_json["comparison_transcript_rows"] = serde_json::Value::from(1_u64);
        report_json["chain"] = serde_json::json!({
            "chain_id": 42161,
            "pinned_blocks": [{
                "number": 465686728,
                "hash": "0x1111111111111111111111111111111111111111111111111111111111111111"
            }],
            "quoters": [{
                "family": "uniswap_v3",
                "pin_block": 465686728,
                "address": "0x0000000000000000000000000000000000000003"
            }]
        });
        std::fs::write(&report, report_json.to_string()).unwrap();

        let result = parity_strict_with_env(report.to_str(), Some("uniswap_v3"));
        std::fs::remove_dir_all(report.parent().unwrap()).unwrap();

        let error = result.expect_err(
            "strict parity must reject a promotion-grade aggregate when its bound transcript contains a byte mismatch",
        );
        let message = error.to_string();
        assert!(
            message.contains("transcript") && message.contains("mismatch"),
            "strict parity rejection must identify the bound transcript mismatch, got: {message}"
        );
    }

    #[test]
    fn strict_transcript_recomputation_accepts_truthful_complete_coverage() {
        let (report, _corpus, _state_pin) = strict_report_file_fixture("truthful-transcript");
        let transcript = report.parent().unwrap().join("comparison_transcript.jsonl");
        let pin_hash = format!("0x{}", "11".repeat(32));
        let quoter = "0x0000000000000000000000000000000000000003";
        let token_low = "0x0000000000000000000000000000000000000001";
        let token_high = "0x0000000000000000000000000000000000000002";
        let mut rows = Vec::new();
        for pool in 1u8..=3 {
            for direction in ["zero_for_one", "one_for_zero"] {
                for amount in 1u8..=6 {
                    let (token_in, token_out) = if direction == "zero_for_one" {
                        (token_low, token_high)
                    } else {
                        (token_high, token_low)
                    };
                    rows.push(
                        serde_json::json!({
                            "schema": "mantis_parity_comparison_v1",
                            "candidate_id": format!("truthful:{pool}:{direction}:{amount}"),
                            "family": "uniswap_v3",
                            "chain_id": 42161,
                            "pin_block": 465686728,
                            "pin_block_hash": pin_hash,
                            "pool_identity": format!("0x{pool:064x}"),
                            "token_in": token_in,
                            "token_out": token_out,
                            "direction": direction,
                            "amount_in": amount.to_string(),
                            "kernel_amount_out": "999",
                            "authoritative_amount_out": "999",
                            "absolute_difference_wei": "0",
                            "exact": true,
                            "mechanical_edge_cases": [
                                "dust", "nominal", "extreme", "near_tick_boundary",
                                "multi_tick_crossing"
                            ],
                            "mechanical_variants": [],
                            "authoritative_address": quoter,
                            "outcome": "resolved"
                        })
                        .to_string(),
                    );
                }
            }
        }
        let transcript_bytes = format!("{}\n", rows.join("\n")).into_bytes();
        std::fs::write(&transcript, &transcript_bytes).unwrap();

        let mut report_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
        report_json["comparison_transcript"] =
            serde_json::Value::String("comparison_transcript.jsonl".into());
        report_json["comparison_transcript_sha256"] =
            serde_json::Value::String(hex_sha256(&transcript_bytes));
        report_json["comparison_transcript_keccak256"] =
            serde_json::Value::String(hex_keccak(&transcript_bytes));
        report_json["comparison_transcript_rows"] = serde_json::Value::from(36_u64);
        report_json["chain"] = serde_json::json!({
            "chain_id": 42161,
            "pinned_blocks": [{"number": 465686728, "hash": pin_hash}],
            "quoters": [{
                "family": "uniswap_v3",
                "pin_block": 465686728,
                "address": quoter
            }]
        });
        std::fs::write(&report, report_json.to_string()).unwrap();

        let raw = std::fs::read_to_string(&report).unwrap();
        let proof = validate_parity_report_json(&raw).unwrap();
        let recomputed = recompute_transcript_proof(&report, &proof).unwrap();
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap()
            .write_all(b"tampered\n")
            .unwrap();
        let mutated = recompute_transcript_proof(&report, &proof).unwrap_err();
        std::fs::remove_dir_all(report.parent().unwrap()).unwrap();

        assert!(recomputed[family_index(PoolKind::UniswapV3)]);
        assert!(matches!(
            mutated,
            ParityProofError::EvidenceHashMismatch {
                kind: "comparison_transcript_sha256",
                ..
            }
        ));
    }

    #[test]
    fn env_gate_with_report_intersects_strict_proof_and_explicit_families() {
        let raw = strict_report_row(
            "uniswap_v3",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &[
                "dust",
                "nominal",
                "extreme",
                "near_tick_boundary",
                "multi_tick_crossing",
            ],
        );
        let path = std::env::temp_dir().join(format!(
            "engine-fastlane-parity-passing-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, raw).expect("write strict parity report");
        let proven = parity_proven_with_env(
            path.to_str(),
            Some("uniswap_v3,pancake_v3"),
        );
        std::fs::remove_file(path).expect("remove strict parity report");

        assert!(proven[family_index(PoolKind::UniswapV3)]);
        assert!(!proven[family_index(PoolKind::PancakeV3)]);
        assert_eq!(proven.into_iter().filter(|is_proven| *is_proven).count(), 1);
    }

    #[test]
    fn env_gate_with_missing_report_fails_closed() {
        let path = std::env::temp_dir().join(format!(
            "engine-fastlane-parity-missing-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let proven = parity_proven_with_env(
            path.to_str(),
            Some("uniswap_v4,uniswap_v3,pancake_v3,algebra_v19"),
        );

        assert!(proven.into_iter().all(|is_proven| !is_proven));
    }

    #[test]
    fn report_gate_rejects_one_wei_and_false_green_conditions() {
        let edges = [
            "dust",
            "nominal",
            "extreme",
            "near_tick_boundary",
            "multi_tick_crossing",
        ];
        for raw in [
            strict_report_row("uniswap_v3", "1", 1, 0, 3, 36, 36, 2, &edges),
            strict_report_row("uniswap_v3", "0", 0, 1, 3, 36, 36, 2, &edges),
            strict_report_row("uniswap_v3", "0", 0, 0, 2, 36, 36, 2, &edges),
            strict_report_row("uniswap_v3", "0", 0, 0, 3, 35, 36, 2, &edges),
            strict_report_row("uniswap_v3", "0", 0, 0, 3, 36, 35, 2, &edges),
            strict_report_row("uniswap_v3", "0", 0, 0, 3, 36, 36, 1, &edges),
            strict_report_row(
                "uniswap_v3",
                "0",
                0,
                0,
                3,
                36,
                36,
                2,
                &["dust", "nominal", "extreme", "multi_tick_crossing"],
            ),
        ] {
            let proven = parity_proven_from_report_json(&raw);
            assert!(
                !proven[family_index(PoolKind::UniswapV3)],
                "incomplete/non-exact evidence must fail closed: {raw}"
            );
        }
    }

    #[test]
    fn report_gate_rejects_evidence_from_another_source_revision() {
        let raw = strict_report_row(
            "uniswap_v3",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &[
                "dust",
                "nominal",
                "extreme",
                "near_tick_boundary",
                "multi_tick_crossing",
            ],
        );
        let mut report: serde_json::Value = serde_json::from_str(&raw).unwrap();
        report["source_head"] = serde_json::Value::String("0".repeat(40));
        let proven = parity_proven_from_report_json(&report.to_string());
        assert!(!proven[family_index(PoolKind::UniswapV3)]);
    }

    #[test]
    fn report_gate_requires_six_inputs_per_direction_on_each_minimum_pool() {
        let raw = strict_report_row(
            "uniswap_v3",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &[
                "dust",
                "nominal",
                "extreme",
                "near_tick_boundary",
                "multi_tick_crossing",
            ],
        );
        let mut report: serde_json::Value = serde_json::from_str(&raw).unwrap();
        report["families"]["uniswap_v3"]["pools_with_bidirectional_input_breadth"] =
            serde_json::Value::from(2);
        let proven = parity_proven_from_report_json(&report.to_string());
        assert!(!proven[family_index(PoolKind::UniswapV3)]);
    }

    #[test]
    fn report_gate_requires_hashed_corpus_and_state_pin_source() {
        let raw = strict_report_row(
            "uniswap_v3",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &[
                "dust",
                "nominal",
                "extreme",
                "near_tick_boundary",
                "multi_tick_crossing",
            ],
        );
        let mut report: serde_json::Value = serde_json::from_str(&raw).unwrap();
        report["corpus_keccak256"] = serde_json::Value::String("not-a-hash".into());
        let proven = parity_proven_from_report_json(&report.to_string());
        assert!(!proven[family_index(PoolKind::UniswapV3)]);
    }

    #[test]
    fn report_gate_requires_every_grouped_family_variant() {
        let raw = strict_report_row(
            "dodo",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &["dust", "nominal", "extreme", "near_empty_reserve"],
        );
        let mut report: serde_json::Value = serde_json::from_str(&raw).unwrap();
        report["families"]["dodo"]["variants"] = serde_json::json!(["dvm", "dpp"]);
        assert!(!parity_proven_from_report_json(&report.to_string())
            [family_index(PoolKind::DodoV2)]);

        report["families"]["dodo"]["variants"] =
            serde_json::json!(["dvm", "dpp", "dsp"]);
        assert!(parity_proven_from_report_json(&report.to_string())
            [family_index(PoolKind::DodoV2)]);
    }

    #[test]
    fn report_gate_requires_family_specific_reserve_edges() {
        let without_near_empty = strict_report_row(
            "uniswap_v2",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &["dust", "nominal", "extreme"],
        );
        assert!(!parity_proven_from_report_json(&without_near_empty)
            [family_index(PoolKind::UniswapV2)]);

        let complete = strict_report_row(
            "uniswap_v2",
            "0",
            0,
            0,
            3,
            36,
            36,
            2,
            &["dust", "nominal", "extreme", "near_empty_reserve"],
        );
        assert!(parity_proven_from_report_json(&complete)[family_index(PoolKind::UniswapV2)]);
    }

    /// W1.2: authoritative promotion is INTERSECTED with the parity proof — an unproven family
    /// demotes (and is reported), a proven one stays, and an empty proof demotes everything.
    #[test]
    fn parity_gate_intersects_promotions() {
        let mut promo = FastlanePromotion::byte_exact_default();
        assert!(promo.is_promoted(PoolKind::UniswapV3));
        let mut proven = [false; N_FAMILIES];
        proven[family_index(PoolKind::UniswapV3)] = true;
        proven[family_index(PoolKind::UniswapV4)] = true;
        let demoted = promo.apply_parity_gate(&proven);
        assert!(promo.is_promoted(PoolKind::UniswapV3), "proven family stays");
        assert!(promo.is_promoted(PoolKind::UniswapV4));
        assert!(!promo.is_promoted(PoolKind::PancakeV3), "unproven family demoted");
        assert!(!promo.is_promoted(PoolKind::UniswapV2));
        assert!(demoted.contains(&family_token(PoolKind::PancakeV3)));
        assert!(!demoted.contains(&family_token(PoolKind::UniswapV3)));

        let mut promo2 = FastlanePromotion::byte_exact_default();
        let none = [false; N_FAMILIES];
        promo2.apply_parity_gate(&none);
        assert_eq!(promo2.promoted_count(), 0, "no proof → nothing authoritative");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_fastlane_env<T>(promoted: Option<&str>, ack: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().expect("fastlane env test lock");
        let old_promoted = std::env::var_os("FASTLANE_PROMOTED_FAMILIES");
        let old_ack = std::env::var_os("FASTLANE_ACK_AGREEMENT_PROVEN");
        match promoted {
            Some(v) => std::env::set_var("FASTLANE_PROMOTED_FAMILIES", v),
            None => std::env::remove_var("FASTLANE_PROMOTED_FAMILIES"),
        }
        match ack {
            Some(v) => std::env::set_var("FASTLANE_ACK_AGREEMENT_PROVEN", v),
            None => std::env::remove_var("FASTLANE_ACK_AGREEMENT_PROVEN"),
        }
        let out = f();
        match old_promoted {
            Some(v) => std::env::set_var("FASTLANE_PROMOTED_FAMILIES", v),
            None => std::env::remove_var("FASTLANE_PROMOTED_FAMILIES"),
        }
        match old_ack {
            Some(v) => std::env::set_var("FASTLANE_ACK_AGREEMENT_PROVEN", v),
            None => std::env::remove_var("FASTLANE_ACK_AGREEMENT_PROVEN"),
        }
        out
    }

    #[test]
    fn family_index_is_unique_and_in_range() {
        let mut seen = [false; N_FAMILIES];
        for k in ALL_FAMILIES {
            let i = family_index(k);
            assert!(i < N_FAMILIES);
            assert!(!seen[i], "family index {i} collided");
            seen[i] = true;
        }
        assert!(seen.iter().all(|&b| b), "every index covered");
    }

    #[test]
    fn family_token_roundtrips() {
        for k in ALL_FAMILIES {
            assert_eq!(family_from_token(family_token(k)), Some(k));
        }
        assert_eq!(family_from_token("not_a_family"), None);
    }

    #[test]
    fn byte_exact_default_contains_only_the_eight_policy_candidates() {
        let p = FastlanePromotion::byte_exact_default();
        assert_eq!(p.promoted_count(), 8);
        for k in BYTE_EXACT_FAMILIES {
            assert!(p.is_promoted(k), "{} must be promoted", family_token(k));
        }
        // Exotic families are NOT promoted by default.
        for k in [
            PoolKind::DodoV2,
            PoolKind::CurveStableSwapNg,
            PoolKind::BalancerStable,
            PoolKind::TraderJoeLb,
            PoolKind::KyberSwapElastic,
            PoolKind::MaverickV2,
            PoolKind::FluidDex,
            PoolKind::CurveCryptoSwap,
        ] {
            assert!(
                !p.is_promoted(k),
                "{} must NOT be promoted by default",
                family_token(k)
            );
        }
    }

    #[test]
    fn env_promotion_requires_evidence_for_non_byte_exact() {
        // Byte-exact families are always promotable (proven), with or without the attestation.
        for k in BYTE_EXACT_FAMILIES {
            assert!(
                env_promotion_allowed(k, false),
                "byte-exact promotes without ack"
            );
            assert!(env_promotion_allowed(k, true));
        }
        // Non-byte-exact (Tier-2) families require the operator's evidence attestation.
        for k in [
            PoolKind::CurveStableSwapNg,
            PoolKind::BalancerStable,
            PoolKind::FluidDex,
            PoolKind::DodoV2,
        ] {
            assert!(
                !env_promotion_allowed(k, false),
                "{} must NOT promote onto the fast lane without the agreement attestation",
                family_token(k)
            );
            assert!(
                env_promotion_allowed(k, true),
                "{} promotes with the attestation",
                family_token(k)
            );
        }
    }

    #[test]
    fn from_env_all_requires_ack_for_non_byte_exact_families() {
        with_fastlane_env(Some("all"), None, || {
            let p = FastlanePromotion::from_env();
            assert!(p.is_promoted(PoolKind::UniswapV2));
            assert!(p.is_promoted(PoolKind::UniswapV3));
            assert!(!p.is_promoted(PoolKind::DodoV2));
            assert!(!p.is_promoted(PoolKind::BalancerWeighted));
            assert_eq!(
                p.promoted_count(),
                BYTE_EXACT_FAMILIES.len(),
                "FASTLANE_PROMOTED_FAMILIES=all without ack must stay byte-exact only"
            );
        });

        with_fastlane_env(Some("all"), Some("1"), || {
            let p = FastlanePromotion::from_env();
            assert!(p.is_promoted(PoolKind::DodoV2));
            assert!(p.is_promoted(PoolKind::BalancerWeighted));
            assert_eq!(p.promoted_count(), N_FAMILIES);
        });
    }

    #[test]
    fn route_eligibility_requires_all_hops_promoted_and_hop_bound() {
        let p = FastlanePromotion::byte_exact_default();
        // All byte-exact → eligible.
        assert!(p.route_families_eligible(&[PoolKind::UniswapV2, PoolKind::UniswapV3]));
        // One exotic hop → ineligible (coverage preserved by routing it elsewhere, not mis-priced).
        assert!(!p.route_families_eligible(&[PoolKind::UniswapV2, PoolKind::DodoV2]));
        // Empty and over-max-hops → ineligible.
        assert!(!p.route_families_eligible(&[]));
        assert!(!p.route_families_eligible(&[
            PoolKind::UniswapV2,
            PoolKind::UniswapV2,
            PoolKind::UniswapV2,
            PoolKind::UniswapV2,
        ]));
    }

    #[test]
    fn promote_extends_the_set() {
        let mut p = FastlanePromotion::byte_exact_default();
        assert!(!p.route_families_eligible(&[PoolKind::DodoV2]));
        p.promote(PoolKind::DodoV2);
        assert!(p.is_promoted(PoolKind::DodoV2));
        assert!(p.route_families_eligible(&[PoolKind::DodoV2]));
        // Mixed promoted route now eligible too.
        assert!(p.route_families_eligible(&[PoolKind::UniswapV3, PoolKind::DodoV2]));
    }

    #[test]
    fn all_promotes_every_family() {
        let p = FastlanePromotion::all(3);
        assert_eq!(p.promoted_count(), N_FAMILIES);
        for k in ALL_FAMILIES {
            assert!(p.is_promoted(k));
        }
    }

    #[test]
    fn standard_erc20_gate_excludes_only_excluded() {
        assert!(token_is_standard_erc20(RiskClass::Permissionless));
        assert!(token_is_standard_erc20(RiskClass::CentralisedStable));
        assert!(!token_is_standard_erc20(RiskClass::Excluded));
    }

    #[test]
    fn promotion_ready_requires_samples_and_zero_disagreement() {
        // Enough agreement, no disagreement → ready.
        assert!(family_promotion_ready(10_000, 0, 10_000));
        // A single disagreement keeps it off the lane regardless of agreement count.
        assert!(!family_promotion_ready(1_000_000, 1, 10_000));
        // Too few samples → not yet ready (no evidence).
        assert!(!family_promotion_ready(5, 0, 10_000));
        // min_samples clamps to ≥1, so zero agreement is never "ready".
        assert!(!family_promotion_ready(0, 0, 0));
        assert!(family_promotion_ready(1, 0, 0));
    }

    #[test]
    fn revert_rate_ppm_is_the_settled_population_fraction() {
        // STAGE 4: revert-rate = reverted / (landed + reverted), in ppm.
        assert_eq!(revert_rate_ppm(0, 0), 0, "nothing settled → no evidence → 0");
        assert_eq!(revert_rate_ppm(100, 0), 0, "all landed → 0 ppm reverted");
        assert_eq!(revert_rate_ppm(0, 100), 1_000_000, "all reverted → 1e6 ppm");
        assert_eq!(revert_rate_ppm(1, 1), 500_000, "half reverted → 500k ppm");
        assert_eq!(revert_rate_ppm(3, 1), 250_000, "1 of 4 reverted → 250k ppm");
        assert_eq!(revert_rate_ppm(999_999, 1), 1, "1 of 1e6 → 1 ppm");
        // No overflow at the u64 ceiling (u128 intermediate).
        assert_eq!(revert_rate_ppm(u64::MAX, 0), 0);
        assert_eq!(revert_rate_ppm(0, u64::MAX), 1_000_000);
    }

    #[test]
    fn mixed_route_is_express_eligible_only_when_every_hop_promoted() {
        // STAGE 4 per-hop eligibility: a MIXED cross-family route rides the S0 express lane iff
        // EVERY hop's family is promoted; a single un-promoted hop keeps the whole route on REVM.
        // This is the exact gate `route_is_fastlane_eligible` delegates to per hop.
        let mixed3 = [PoolKind::UniswapV3, PoolKind::DodoV2, PoolKind::TraderJoeLb];

        // All three families promoted → the mixed route is express-eligible.
        let mut all = FastlanePromotion::empty(3);
        for k in mixed3 {
            all.promote(k);
        }
        assert!(
            all.route_families_eligible(&mixed3),
            "every hop promoted ⇒ express-eligible"
        );
        // Order-independent (the gate is per-hop, not positional).
        assert!(all.route_families_eligible(&[
            PoolKind::TraderJoeLb,
            PoolKind::UniswapV3,
            PoolKind::DodoV2
        ]));

        // Drop ANY one family → the whole mixed route falls back to REVM (never express).
        for dropped in mixed3 {
            let mut p = FastlanePromotion::empty(3);
            for k in mixed3.into_iter().filter(|k| *k != dropped) {
                p.promote(k);
            }
            assert!(
                !p.route_families_eligible(&mixed3),
                "un-promoting {} must make the mixed route ineligible (→ REVM)",
                family_token(dropped)
            );
        }

        // A mixed byte-exact + exotic route: the byte-exact hop alone is NOT enough — the exotic hop
        // must be promoted too (the canonical S4 case: a V2/DODO route needs DODO proven).
        let v2_dodo = [PoolKind::UniswapV2, PoolKind::DodoV2];
        let byte_exact_only = FastlanePromotion::byte_exact_default();
        assert!(
            !byte_exact_only.route_families_eligible(&v2_dodo),
            "byte-exact default does NOT express-route a V2/DODO mixed route (DODO un-promoted)"
        );
        let mut v2_and_dodo = FastlanePromotion::byte_exact_default();
        v2_and_dodo.promote(PoolKind::DodoV2);
        assert!(
            v2_and_dodo.route_families_eligible(&v2_dodo),
            "promoting DODO makes the V2/DODO mixed route express-eligible"
        );
    }
}
