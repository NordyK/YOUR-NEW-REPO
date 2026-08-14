//! T3.1 — codified latency budgets + a machine-independent perf-regression gate.
//!
//! Mantis has plenty of criterion / `#[ignore]` microbenches but, before this module, no *codified
//! target* and no *regression gate*: the 2–2.85× kernel speedups (and the T2.1/T2.2/T0.2 wins this
//! session) could silently regress and nothing would fail.
//!
//! Two artifacts live here:
//!   1. [`STAGE_BUDGETS`] — per-stage p50/p99 SLOs (aspirational target + the current measured
//!      baseline for reference). A *documentation* contract, checked for well-formedness.
//!   2. [`SPEEDUP_FLOORS`] — the proven optimization ratios as **regression floors**. The perf-gate
//!      ([`tests::perf_gate_speedup_ratios_hold`], release-only) re-measures the ratios live and
//!      fails if any drops below its floor.
//!
//! Why ratios, not absolute ns: absolute timings are machine-dependent (laptop dev vs Graviton4)
//! and noisy (measured sim p50 swung 1.34↔2.52 ms on identical code). A *speedup ratio*
//! (cached vs uncached, memo vs recompute) is machine-independent and low-variance, so it gates the
//! thing that actually matters — "the optimization still works" — without flaking on noise.

/// A per-pipeline-stage latency budget. `target_*` are the SLOs we design toward; `measured_*` is
/// the most recent laptop baseline, recorded so a reader sees the gap without a live run.
#[derive(Debug, Clone, Copy)]
pub struct StageBudget {
    /// Stage name (matches the heartbeat/attribution counters).
    pub stage: &'static str,
    /// Aspirational p50 budget (µs).
    pub target_p50_us: f64,
    /// Aspirational p99 budget (µs).
    pub target_p99_us: f64,
    /// Most recent measured p50 (µs) on the laptop — `0.0` if not yet measured here.
    pub measured_p50_us: f64,
    /// Provenance / caveat.
    pub note: &'static str,
}

/// Per-stage budgets. The `sim` target is the user's Tigger-class 500 µs goal; the measured p50
/// (≈1.46 ms) is over it — the gap is intentional and market/`MAX_HOPS`-dependent (see the
/// simulation-speedup analyses), so `sim` is a *target*, not a gate.
///
/// NOMENCLATURE (network review 2026-07-13, §Priority 10): every budget here is a **local same-host
/// stage** latency (CLOCK_MONOTONIC compute/queue time), NOT a network one-way. The `submit` budget
/// is local build+sign+kernel-write; the network submit leg headline (`~347 µs`) is a **half
/// application-RTT proxy** (RTT/2), not an observed one-way — see `run_manifest` (`submit_path_metric
/// = half_application_rtt_proxy`) and `seq_latency_monitor` (`metric = half_rtt_proxy`). Do not read
/// any number below as a sequencer→host one-way.
pub const STAGE_BUDGETS: &[StageBudget] = &[
    StageBudget {
        stage: "parse",
        target_p50_us: 5.0,
        target_p99_us: 10.0,
        measured_p50_us: 0.0,
        note: "feed rx → decoded event; Tigger budget. AWS-measured.",
    },
    StageBudget {
        stage: "sim",
        target_p50_us: 500.0,
        target_p99_us: 1_500.0,
        measured_p50_us: 1_460.0,
        note: "rank+size+eval per dirty event. p50 is MAX_HOPS/market-dependent; 500µs aspirational.",
    },
    StageBudget {
        stage: "revm",
        target_p50_us: 350.0,
        target_p99_us: 1_550.0,
        measured_p50_us: 0.0,
        note: "REVM validation; T0.2 state-mirror demotes per-slot RPC (~30ms) to a 76ns hit. AWS-measured.",
    },
    StageBudget {
        stage: "submit",
        target_p50_us: 100.0,
        target_p99_us: 300.0,
        measured_p50_us: 0.0,
        note: "build+sign+broadcast. AWS-measured.",
    },
    StageBudget {
        stage: "e2e",
        target_p50_us: 1_000.0,
        target_p99_us: 4_000.0,
        measured_p50_us: 0.0,
        note: "event → broadcast end-to-end SLO. AWS-measured.",
    },
];

