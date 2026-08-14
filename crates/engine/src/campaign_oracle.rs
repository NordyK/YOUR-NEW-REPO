//! WI-01 campaign regression oracle — the non-vacuous baseline every later fix-port phase
//! diffs against (codex plan §4 WI-01, §11; master plan §D.1/§H.4).
//!
//! Surfaces owned here:
//!   * the golden candidate-record schema (10-dimension key + §11.1 grouping dimensions) and
//!     its fixture-set anti-vacuity validator;
//!   * the phase-diff contract (unaffected-corpus multiset equality; vacuous comparisons are
//!     an ERROR, never a pass);
//!   * the correction-manifest contract (every changed row carries old/new identity+output,
//!     issue id, an allowed transition, and canonical-replay/Anvil proof; rows never vanish);
//!   * the hot-path allocation/syscall probe coverage registry (§10.3 authority boundaries);
//!   * the express fail-dark terminal-boundary verdict (§H.4): a known-good corpus must reach
//!     the prepared dry-run / fake-wire TERMINAL boundary with zero raw send — an admission
//!     counter alone is NOT proof of liveness.
//!
//! Enforcement history: the WI-01 RED commit landed these functions as PERMISSIVE stubs and
//! proved (via `tests/campaign_baseline_oracle_static.rs` and the pipeline fail-dark oracle
//! tests) that each named test fails while enforcement is absent; this GREEN state is the real
//! contract behind the identical API.

use std::collections::BTreeMap;

use serde_json::Value;

/// Golden-record schema version stamped into every WI-01 baseline artifact.
pub const GOLDEN_SCHEMA_VERSION: &str = "wi01_golden.v1";

/// The 10-dimension golden key (codex §4 WI-01) plus the §11.1 grouping dimensions
/// (`risk_class`, `price_anchor_class`, `submit_eligibility`). Every golden record MUST carry
/// every one of these keys — a record missing any dimension is rejected, never skipped.
pub const REQUIRED_GOLDEN_DIMENSIONS: [&str; 15] = [
    "trigger_tx_hash",
    "feed_sequence",
    "inner_ordinal",
    "route_static_hash",
    "strategy_kind",
    "ordered_pool_kind_signature",
    "direction",
    "amount_bucket",
    "decision",
    "typed_decline_reason",
    "risk_class",
    "price_anchor_class",
    "submit_eligibility",
    // WI-01b Gap 1: the candidate INPUT/OUTPUT are first-class golden dimensions — a changed
    // arb output must never diff as "unchanged". `cycle_amount_out` is a decimal string for a
    // positive candidate and JSON null for a typed decline.
    "amount_in",
    "cycle_amount_out",
];

/// All 18 `PoolKind` variant names, in enum-discriminant order (engine-state `pool.rs`).
/// The golden fixture set must exercise every one of these under `ACTIVE_FAMILIES=all`.
pub const POOL_KIND_CATALOG: [&str; 18] = [
    "UniswapV2",
    "SushiV2",
    "CamelotV2",
    "UniswapV3",
    "SushiV3",
    "PancakeV3",
    "AlgebraV19",
    "CurveStableSwapNg",
    "CurveCryptoSwap",
    "TraderJoeLb",
    "MaverickV2",
    "FluidDex",
    "DodoV2",
    "KyberSwapElastic",
    "SolidlyV3",
    "UniswapV4",
    "BalancerStable",
    "BalancerWeighted",
];

/// Every compiled `StrategyKind` variant name (engine-routes `table.rs`).
pub const STRATEGY_KIND_CATALOG: [&str; 8] = [
    "TwoHop", "Triangle", "FourHop", "FiveHop", "SixHop", "SevenHop", "EightHop", "SelfImpact",
];

/// `decision` vocabulary: a golden row is either a positive candidate or a typed decline
/// (the §11.2 transition vocabulary).
pub const DECISION_CANDIDATE: &str = "candidate";
/// See [`DECISION_CANDIDATE`].
pub const DECISION_TYPED_DECLINE: &str = "typed_decline";

/// Schema violations surfaced by the golden-record validators.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GoldenSchemaError {
    /// A record omitted one of [`REQUIRED_GOLDEN_DIMENSIONS`].
    #[error("golden record {row} is missing required dimension `{dimension}`")]
    MissingDimension {
        /// Row index in the fixture set.
        row: usize,
        /// The absent dimension key.
        dimension: &'static str,
    },
    /// A record carried a dimension with an invalid/unknown value.
    #[error("golden record {row} has invalid `{dimension}`: {detail}")]
    InvalidDimension {
        /// Row index in the fixture set.
        row: usize,
        /// The offending dimension key.
        dimension: &'static str,
        /// Human-readable detail.
        detail: String,
    },
    /// The fixture set is empty — an empty baseline can only compare vacuously.
    #[error("golden fixture set is empty — a vacuous baseline is an error, not a pass")]
    EmptyFixtureSet,
    /// A required coverage dimension has zero POSITIVE fixtures.
    #[error("golden fixture set has zero positive fixtures for `{dimension}`")]
    ZeroPositiveFixtures {
        /// The starved dimension (e.g. a strategy kind or family name).
        dimension: String,
    },
}

/// Aggregate coverage summary for a validated golden fixture set.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GoldenSummary {
    /// Total records.
    pub rows: u64,
    /// Records with `decision == "candidate"`.
    pub positives: u64,
    /// Records with `decision == "typed_decline"`.
    pub typed_declines: u64,
    /// Positive-fixture count per strategy kind.
    pub positives_by_strategy: BTreeMap<String, u64>,
    /// Row count per pool-kind family (a family counts once per row it appears in).
    pub rows_by_family: BTreeMap<String, u64>,
    /// Positive-fixture count per direction signature.
    pub positives_by_direction: BTreeMap<String, u64>,
    /// Positive-fixture count per amount bucket.
    pub positives_by_amount_bucket: BTreeMap<String, u64>,
}

fn require_str<'a>(
    row: usize,
    record: &'a Value,
    dimension: &'static str,
) -> Result<&'a str, GoldenSchemaError> {
    record
        .get(dimension)
        .ok_or(GoldenSchemaError::MissingDimension { row, dimension })?
        .as_str()
        .ok_or_else(|| GoldenSchemaError::InvalidDimension {
            row,
            dimension,
            detail: "must be a string".into(),
        })
}