/// A proven optimization expressed as a regression floor. `measured_x100` is what we observed when
/// it shipped (×100, so 163 = 1.63×); `min_ratio_x100` is the floor below which the perf-gate fails
/// — set well under the measured value so normal noise never trips it, only a real regression.
#[derive(Debug, Clone, Copy)]
pub struct SpeedupFloor {
    /// Optimization name.
    pub name: &'static str,
    /// Floor ratio (×100). The live ratio must stay ≥ this.
    pub min_ratio_x100: u32,
    /// Ratio measured when the optimization shipped (×100).
    pub measured_x100: u32,
    /// What is being compared.
    pub note: &'static str,
}

/// The session's proven speedups, as regression floors. Floors sit ~20–60% below the measured
/// ratio so the gate catches a real loss (e.g. a kernel reverting to its slow path) not noise.
pub const SPEEDUP_FLOORS: &[SpeedupFloor] = &[
    SpeedupFloor {
        name: "curve_d_cache",
        min_ratio_x100: 130,
        measured_x100: 163,
        note: "T2.1: get_dy vs get_dy_cached (skips the size-independent Newton D solve).",
    },
    SpeedupFloor {
        name: "lb_bin_price_memo",
        min_ratio_x100: 1_000,
        measured_x100: 3_850,
        note: "T2.2: thread-local memo of get_price_from_id vs recomputing the pow each call.",
    },
];

// ─── WI-01 campaign-baseline budgets (codex plan §10.2/§10.3) ────────────────────────────────
//
// Enforcement history: the WI-01 RED commit landed the functions below as PERMISSIVE stubs and
// proved via `tests/campaign_baseline_oracle_static.rs` that each budget test fails while
// enforcement is absent; this GREEN state is the real contract behind the identical API.

/// `RouteAuthorityCell` is exactly 64 bytes (§10.2) — one cache line.
pub const ROUTE_AUTHORITY_CELL_BYTES: u64 = 64;

/// The route-authority table may never exceed 512 MiB (§10.2)...
pub const ROUTE_AUTHORITY_TABLE_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// ...nor 5% of the declared process memory budget (§10.2). Stored ×100 to stay integer.
pub const ROUTE_AUTHORITY_TABLE_MAX_PROCESS_PCT_X100: u64 = 500;

/// Dirty-route publication budget (§10.3): base ns per feed event...
pub const STATE_COMMIT_BASE_BUDGET_NS: u64 = 500;

/// ...plus per-unique-dirty-route ns (p99).
pub const STATE_COMMIT_PER_DIRTY_ROUTE_NS: u64 = 60;

/// A p999 acceptance claim requires at least this many candidate-correlated observations,
/// aggregated across interleaved ≤1h runs (§10.3 / WI-01 verification gate).
pub const P999_MIN_CANDIDATE_CORRELATED_SAMPLES: u64 = 1_000_000;

/// Interleaved baseline/candidate repetitions required by the measurement protocol (§10.3).
pub const AB_INTERLEAVED_REPETITIONS: u64 = 30;

/// §10.3 FIXED regression limits: full-loop p50 ≤ +1%, p99 ≤ +3%, p999 upper 95% CI ≤ +5%.
/// A phase-acceptance run declaring weaker thresholds is invalid by construction.
pub const P50_MAX_REGRESSION_PCT: f64 = 1.0;
/// See [`P50_MAX_REGRESSION_PCT`].
pub const P99_MAX_REGRESSION_PCT: f64 = 3.0;
/// See [`P50_MAX_REGRESSION_PCT`].
pub const P999_UPPER_CI_MAX_REGRESSION_PCT: f64 = 5.0;

/// Hot ring payload stride may grow by at most this many net bytes across the whole campaign
/// (§10.2), measured against the WI-01 frozen layout baseline.
pub const RING_STRIDE_NET_INCREASE_CAP_BYTES: u64 = 16;

/// Machine-readable layout of one hot ring payload type (size/alignment freeze).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HotPayloadLayout {
    /// Type name (e.g. `Candidate`).
    pub type_name: String,
    /// `size_of` in bytes.
    pub size_bytes: u64,
    /// `align_of` in bytes.
    pub align_bytes: u64,
}

/// Budget violations raised by the WI-01 budget validators.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BudgetViolation {
    /// The route-authority cell is not exactly 64 bytes.
    #[error("route-authority cell must be exactly {expected} bytes, got {actual}")]
    CellSize {
        /// Required size.
        expected: u64,
        /// Declared size.
        actual: u64,
    },
    /// The route-authority table exceeds an absolute or relative memory cap.
    #[error("route-authority table {table_bytes} B exceeds cap {cap_bytes} B ({detail})")]
    TableOverBudget {
        /// Computed table size.
        table_bytes: u64,
        /// The violated cap.
        cap_bytes: u64,
        /// Which cap (absolute / process-fraction).
        detail: String,
    },
    /// A layout baseline row is missing or drifted.
    #[error("hot payload layout violation: {detail}")]
    Layout {
        /// What drifted.
        detail: String,
    },
    /// A latency-gate artifact row is unusable for gating.
    #[error("latency gate row {row} rejected: {detail}")]
    LatencyRow {
        /// Row index.
        row: usize,
        /// Why it cannot gate.
        detail: String,
    },
    /// The state-commit budget is absent or inconsistent with the frozen fanout.
    #[error("state-commit budget violation: {detail}")]
    StateCommit {
        /// What is missing/inconsistent.
        detail: String,
    },
}

/// Accepted route-authority table budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteAuthorityTableBudget {
    /// Total table bytes (`route_count × cell_bytes`).
    pub table_bytes: u64,
}

/// Enforce the §10.2 route-authority table budget for a sealed profile: the cell must be
/// exactly [`ROUTE_AUTHORITY_CELL_BYTES`], and `route_count × cell` must stay within BOTH the
/// absolute 512 MiB cap and 5% of `process_budget_bytes`.
pub fn route_authority_table_budget(
    route_count: u64,
    cell_bytes: u64,
    process_budget_bytes: u64,
) -> Result<RouteAuthorityTableBudget, BudgetViolation> {
    if cell_bytes != ROUTE_AUTHORITY_CELL_BYTES {
        return Err(BudgetViolation::CellSize {
            expected: ROUTE_AUTHORITY_CELL_BYTES,
            actual: cell_bytes,
        });
    }
    let table_bytes = route_count.saturating_mul(cell_bytes);
    if table_bytes > ROUTE_AUTHORITY_TABLE_MAX_BYTES {
        return Err(BudgetViolation::TableOverBudget {
            table_bytes,
            cap_bytes: ROUTE_AUTHORITY_TABLE_MAX_BYTES,
            detail: "absolute 512 MiB cap".into(),
        });
    }
    // table ≤ 5% of the declared process budget: table × 10_000 ≤ budget × PCT_X100 (u128 —
    // no overflow for any u64 inputs).
    let lhs = u128::from(table_bytes) * 10_000;
    let rhs = u128::from(process_budget_bytes) * u128::from(ROUTE_AUTHORITY_TABLE_MAX_PROCESS_PCT_X100);
    if lhs > rhs {
        return Err(BudgetViolation::TableOverBudget {
            table_bytes,
            cap_bytes: (rhs / 10_000) as u64,
            detail: format!(
                "5% of the declared process budget ({process_budget_bytes} B)"
            ),
        });
    }
    Ok(RouteAuthorityTableBudget { table_bytes })
}

/// The absolute p99 state-commit budget for one feed event dirtying `unique_dirty_route_count`
/// routes (§10.3): `500 ns + 60 ns × count`.
pub fn state_commit_budget_ns(unique_dirty_route_count: u64) -> u64 {
    STATE_COMMIT_BASE_BUDGET_NS
        .saturating_add(unique_dirty_route_count.saturating_mul(STATE_COMMIT_PER_DIRTY_ROUTE_NS))
}