fn require_u64(row: usize, record: &Value, dimension: &'static str) -> Result<u64, GoldenSchemaError> {
    record
        .get(dimension)
        .ok_or(GoldenSchemaError::MissingDimension { row, dimension })?
        .as_u64()
        .ok_or_else(|| GoldenSchemaError::InvalidDimension {
            row,
            dimension,
            detail: "must be an unsigned integer".into(),
        })
}

/// Validate one golden record against the schema: every required dimension present, every
/// vocabulary dimension drawn from its closed catalog, and decision/decline coherent.
pub fn validate_golden_record(row: usize, record: &Value) -> Result<(), GoldenSchemaError> {
    if !record.is_object() {
        return Err(GoldenSchemaError::InvalidDimension {
            row,
            dimension: "record",
            detail: "golden record must be a JSON object".into(),
        });
    }
    // Presence first, in schema order, so the FIRST missing dimension is named.
    for dimension in REQUIRED_GOLDEN_DIMENSIONS {
        if record.get(dimension).is_none() {
            return Err(GoldenSchemaError::MissingDimension { row, dimension });
        }
    }
    let tx = require_str(row, record, "trigger_tx_hash")?;
    if !(tx.len() == 66 && tx.starts_with("0x") && tx[2..].chars().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(GoldenSchemaError::InvalidDimension {
            row,
            dimension: "trigger_tx_hash",
            detail: format!("`{tx}` is not a 0x-prefixed 32-byte hex hash"),
        });
    }
    require_u64(row, record, "feed_sequence")?;
    require_u64(row, record, "inner_ordinal")?;
    let static_hash = require_str(row, record, "route_static_hash")?;
    if !static_hash.starts_with("0x") || static_hash.len() < 6 {
        return Err(GoldenSchemaError::InvalidDimension {
            row,
            dimension: "route_static_hash",
            detail: format!("`{static_hash}` is not a 0x-prefixed hash"),
        });
    }
    let strategy = require_str(row, record, "strategy_kind")?;
    if !STRATEGY_KIND_CATALOG.contains(&strategy) {
        return Err(GoldenSchemaError::InvalidDimension {
            row,
            dimension: "strategy_kind",
            detail: format!("unknown strategy `{strategy}`"),
        });
    }
    let signature = require_str(row, record, "ordered_pool_kind_signature")?;
    if signature.is_empty() {
        return Err(GoldenSchemaError::InvalidDimension {
            row,
            dimension: "ordered_pool_kind_signature",
            detail: "empty signature".into(),
        });
    }
    for family in signature.split('>') {
        if !POOL_KIND_CATALOG.contains(&family) {
            return Err(GoldenSchemaError::InvalidDimension {
                row,
                dimension: "ordered_pool_kind_signature",
                detail: format!("unknown family `{family}` in `{signature}`"),
            });
        }
    }
    let direction = require_str(row, record, "direction")?;
    if direction.is_empty() || !direction.chars().all(|c| c == '0' || c == '1') {
        return Err(GoldenSchemaError::InvalidDimension {
            row,
            dimension: "direction",
            detail: format!("`{direction}` is not a per-hop 0/1 string"),
        });
    }
    require_u64(row, record, "amount_bucket")?;
    let decision = require_str(row, record, "decision")?;
    let decline_reason = require_str(row, record, "typed_decline_reason")?;
    match decision {
        DECISION_CANDIDATE => {
            if decline_reason != "none" {
                return Err(GoldenSchemaError::InvalidDimension {
                    row,
                    dimension: "typed_decline_reason",
                    detail: format!("positive candidate must carry `none`, got `{decline_reason}`"),
                });
            }
        }
        DECISION_TYPED_DECLINE => {
            if decline_reason.is_empty() || decline_reason == "none" {
                return Err(GoldenSchemaError::InvalidDimension {
                    row,
                    dimension: "typed_decline_reason",
                    detail: "a typed decline must carry its real typed reason".into(),
                });
            }
        }
        other => {
            return Err(GoldenSchemaError::InvalidDimension {
                row,
                dimension: "decision",
                detail: format!("unknown decision `{other}`"),
            });
        }
    }
    // WI-01b Gap 1: candidate input/output are validated dimensions. `amount_in` is a decimal
    // string; `cycle_amount_out` must be a decimal string for a positive candidate and may be
    // null for a typed decline (an unpriceable cycle has no output).
    let amount_in = require_str(row, record, "amount_in")?;
    if amount_in.is_empty() || !amount_in.chars().all(|c| c.is_ascii_digit()) {
        return Err(GoldenSchemaError::InvalidDimension {
            row,
            dimension: "amount_in",
            detail: format!("`{amount_in}` is not a decimal amount string"),
        });
    }
    let out_value = record.get("cycle_amount_out").expect("presence checked above");
    match out_value.as_str() {
        Some(out) if !out.is_empty() && out.chars().all(|c| c.is_ascii_digit()) => {}
        _ if out_value.is_null() && decision == DECISION_TYPED_DECLINE => {}
        _ => {
            return Err(GoldenSchemaError::InvalidDimension {
                row,
                dimension: "cycle_amount_out",
                detail: format!(
                    "`{out_value}` must be a decimal string (nullable only for typed declines)"
                ),
            });
        }
    }
    for dimension in ["risk_class", "price_anchor_class", "submit_eligibility"] {
        // Presence is already guaranteed above; enforce non-empty string content.
        let dim: &'static str = match dimension {
            "risk_class" => "risk_class",
            "price_anchor_class" => "price_anchor_class",
            _ => "submit_eligibility",
        };
        let v = require_str(row, record, dim)?;
        if v.is_empty() {
            return Err(GoldenSchemaError::InvalidDimension {
                row,
                dimension: dim,
                detail: "must be non-empty".into(),
            });
        }
    }
    Ok(())
}