/// Enforce that the frozen maximum-degree dirty fanout has an absolute state-commit budget that
/// fits inside the frozen RX→state-ready budget.
pub fn validate_state_commit_budget(
    max_unique_dirty_route_count: u64,
    frozen_rx_to_state_ready_budget_ns: u64,
) -> Result<(), BudgetViolation> {
    let budget = state_commit_budget_ns(max_unique_dirty_route_count);
    if budget > frozen_rx_to_state_ready_budget_ns {
        return Err(BudgetViolation::StateCommit {
            detail: format!(
                "max-degree event budget {budget} ns (degree {max_unique_dirty_route_count}) \
                 exceeds the frozen RX→state-ready budget {frozen_rx_to_state_ready_budget_ns} ns"
            ),
        });
    }
    Ok(())
}

/// Enforce the WI-01 hot-payload layout freeze: every baseline row must match the live layout
/// exactly, every live payload must be in the baseline, and the baseline itself must be
/// well-formed (positive power-of-two alignment, non-zero size).
pub fn validate_hot_payload_layout(
    baseline: &[HotPayloadLayout],
    actual: &[HotPayloadLayout],
) -> Result<(), BudgetViolation> {
    for row in baseline {
        if row.size_bytes == 0 || row.align_bytes == 0 || !row.align_bytes.is_power_of_two() {
            return Err(BudgetViolation::Layout {
                detail: format!(
                    "baseline row `{}` malformed: size {} align {}",
                    row.type_name, row.size_bytes, row.align_bytes
                ),
            });
        }
    }
    for live in actual {
        let frozen = baseline
            .iter()
            .find(|b| b.type_name == live.type_name)
            .ok_or_else(|| BudgetViolation::Layout {
                detail: format!(
                    "live hot payload `{}` has no frozen baseline row (silent new ring payload)",
                    live.type_name
                ),
            })?;
        if frozen.size_bytes != live.size_bytes || frozen.align_bytes != live.align_bytes {
            return Err(BudgetViolation::Layout {
                detail: format!(
                    "`{}` drifted: frozen {}B/align{} vs live {}B/align{}",
                    live.type_name,
                    frozen.size_bytes,
                    frozen.align_bytes,
                    live.size_bytes,
                    live.align_bytes
                ),
            });
        }
    }
    for frozen in baseline {
        if !actual.iter().any(|l| l.type_name == frozen.type_name) {
            return Err(BudgetViolation::Layout {
                detail: format!(
                    "frozen payload `{}` no longer reported by the live layout plumbing",
                    frozen.type_name
                ),
            });
        }
    }
    Ok(())
}

/// Summary of an accepted latency-gate artifact.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LatencyGateSummary {
    /// Accepted candidate-correlated rows.
    pub rows: u64,
    /// Rows carrying an admissible p999 claim (≥ 1M candidate-correlated samples).
    pub p999_rows: u64,
}

/// Validate latency-gate artifact rows: every row must be candidate-correlated (a non-empty
/// correlation key), carry a stage and a sample count, and any row claiming a p999 must carry
/// at least [`P999_MIN_CANDIDATE_CORRELATED_SAMPLES`] samples. Uncorrelated rows and
/// under-sampled tail claims are ERRORS — they must never gate a phase.
pub fn validate_latency_gate_rows(
    rows: &[serde_json::Value],
) -> Result<LatencyGateSummary, BudgetViolation> {
    let mut summary = LatencyGateSummary::default();
    for (row, value) in rows.iter().enumerate() {
        let stage = value.get("stage").and_then(serde_json::Value::as_str);
        if stage.is_none_or(str::is_empty) {
            return Err(BudgetViolation::LatencyRow {
                row,
                detail: "missing stage".into(),
            });
        }
        if value.get("candidate_correlated").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(BudgetViolation::LatencyRow {
                row,
                detail: "row is not candidate-correlated — sums of unrelated percentiles \
                         must never gate a phase"
                    .into(),
            });
        }
        let key = value.get("correlation_key").and_then(serde_json::Value::as_str);
        if key.is_none_or(str::is_empty) {
            return Err(BudgetViolation::LatencyRow {
                row,
                detail: "missing correlation_key".into(),
            });
        }
        let samples = value
            .get("samples")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| BudgetViolation::LatencyRow {
                row,
                detail: "missing samples count".into(),
            })?;
        if samples == 0 {
            return Err(BudgetViolation::LatencyRow {
                row,
                detail: "zero samples".into(),
            });
        }
        if value.get("p999_ns").is_some() {
            if samples < P999_MIN_CANDIDATE_CORRELATED_SAMPLES {
                return Err(BudgetViolation::LatencyRow {
                    row,
                    detail: format!(
                        "p999 claim with {samples} samples — a p999 acceptance claim needs \
                         at least {P999_MIN_CANDIDATE_CORRELATED_SAMPLES} candidate-correlated \
                         observations aggregated across ≤1h runs"
                    ),
                });
            }
            summary.p999_rows += 1;
        }
        summary.rows += 1;
    }
    Ok(summary)
}

/// Metadata contract for a §10.3 PHASE-ACCEPTANCE latency run (the gate later phases must
/// pass — distinct from row admissibility): release profile, pinned cores, at least
/// [`AB_INTERLEAVED_REPETITIONS`] interleaved baseline/candidate repetitions with paired
/// samples, and the explicit p50/p99/p999 regression thresholds. Anything else is a protocol
/// violation and must never gate a phase.
pub fn validate_phase_acceptance_run(metadata: &serde_json::Value) -> Result<(), BudgetViolation> {
    let fail = |detail: String| BudgetViolation::StateCommit { detail };
    if metadata.get("build_profile").and_then(serde_json::Value::as_str) != Some("release") {
        return Err(fail("phase acceptance requires build_profile=release".into()));
    }
    if metadata.get("pinned_cores").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(fail("phase acceptance requires pinned_cores=true".into()));
    }
    let reps = metadata
        .get("interleaved_repetitions")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if reps < AB_INTERLEAVED_REPETITIONS {
        return Err(fail(format!(
            "phase acceptance requires >= {AB_INTERLEAVED_REPETITIONS} interleaved A/B              repetitions, got {reps}"
        )));
    }
    if metadata
        .get("paired_baseline_candidate")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(fail(
            "phase acceptance compares candidate-correlated PAIRED rows, never sums of              unrelated percentiles"
                .into(),
        ));
    }
    // WI-01c Gap 4: the thresholds are the FIXED §10.3 limits — a declared threshold weaker
    // than +1%/+3%/+5% (or non-positive) invalidates the run outright.
    for (threshold, max) in [
        ("p50_threshold_pct", P50_MAX_REGRESSION_PCT),
        ("p99_threshold_pct", P99_MAX_REGRESSION_PCT),
        ("p999_upper_ci_threshold_pct", P999_UPPER_CI_MAX_REGRESSION_PCT),
    ] {
        let Some(value) = metadata.get(threshold).and_then(serde_json::Value::as_f64) else {
            return Err(fail(format!(
                "phase acceptance requires an explicit `{threshold}`"
            )));
        };
        if !(value > 0.0 && value <= max) {
            return Err(fail(format!(
                "`{threshold}`={value} violates the fixed §10.3 limit (0 < t <= {max})"
            )));
        }
    }
    Ok(())
}

/// Look up a stage budget by name.
pub fn stage_budget(stage: &str) -> Option<&'static StageBudget> {
    STAGE_BUDGETS.iter().find(|b| b.stage == stage)
}