/// Validate a whole golden fixture set: every record valid, set non-empty, and at least one
/// POSITIVE fixture overall (per-dimension anti-vacuity floors are asserted by the baseline
/// tests over the returned summary, so starved dimensions are named exactly).
pub fn validate_golden_fixture_set(records: &[Value]) -> Result<GoldenSummary, GoldenSchemaError> {
    if records.is_empty() {
        return Err(GoldenSchemaError::EmptyFixtureSet);
    }
    let mut summary = GoldenSummary::default();
    for (row, record) in records.iter().enumerate() {
        validate_golden_record(row, record)?;
        summary.rows += 1;
        let decision = record["decision"].as_str().expect("validated above");
        let positive = decision == DECISION_CANDIDATE;
        if positive {
            summary.positives += 1;
        } else {
            summary.typed_declines += 1;
        }
        let strategy = record["strategy_kind"].as_str().expect("validated");
        let signature = record["ordered_pool_kind_signature"].as_str().expect("validated");
        let mut seen_families: Vec<&str> = signature.split('>').collect();
        seen_families.sort_unstable();
        seen_families.dedup();
        for family in seen_families {
            *summary.rows_by_family.entry(family.to_string()).or_insert(0) += 1;
        }
        if positive {
            *summary
                .positives_by_strategy
                .entry(strategy.to_string())
                .or_insert(0) += 1;
            let direction = record["direction"].as_str().expect("validated");
            *summary
                .positives_by_direction
                .entry(direction.to_string())
                .or_insert(0) += 1;
            let bucket = record["amount_bucket"].as_u64().expect("validated");
            *summary
                .positives_by_amount_bucket
                .entry(bucket.to_string())
                .or_insert(0) += 1;
        }
    }
    if summary.positives == 0 {
        return Err(GoldenSchemaError::ZeroPositiveFixtures {
            dimension: "decision=candidate".into(),
        });
    }
    Ok(summary)
}

/// One changed row in a phase diff (same golden key, different decision/output — or a key that
/// disappeared/appeared between baseline and candidate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedRow {
    /// Canonical golden key of the affected row (JSON-encoded key tuple).
    pub key: String,
    /// What changed: `changed` | `removed` | `added`.
    pub change: String,
    /// The exact baseline records at this identity (empty for `added`).
    pub old_records: Vec<Value>,
    /// The exact candidate records at this identity (empty for `removed`).
    pub new_records: Vec<Value>,
}

/// Result of diffing a candidate fixture set against the Phase-0 baseline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PhaseDiff {
    /// Rows identical in both sets.
    pub unchanged: u64,
    /// Rows whose key exists in both sets but whose decision/output differs, plus
    /// removed/added keys. Every entry requires a correction-manifest row.
    pub changed: Vec<ChangedRow>,
}

/// Phase-diff contract violations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PhaseDiffError {
    /// Baseline or candidate set failed schema validation.
    #[error("phase diff input rejected: {0}")]
    Schema(#[from] GoldenSchemaError),
    /// The comparison would be vacuous (empty baseline or zero positive baseline fixtures).
    #[error("phase diff is vacuous: {detail}")]
    Vacuous {
        /// Why the comparison proves nothing.
        detail: String,
    },
}

/// The identity half of the golden key (everything except decision/typed_decline_reason).
/// Rows pair by identity across baseline/candidate; the outcome half is what a diff compares.
fn golden_identity_key(record: &Value) -> String {
    serde_json::json!([
        record["trigger_tx_hash"],
        record["feed_sequence"],
        record["inner_ordinal"],
        record["route_static_hash"],
        record["strategy_kind"],
        record["ordered_pool_kind_signature"],
        record["direction"],
        record["amount_bucket"],
    ])
    .to_string()
}

/// The outcome half of a golden row: the CANONICAL FULL RECORD (sorted-key serialization of
/// every field). Any drift — decision, typed reason, candidate input/output, submit
/// eligibility, risk/anchor class, or any future field — surfaces as a changed row. (WI-01b
/// Gap 1: the previous decision-only outcome let a changed arb output diff as unchanged.)
fn golden_outcome(record: &Value) -> String {
    serde_json::to_string(record).expect("golden record serializes")
}

/// Diff a candidate golden fixture set against the baseline. A vacuous comparison (empty or
/// zero-positive baseline, or schema-invalid rows) FAILS — an oracle that compares nothing
/// must not pass. Rows pair by the 8-dimension identity key; outcome changes, removals, and
/// additions are surfaced as changed rows for the correction manifest to authorize.
pub fn phase_diff(baseline: &[Value], candidate: &[Value]) -> Result<PhaseDiff, PhaseDiffError> {
    if baseline.is_empty() {
        return Err(PhaseDiffError::Vacuous {
            detail: "baseline fixture set is empty".into(),
        });
    }
    let baseline_summary = validate_golden_fixture_set(baseline)?;
    if baseline_summary.positives == 0 {
        return Err(PhaseDiffError::Vacuous {
            detail: "baseline carries zero positive candidate fixtures".into(),
        });
    }
    for (row, record) in candidate.iter().enumerate() {
        validate_golden_record(row, record)?;
    }
    // identity → sorted multiset of outcomes (a corpus may legitimately carry multiple rows
    // with one identity, e.g. re-fires; outcome multisets must then match exactly).
    let index = |rows: &[Value]| -> BTreeMap<String, Vec<String>> {
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for record in rows {
            map.entry(golden_identity_key(record))
                .or_default()
                .push(golden_outcome(record));
        }
        for outcomes in map.values_mut() {
            outcomes.sort_unstable();
        }
        map
    };
    let records_at = |rows: &[Value], key: &str| -> Vec<Value> {
        rows.iter()
            .filter(|r| golden_identity_key(r) == key)
            .cloned()
            .collect()
    };
    let base = index(baseline);
    let cand = index(candidate);
    let mut diff = PhaseDiff::default();
    for (key, base_outcomes) in &base {
        match cand.get(key) {
            Some(cand_outcomes) if cand_outcomes == base_outcomes => {
                diff.unchanged += base_outcomes.len() as u64;
            }
            Some(_) => diff.changed.push(ChangedRow {
                key: key.clone(),
                change: "changed".into(),
                old_records: records_at(baseline, key),
                new_records: records_at(candidate, key),
            }),
            None => diff.changed.push(ChangedRow {
                key: key.clone(),
                change: "removed".into(),
                old_records: records_at(baseline, key),
                new_records: Vec::new(),
            }),
        }
    }
    for key in cand.keys() {
        if !base.contains_key(key) {
            diff.changed.push(ChangedRow {
                key: key.clone(),
                change: "added".into(),
                old_records: Vec::new(),
                new_records: records_at(candidate, key),
            });
        }
    }
    Ok(diff)
}