/// Look up a speedup floor by name.
pub fn speedup_floor(name: &str) -> Option<&'static SpeedupFloor> {
    SPEEDUP_FLOORS.iter().find(|f| f.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Always-on structural gate (non-flaky): the codified artifact must be well-formed. ---

    #[test]
    fn stage_budgets_are_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for b in STAGE_BUDGETS {
            assert!(seen.insert(b.stage), "duplicate stage {}", b.stage);
            assert!(b.target_p50_us > 0.0, "{} p50 must be > 0", b.stage);
            assert!(
                b.target_p99_us >= b.target_p50_us,
                "{} p99 {} < p50 {}",
                b.stage,
                b.target_p99_us,
                b.target_p50_us
            );
            assert!(b.measured_p50_us >= 0.0);
        }
        // The stages we attribute the pipeline by must all be present.
        for required in ["parse", "sim", "revm", "submit", "e2e"] {
            assert!(
                stage_budget(required).is_some(),
                "missing budget for {required}"
            );
        }
    }

    #[test]
    fn speedup_floors_are_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for f in SPEEDUP_FLOORS {
            assert!(seen.insert(f.name), "duplicate floor {}", f.name);
            assert!(f.min_ratio_x100 > 100, "{} floor must be > 1.0×", f.name);
            assert!(
                f.measured_x100 >= f.min_ratio_x100,
                "{} measured {} below its own floor {}",
                f.name,
                f.measured_x100,
                f.min_ratio_x100
            );
        }
    }

    // --- Release-only perf-gate (machine-independent ratios): run in CI / on Graviton4 via ---
    // `cargo test -p engine --release latency_budgets::tests::perf_gate_speedup_ratios_hold -- --ignored --nocapture`
    // Marked #[ignore] so the flaky-in-debug timing never runs in the default `cargo test`.

    /// Median of `iters` timings of `f`, in nanoseconds (robust to the odd scheduler hiccup).
    fn median_ns(iters: usize, inner: u64, mut f: impl FnMut() -> u64) -> f64 {
        use std::time::Instant;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let t = Instant::now();
            std::hint::black_box(f());
            samples.push(t.elapsed().as_nanos() as f64 / inner as f64);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        samples[iters / 2]
    }

    #[test]
    #[ignore]
    fn perf_gate_speedup_ratios_hold() {
        use engine_amm::curve::{
            get_dy, get_dy_cached, precompute_d, StableSwapState, A_PRECISION,
        };

        // --- curve_d_cache: get_dy (recomputes D) vs get_dy_cached (reuses D). ---
        let state = StableSwapState {
            n_coins: 2,
            balances: [
                75_156_140_434_690_947_953_802,
                43_034_902_322,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            rates: [1_000_000_000_000_000_000, 10u128.pow(30), 0, 0, 0, 0, 0, 0],
            a_param: 1_000 * A_PRECISION,
            fee: 1_000_000,
            offpeg_fee_multiplier: 0,
        };
        let d = precompute_d(&state).unwrap();
        const INNER: u64 = 200;
        let base = median_ns(101, INNER, || {
            let mut acc = 0u128;
            for k in 0..INNER {
                acc ^= get_dy(&state, 0, 1, 1_000_000_000_000_000_000 + k as u128).unwrap_or(0);
            }
            acc as u64
        });
        let cached = median_ns(101, INNER, || {
            let mut acc = 0u128;
            for k in 0..INNER {
                acc ^= get_dy_cached(&state, 0, 1, 1_000_000_000_000_000_000 + k as u128, d)
                    .unwrap_or(0);
            }
            acc as u64
        });
        let ratio_x100 = (base / cached * 100.0) as u32;
        let floor = speedup_floor("curve_d_cache").unwrap();
        println!(
            "curve_d_cache: base {base:.0} ns/q, cached {cached:.0} ns/q => {:.2}× (floor {:.2}×, shipped {:.2}×)",
            base / cached,
            floor.min_ratio_x100 as f64 / 100.0,
            floor.measured_x100 as f64 / 100.0,
        );
        assert!(
            ratio_x100 >= floor.min_ratio_x100,
            "PERF REGRESSION: curve_d_cache {:.2}× < floor {:.2}× — the D-cache stopped helping",
            ratio_x100 as f64 / 100.0,
            floor.min_ratio_x100 as f64 / 100.0,
        );
    }
}