/// Correction-manifest contract violations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CorrectionManifestError {
    /// A changed row has no manifest entry.
    #[error("changed row `{key}` has no correction-manifest entry")]
    UnmanifestedChange {
        /// The changed row's golden key.
        key: String,
    },
    /// A manifest entry is missing a required field.
    #[error("correction manifest entry `{key}` is missing `{field}`")]
    MissingField {
        /// The entry's golden key.
        key: String,
        /// The absent field.
        field: &'static str,
    },
    /// A manifest entry lacks canonical replay / Anvil proof.
    #[error("correction manifest entry `{key}` carries no canonical replay/Anvil proof")]
    MissingProof {
        /// The entry's golden key.
        key: String,
    },
    /// A manifest entry declares a transition outside the reviewed §11.2 set.
    #[error("correction manifest entry `{key}` declares forbidden transition `{transition}`")]
    ForbiddenTransition {
        /// The entry's golden key.
        key: String,
        /// The declared transition.
        transition: String,
    },
    /// The proof artifact's replay payloads do not equal the canonical old/new values derived
    /// from the actual changed records — a hash-valid but UNRELATED file proves nothing.
    #[error("correction `{key}` proof does not bind this correction's identity: {detail}")]
    ProofIdentityMismatch {
        /// The entry's golden key.
        key: String,
        /// What diverged.
        detail: String,
    },
    /// A row disappeared — no manifest can authorize row disappearance.
    #[error("row `{key}` disappeared from the corpus — no row may disappear (§11.2)")]
    RowDisappeared {
        /// The removed row's golden key.
        key: String,
    },
}

/// The reviewed transitions a correction manifest may declare (§11.2).
pub const ALLOWED_CORRECTION_TRANSITIONS: [&str; 3] = [
    "candidate->typed_decline",
    "candidate->corrected_candidate",
    "typed_decline->proven_candidate",
];

/// Validate a correction manifest against a phase diff: every changed row must carry old
/// identity/output, new identity/output, an issue id, an allowed transition, and a canonical
/// replay / Anvil proof reference. Removed rows are always an error.
pub fn validate_correction_manifest(
    base_dir: &std::path::Path,
    manifest: &[Value],
    diff: &PhaseDiff,
) -> Result<(), CorrectionManifestError> {
    let mut by_key: BTreeMap<&str, &Value> = BTreeMap::new();
    // Identity corrections (FL-03 inner_ordinal / FL-07 route class): old_key -> (new_key, entry).
    let mut identity_links: BTreeMap<String, (String, &Value)> = BTreeMap::new();
    for entry in manifest {
        let key = entry
            .get("key")
            .and_then(Value::as_str)
            .ok_or(CorrectionManifestError::MissingField {
                key: "<unkeyed entry>".into(),
                field: "key",
            })?;
        by_key.insert(key, entry);
        if let Some(link) = entry.get("identity_correction") {
            let old_key = link.get("old_key").and_then(Value::as_str).ok_or(
                CorrectionManifestError::MissingField {
                    key: key.to_string(),
                    field: "identity_correction",
                },
            )?;
            let new_key = link.get("new_key").and_then(Value::as_str).ok_or(
                CorrectionManifestError::MissingField {
                    key: key.to_string(),
                    field: "identity_correction",
                },
            )?;
            // WI-01c Gap 2d: the link must be SELF-CONSISTENT (entry keyed by the removed row)
            // and INJECTIVE (no two removals may share one addition).
            if key != old_key {
                return Err(CorrectionManifestError::ForbiddenTransition {
                    key: key.to_string(),
                    transition: format!(
                        "identity link keyed `{key}` but claims old_key `{old_key}` — a link                          must be keyed by the removed row it authorizes"
                    ),
                });
            }
            if identity_links
                .values()
                .any(|(existing_new, _)| existing_new == new_key)
            {
                return Err(CorrectionManifestError::ForbiddenTransition {
                    key: key.to_string(),
                    transition: format!(
                        "identity link target `{new_key}` already consumed — two removals must                          never launder through one addition"
                    ),
                });
            }
            identity_links.insert(old_key.to_string(), (new_key.to_string(), entry));
        }
    }
    let added_keys: BTreeMap<&str, &ChangedRow> = diff
        .changed
        .iter()
        .filter(|r| r.change == "added")
        .map(|r| (r.key.as_str(), r))
        .collect();
    let mut consumed_added: Vec<String> = Vec::new();
    for row in &diff.changed {
        match row.change.as_str() {
            "removed" => {
                // §11.2: a removed row is fatal UNLESS a manifest identity-correction entry
                // links it to a present added row (the plan's own FL-03/FL-07 corrections
                // legitimately change identity-key fields).
                let Some((new_key, entry)) = identity_links.get(&row.key) else {
                    return Err(CorrectionManifestError::RowDisappeared {
                        key: row.key.clone(),
                    });
                };
                let Some(added) = added_keys.get(new_key.as_str()) else {
                    return Err(CorrectionManifestError::RowDisappeared {
                        key: row.key.clone(),
                    });
                };
                // WI-01c Gap 2d: the linked pair may differ ONLY in the reviewed FL-03/FL-07
                // identity fields (inner_ordinal, route_static_hash) — any other identity
                // drift is not an identity correction, it is a disappearance.
                let old_identity = canonical_identity_payload(&row.old_records);
                let new_identity = canonical_identity_payload(&added.new_records);
                if let (Some(old_map), Some(new_map)) =
                    (old_identity.as_object(), new_identity.as_object())
                {
                    for (dim, old_value) in old_map {
                        if matches!(dim.as_str(), "inner_ordinal" | "route_static_hash") {
                            continue;
                        }
                        if new_map.get(dim) != Some(old_value) {
                            return Err(CorrectionManifestError::ForbiddenTransition {
                                key: row.key.clone(),
                                transition: format!(
                                    "identity link changes `{dim}` — only the reviewed                                      FL-03 (inner_ordinal) / FL-07 (route_static_hash)                                      identity fields may change"
                                ),
                            });
                        }
                    }
                } else {
                    return Err(CorrectionManifestError::RowDisappeared {
                        key: row.key.clone(),
                    });
                }
                validate_correction_entry(base_dir, entry, &row.key, row, added)?;
                consumed_added.push(new_key.clone());
            }
            "added" => {} // adjudicated below once identity links are consumed
            _ => {
                let entry = by_key.get(row.key.as_str()).ok_or_else(|| {
                    CorrectionManifestError::UnmanifestedChange {
                        key: row.key.clone(),
                    }
                })?;
                validate_correction_entry(base_dir, entry, &row.key, row, row)?;
            }
        }
    }
    for row in diff.changed.iter().filter(|r| r.change == "added") {
        if consumed_added.contains(&row.key) {
            continue;
        }
        let entry = by_key.get(row.key.as_str()).ok_or_else(|| {
            CorrectionManifestError::UnmanifestedChange {
                key: row.key.clone(),
            }
        })?;
        validate_correction_entry(base_dir, entry, &row.key, row, row)?;
    }
    Ok(())
}

/// Derive the §11.2 transition from the ACTUAL records, TOTALLY over the multiset: every old
/// record must share one decision and every new record likewise (a mixed-decision multiset has
/// no single reviewed transition), decline->decline drift has no reviewed class, and a
/// standalone addition (no old records) is underivable — additions are only representable
/// through an identity link.
fn derive_transition(
    old_side: &ChangedRow,
    new_side: &ChangedRow,
) -> Result<&'static str, String> {
    let uniform_decision = |records: &[Value], side: &str| -> Result<String, String> {
        let mut decisions: Vec<&str> = records
            .iter()
            .map(|r| r.get("decision").and_then(Value::as_str).unwrap_or(""))
            .collect();
        decisions.sort_unstable();
        decisions.dedup();
        match decisions.as_slice() {
            [one] if !one.is_empty() => Ok(one.to_string()),
            [] => Err(format!("{side} side has no records — transition underivable")),
            _ => Err(format!(
                "{side} side is a MIXED-decision multiset {decisions:?} — no single reviewed                  transition exists"
            )),
        }
    };
    let old_decision = uniform_decision(&old_side.old_records, "old")?;
    let new_decision = uniform_decision(&new_side.new_records, "new")?;
    match (old_decision.as_str(), new_decision.as_str()) {
        (DECISION_CANDIDATE, DECISION_CANDIDATE) => Ok("candidate->corrected_candidate"),
        (DECISION_CANDIDATE, DECISION_TYPED_DECLINE) => Ok("candidate->typed_decline"),
        (DECISION_TYPED_DECLINE, DECISION_CANDIDATE) => Ok("typed_decline->proven_candidate"),
        (DECISION_TYPED_DECLINE, DECISION_TYPED_DECLINE) => Err(
            "decline->decline drift has no reviewed §11.2 transition class".to_string(),
        ),
        (a, b) => Err(format!("underivable transition {a}->{b}")),
    }
}

fn validate_correction_entry(
    base_dir: &std::path::Path,
    entry: &Value,
    key: &str,
    old_side: &ChangedRow,
    new_side: &ChangedRow,
) -> Result<(), CorrectionManifestError> {
    for field in [
        "issue_id",
        "transition",
        "old_identity",
        "old_output",
        "new_identity",
        "new_output",
    ] {
        if entry.get(field).is_none() {
            return Err(CorrectionManifestError::MissingField {
                key: key.to_string(),
                field: match field {
                    "issue_id" => "issue_id",
                    "transition" => "transition",
                    "old_identity" => "old_identity",
                    "old_output" => "old_output",
                    "new_identity" => "new_identity",
                    _ => "new_output",
                },
            });
        }
    }
    let transition = entry["transition"].as_str().unwrap_or("");
    if !ALLOWED_CORRECTION_TRANSITIONS.contains(&transition) {
        return Err(CorrectionManifestError::ForbiddenTransition {
            key: key.to_string(),
            transition: transition.to_string(),
        });
    }
    // WI-01c Gap 2c: the declared transition must MATCH the transition derived TOTALLY from
    // the actual old/new record multisets — mixed multisets, decline->decline drift, and
    // standalone additions are all underivable and therefore unauthorizable.
    match derive_transition(old_side, new_side) {
        Ok(derived) if derived == transition => {}
        Ok(derived) => {
            return Err(CorrectionManifestError::ForbiddenTransition {
                key: key.to_string(),
                transition: format!("declared `{transition}` but actual records derive `{derived}`"),
            });
        }
        Err(detail) => {
            return Err(CorrectionManifestError::ForbiddenTransition {
                key: key.to_string(),
                transition: format!("declared `{transition}` but {detail}"),
            });
        }
    }
    // WI-01d: record multiplicity is CONSERVED across a correction — N old records must map
    // to N new records (a 2→1 collapse is a disappearance, which no manifest may authorize;
    // additions/removals of re-fire records require a baseline recapture, not a correction).
    if old_side.old_records.len() != new_side.new_records.len() {
        return Err(CorrectionManifestError::RowDisappeared {
            key: key.to_string(),
        });
    }
    // WI-01c Gap 2a: the MANIFEST's old/new claims must equal the canonical values derived
    // from the actual changed records — presence is not truth.
    let canonical_old_identity = canonical_identity_payload(&old_side.old_records);
    let canonical_new_identity = canonical_identity_payload(&new_side.new_records);
    let canonical_old_output = canonical_outcome_payload(&old_side.old_records);
    let canonical_new_output = canonical_outcome_payload(&new_side.new_records);
    for (field, canonical) in [
        ("old_identity", &canonical_old_identity),
        ("old_output", &canonical_old_output),
        ("new_identity", &canonical_new_identity),
        ("new_output", &canonical_new_output),
    ] {
        if &entry[field] != canonical {
            return Err(CorrectionManifestError::ProofIdentityMismatch {
                key: key.to_string(),
                detail: format!("manifest `{field}` does not equal the canonical value"),
            });
        }
    }
    // WI-01b Gap 2a: proof must be a PRESENT, hash-bound, schema-valid canonical-replay/Anvil
    // artifact — metadata alone is not proof, and a fabricated reference must never pass.
    let proof = entry
        .get("proof")
        .ok_or_else(|| CorrectionManifestError::MissingProof {
            key: key.to_string(),
        })?;
    let kind_ok = matches!(
        proof.get("kind").and_then(Value::as_str),
        Some("canonical_replay") | Some("anvil")
    );
    let reference = proof.get("reference").and_then(Value::as_str).unwrap_or("");
    let pinned_sha = proof.get("sha256").and_then(Value::as_str).unwrap_or("");
    if !kind_ok || reference.is_empty() || pinned_sha.len() != 64 {
        return Err(CorrectionManifestError::MissingProof {
            key: key.to_string(),
        });
    }
    // WI-01c Gap 2e: PATH POLICY FIRST — the proof must be a committed artifact under the
    // proof root; absolute paths and `..` traversal never reach the filesystem.
    let reference_path = std::path::Path::new(reference);
    let escapes = reference_path.is_absolute()
        || reference_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        || !reference.starts_with(PROOF_ROOT);
    if escapes {
        return Err(CorrectionManifestError::MissingProof {
            key: key.to_string(),
        });
    }
    let path = base_dir.join(reference);
    let Ok(bytes) = std::fs::read(&path) else {
        return Err(CorrectionManifestError::MissingProof {
            key: key.to_string(),
        });
    };
    if sha256_hex(&bytes) != pinned_sha.to_ascii_lowercase() {
        return Err(CorrectionManifestError::MissingProof {
            key: key.to_string(),
        });
    }
    let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
        return Err(CorrectionManifestError::MissingProof {
            key: key.to_string(),
        });
    };
    // WI-01c Gap 2b: TYPED kind-specific schema + typed source pins.
    let kind = proof.get("kind").and_then(Value::as_str).unwrap_or("");
    let required_schema = match kind {
        "canonical_replay" => "wi01_canonical_replay_proof.v1",
        _ => "wi01_anvil_proof.v1",
    };
    if doc.get("schema").and_then(Value::as_str) != Some(required_schema) {
        return Err(CorrectionManifestError::MissingProof {
            key: key.to_string(),
        });
    }
    let source = doc.get("source").and_then(Value::as_object).cloned().unwrap_or_default();
    let commit_ok = source
        .get("commit")
        .and_then(Value::as_str)
        .is_some_and(|c| c.len() == 40 && c.chars().all(|ch| ch.is_ascii_hexdigit()));
    let config_ok = source
        .get("config_sha256")
        .and_then(Value::as_str)
        .is_some_and(|c| c.len() == 64 && c.chars().all(|ch| ch.is_ascii_hexdigit()));
    let corpus_ok = source
        .get("corpus")
        .and_then(Value::as_str)
        .is_some_and(|c| !c.is_empty());
    if !commit_ok || !config_ok || !corpus_ok {
        return Err(CorrectionManifestError::MissingProof {
            key: key.to_string(),
        });
    }
    // WI-01c Gap 2a: the proof's REPLAY payloads must equal the canonical old/new values —
    // a hash-valid but unrelated file proves file identity, not correction identity.
    let replay = &doc["replay"];
    for (field, canonical) in [
        ("old_identity", &canonical_old_identity),
        ("old_output", &canonical_old_output),
        ("new_identity", &canonical_new_identity),
        ("new_output", &canonical_new_output),
    ] {
        if &replay[field] != canonical {
            return Err(CorrectionManifestError::ProofIdentityMismatch {
                key: key.to_string(),
                detail: format!(
                    "proof replay `{field}` does not equal the canonical value derived from                      the changed records"
                ),
            });
        }
    }
    Ok(())
}

/// The only directory a correction proof may live in (WI-01c Gap 2e).
pub const PROOF_ROOT: &str = "codex/campaign_baseline_wi01/proof_fixtures/";

/// WI-01d Gap 3: adjudicate the fail-dark corpus PARTITION on route IDENTITY and CAUSE:
/// `terminal` = routes whose sign+envelope terminal was actually crossed (post-terminal tap),
/// `sampler_tap` = routes the production sampler DECIDED to divert, `drained_revm` = routes
/// found in the REVM ring afterwards. The corpus must equal the DISJOINT union of terminal
/// and sampler-diverted routes, and every REVM-ring occupant must be sampler-caused.
///
pub fn assert_corpus_partition(
    corpus: &std::collections::BTreeSet<u32>,
    terminal: &std::collections::BTreeSet<u32>,
    sampler_tap: &std::collections::BTreeSet<u32>,
    drained_revm: &std::collections::BTreeSet<u32>,
) -> Result<(), TerminalBoundaryFailure> {
    // Counters-shaped failure: expected=corpus, signed=terminal, envelope_ready=sampler
    // decisions, would_submit=REVM-ring occupants — the mismatching sets are printed by the
    // asserting caller via Debug.
    let fail = || TerminalBoundaryFailure::FailDark {
        expected: corpus.len() as u64,
        signed: terminal.len() as u64,
        envelope_ready: sampler_tap.len() as u64,
        would_submit: drained_revm.len() as u64,
    };
    // Every REVM-ring occupant must be sampler-CAUSED, and every sampler decision must have
    // actually landed in the ring — a mismatch is a misroute or a lost diversion.
    if sampler_tap != drained_revm {
        return Err(fail());
    }
    // Terminal and sampled must be DISJOINT and their union must be EXACTLY the corpus.
    if terminal.intersection(sampler_tap).next().is_some() {
        return Err(fail());
    }
    let union: std::collections::BTreeSet<u32> =
        terminal.union(sampler_tap).copied().collect();
    if &union != corpus {
        return Err(fail());
    }
    Ok(())
}


/// Canonical outcome payload of one diff side: the deterministic sorted multiset of the
/// COMPLETE exact records at that identity — the very object `phase_diff` compares (WI-01d:
/// the earlier four-field projection let an approved output correction piggyback a regression
/// in any omitted or future field). The manifest and the proof's replay payloads MUST equal
/// this in full.
pub fn canonical_outcome_payload(records: &[Value]) -> Value {
    let mut rows: Vec<Value> = records.to_vec();
    rows.sort_by_key(|v| serde_json::to_string(v).expect("golden record serializes"));
    Value::Array(rows)
}

/// Canonical identity payload of one diff side: the 8 identity dimensions of the first record
/// (all records at one key share them by construction).
pub fn canonical_identity_payload(records: &[Value]) -> Value {
    let Some(r) = records.first() else {
        return Value::Null;
    };
    serde_json::json!({
        "trigger_tx_hash": r["trigger_tx_hash"],
        "feed_sequence": r["feed_sequence"],
        "inner_ordinal": r["inner_ordinal"],
        "route_static_hash": r["route_static_hash"],
        "strategy_kind": r["strategy_kind"],
        "ordered_pool_kind_signature": r["ordered_pool_kind_signature"],
        "direction": r["direction"],
        "amount_bucket": r["amount_bucket"],
    })
}

/// Pure-Rust SHA-256 (FIPS 180-4), tooling-path only — proof artifacts are validated off the
/// hot path and this keeps the oracle dependency-free and shell-free. Public since WI-02: the
/// `runtime_policy` record/table hashes reuse this same digest (never on a hot core).
pub fn sha256_hex(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = bytes.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

/// The §10.3 authority boundaries the hot-path allocation/syscall probe must declare coverage
/// for. Boundaries that do not exist yet (they land with WI-05..WI-14) must still be DECLARED,
/// with an explicit `pending_work_item` status — silence is a failure.
pub const REQUIRED_AUTHORITY_BOUNDARIES: [&str; 14] = [
    // WI-01d: the WI-02 strategy-genesis boundary is REQUIRED — its pending row (armed
    // binding_tripwire) must exist until the real detector/enumerator/emitter genesis proof
    // is measured across all 18 families.
    "strategy_genesis_all_families",
    // WI-02c: worker-enforcement of the sealed RuntimePolicy.proven_families (object
    // handoff into Pipeline — no second re-resolution in start_threads — plus
    // equality/refuse-mismatch on the fast-lane worker) is DEFERRED to WI-16, which owns
    // the validation-mode matrix + FL-01 live boot gate. Safe: no live capital runs before
    // WI-16 (dry-run, counting-fake transport, no seeded state). The armed pending row +
    // the wi16_sealed_authority_tripwire canary fire if WI-16 lands its rewire without
    // measuring this boundary.
    "sealed_policy_worker_authority",
    "frame_decode_hot_loop",
    "candidate_emission",
    "express_walk_accept_prepare",
    "state_token_stamp",
    "state_token_validate_current",
    "state_token_validate_stale",
    "route_cell_rewrite_under_paused_reader",
    "epoch_read_two_hop",
    "epoch_read_eight_hop",
    "gas_profile_lookup",
    "epoch_collision",
    "max_degree_csr_fanout",
];

/// Declared probe coverage for one authority boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Measured now, in-process: allocation and hot-thread counter deltas captured offline.
    Measured {
        /// Heap allocations observed after warm-up. MUST be 0 when `zero_alloc_contract`;
        /// otherwise this is the FROZEN baseline count later phases may not increase.
        allocs_after_warmup: u64,
        /// Involuntary+voluntary context-switch delta observed during the measured window
        /// (the offline syscall-pressure proxy; a full strace ledger is a live-run artifact).
        ctx_switch_delta: u64,
        /// Whether this boundary is under the §10.3 zero-allocation contract.
        zero_alloc_contract: bool,
    },
    /// The boundary's structure does not exist yet; coverage is owed by the named work item.
    PendingWorkItem {
        /// The owing work item (e.g. "WI-08").
        work_item: String,
        /// Why it cannot be measured before that work item lands.
        reason: String,
    },
}

/// Parsed probe-coverage artifact.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProbeCoverage {
    /// Status per declared boundary.
    pub boundaries: BTreeMap<String, ProbeStatus>,
}

/// Probe-coverage contract violations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProbeCoverageError {
    /// A required boundary is not declared at all.
    #[error("authority boundary `{boundary}` has no probe-coverage declaration")]
    UndeclaredBoundary {
        /// The silent boundary.
        boundary: &'static str,
    },
    /// A declaration is malformed.
    #[error("probe-coverage artifact is malformed: {detail}")]
    Malformed {
        /// Parse/shape detail.
        detail: String,
    },
    /// A measured hot-path boundary reported non-zero allocations after warm-up.
    #[error("boundary `{boundary}` allocated {allocs} time(s) after warm-up on the hot path")]
    HotPathAllocation {
        /// The offending boundary.
        boundary: String,
        /// Allocation count observed.
        allocs: u64,
    },
}

/// Validate the probe-coverage artifact: parse every declared boundary, refuse malformed
/// declarations, and refuse any measured hot-path boundary that allocated after warm-up.
/// (Completeness against [`REQUIRED_AUTHORITY_BOUNDARIES`] is asserted by the baseline test
/// over the returned map so the FIRST silent boundary is named exactly.)
pub fn alloc_syscall_probe_coverage(artifact: &Value) -> Result<ProbeCoverage, ProbeCoverageError> {
    if artifact.get("schema").and_then(Value::as_str) != Some("wi01_probe_coverage.v1") {
        return Err(ProbeCoverageError::Malformed {
            detail: "schema must be wi01_probe_coverage.v1".into(),
        });
    }
    let boundaries = artifact
        .get("boundaries")
        .and_then(Value::as_object)
        .ok_or_else(|| ProbeCoverageError::Malformed {
            detail: "missing `boundaries` object".into(),
        })?;
    let mut coverage = ProbeCoverage::default();
    for (name, decl) in boundaries {
        let status = decl
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| ProbeCoverageError::Malformed {
                detail: format!("boundary `{name}` missing `status`"),
            })?;
        let parsed = match status {
            "measured" => {
                let allocs = decl
                    .get("allocs_after_warmup")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| ProbeCoverageError::Malformed {
                        detail: format!("boundary `{name}` missing allocs_after_warmup"),
                    })?;
                let ctx = decl
                    .get("ctx_switch_delta")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| ProbeCoverageError::Malformed {
                        detail: format!("boundary `{name}` missing ctx_switch_delta"),
                    })?;
                // `zero_alloc_contract` (default true) marks boundaries under the §10.3
                // zero-alloc contract. Boundaries that legitimately allocate TODAY (e.g. the
                // express walk's per-walk scratch) declare `false` — their measured count is
                // FROZEN as the baseline instead, so later phases cannot add allocations
                // without moving a pinned number.
                let zero_alloc_contract = decl
                    .get("zero_alloc_contract")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if zero_alloc_contract && allocs > 0 {
                    return Err(ProbeCoverageError::HotPathAllocation {
                        boundary: name.clone(),
                        allocs,
                    });
                }
                ProbeStatus::Measured {
                    allocs_after_warmup: allocs,
                    ctx_switch_delta: ctx,
                    zero_alloc_contract,
                }
            }
            "pending_work_item" => {
                // WI-01d: a pending boundary MUST declare an armed binding tripwire — a
                // data label that no machine reads arms nothing.
                if decl.get("binding_tripwire").and_then(Value::as_bool) != Some(true) {
                    return Err(ProbeCoverageError::Malformed {
                        detail: format!(
                            "pending boundary `{name}` has no ARMED binding_tripwire"
                        ),
                    });
                }
                let work_item = decl
                    .get("work_item")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| ProbeCoverageError::Malformed {
                        detail: format!("pending boundary `{name}` missing work_item"),
                    })?;
                let reason = decl
                    .get("reason")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| ProbeCoverageError::Malformed {
                        detail: format!("pending boundary `{name}` missing reason"),
                    })?;
                ProbeStatus::PendingWorkItem {
                    work_item: work_item.to_string(),
                    reason: reason.to_string(),
                }
            }
            other => {
                return Err(ProbeCoverageError::Malformed {
                    detail: format!("boundary `{name}` has unknown status `{other}`"),
                })
            }
        };
        coverage.boundaries.insert(name.clone(), parsed);
    }
    Ok(coverage)
}

/// Snapshot of the submit-side terminal-boundary counters, taken before/after driving a corpus
/// through the express lane (pipeline statics: signed, envelope-ready, wire attempts/acks, and
/// the per-loop would-submit decision counter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalBoundaryCounters {
    /// `SIGNED_COUNT`: a real signature was produced (the sign path ran).
    pub signed: u64,
    /// `SUBMIT_ENVELOPE_READY`: the exact would-hit-the-wire `eth_sendRawTransaction` JSON-RPC
    /// body was built. This is the prepared dry-run terminal boundary.
    pub envelope_ready: u64,
    /// `SUBMIT_WIRE_ATTEMPTS`: raw wire writes attempted. MUST NOT move in a dry-run.
    pub wire_attempts: u64,
    /// `SUBMIT_WIRE_ACKS`: sequencer acks received. MUST NOT move in a dry-run.
    pub wire_acks: u64,
    /// The submit loop's per-run would-submit decision counter.
    pub would_submit_decisions: u64,
}

/// Proof that a corpus reached the prepared dry-run terminal boundary with zero raw send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalBoundaryProof {
    /// How many corpus candidates were signed AND envelope-framed (the terminal boundary).
    pub reached_terminal: u64,
}

/// Fail-dark / raw-send verdicts.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TerminalBoundaryFailure {
    /// The lane went DARK: fewer candidates reached the prepared terminal boundary than the
    /// known-good corpus guarantees (the 2026-07-14 fail-dark class).
    #[error(
        "express lane FAIL-DARK: expected >= {expected} candidates at the prepared dry-run \
         terminal boundary, observed signed={signed} envelope_ready={envelope_ready} \
         would_submit={would_submit}"
    )]
    FailDark {
        /// Corpus-guaranteed minimum.
        expected: u64,
        /// Observed signed delta.
        signed: u64,
        /// Observed envelope-ready delta.
        envelope_ready: u64,
        /// Observed would-submit delta.
        would_submit: u64,
    },
    /// Raw bytes moved toward the wire during a dry-run.
    #[error("raw send detected in dry-run: wire_attempts_delta={attempts} wire_acks_delta={acks}")]
    RawSendDetected {
        /// Wire-attempt delta.
        attempts: u64,
        /// Wire-ack delta.
        acks: u64,
    },
}

/// Verdict for the §H.4 fail-dark express-liveness oracle: at least `expected_min` known-good
/// candidates must have crossed the SIGN + ENVELOPE + would-submit terminal boundary, and zero
/// raw wire attempts/acks may have occurred. The raw-send check fires FIRST — a lane that both
/// darkened and leaked must surface the money violation, not the liveness one.
pub fn assert_express_terminal_boundary(
    before: TerminalBoundaryCounters,
    after: TerminalBoundaryCounters,
    expected_min: u64,
) -> Result<TerminalBoundaryProof, TerminalBoundaryFailure> {
    let attempts = after.wire_attempts.saturating_sub(before.wire_attempts);
    let acks = after.wire_acks.saturating_sub(before.wire_acks);
    if attempts != 0 || acks != 0 {
        return Err(TerminalBoundaryFailure::RawSendDetected { attempts, acks });
    }
    let signed = after.signed.saturating_sub(before.signed);
    let envelope_ready = after.envelope_ready.saturating_sub(before.envelope_ready);
    let would_submit = after
        .would_submit_decisions
        .saturating_sub(before.would_submit_decisions);
    if signed < expected_min || envelope_ready < expected_min || would_submit < expected_min {
        return Err(TerminalBoundaryFailure::FailDark {
            expected: expected_min,
            signed,
            envelope_ready,
            would_submit,
        });
    }
    Ok(TerminalBoundaryProof {
        reached_terminal: signed.min(envelope_ready).min(would_submit),
    })
}

/// Deterministic, platform-stable static hash of a route's structural identity: the ordered
/// (pool address, family, direction, fee-or-spacing) tuple per hop. FNV-1a/64 over a canonical
/// byte encoding — stable across runs, architectures, and serde versions, so golden keys never
/// drift with the route table's dense-id assignment.
pub fn route_static_hash(hops: &[([u8; 20], &str, u8, u32)]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    };
    for (address, family, direction, fee) in hops {
        for b in address {
            eat(*b);
        }
        for b in family.as_bytes() {
            eat(*b);
        }
        eat(*direction);
        for b in fee.to_be_bytes() {
            eat(b);
        }
        eat(b'|');
    }
    format!("0x{hash:016x}")
}
