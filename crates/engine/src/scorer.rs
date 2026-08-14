//! Plan §6.2 — background pool scorer.
//!
//! Runs on a dedicated background thread (`name = "scorer"`). Wakes every
//! `cfg.discovery.score_interval_secs` (5–30 min per plan), reads per-pool telemetry
//! (`PoolTelemetry`), recomputes `pool_score` via the §6.1 formula, and emits an INFO
//! log line per recompute with the top-N ranking. **It does NOT swap the live route
//! table yet** — atomic tier swap is the next step; this build delivers the cadence and
//! the formula running on live telemetry so the operator can see what re-election
//! would do.
//!
//! Telemetry signals fed in:
//! - `volume_ewma` — currently the on-chain seed value (`PoolTelemetry::initial_volume`);
//!   live EWMA-from-flow refinement comes in the follow-up that wires `Swap` event
//!   listening.
//! - `historical_hit_rate` — `candidate_count / dirty_count` clamped to ≤ 1.0.
//! - `revert_rate_penalty` — `reverted_count / submitted_count` (zero today; non-zero
//!   once the executor is deployed and we observe real reverts).
//! All other inputs stay at the unit baseline; this is the cold-start regime.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use engine_discovery::{score_pool, PoolScoreInputs, PoolTelemetry};

use crate::pipeline::DetectionContext;

/// Launch the background scorer. Returns the `JoinHandle` so the engine can join on
/// shutdown.
pub fn spawn_scorer(
    detection: Arc<DetectionContext>,
    interval_secs: u64,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<thread::JoinHandle<()>> {
    let interval = Duration::from_secs(interval_secs.max(60));
    thread::Builder::new()
        .name("engine-scorer".into())
        .spawn(move || {
            // Sleep in small steps so we react to shutdown promptly.
            let step = Duration::from_millis(500);
            let mut last_run = Instant::now()
                .checked_sub(interval)
                .unwrap_or_else(Instant::now);
            while !shutdown.load(Ordering::Acquire) {
                if last_run.elapsed() >= interval {
                    recompute_and_log(&detection);
                    last_run = Instant::now();
                }
                thread::sleep(step);
            }
        })
}

fn recompute_and_log(detection: &DetectionContext) {
    let n = detection.universe.pools.len();
    if n == 0 {
        warn!("scorer: no pools registered; skipping");
        return;
    }
    let telemetry: &PoolTelemetry = &detection.telemetry;
    let mut scores: Vec<(u32, u64, ScoreBreakdown)> = Vec::with_capacity(n);
    for idx in 0..n {
        let (dirty, candidate, submitted, reverted) = telemetry.snapshot(idx);
        let mut inputs = PoolScoreInputs::unit_baseline();
        inputs.volume_ewma = telemetry.initial_volume(idx);
        // historical_hit_rate = candidates / dirty, clamped to [0, 1].
        inputs.historical_hit_rate = if dirty > 0 {
            // candidate may legitimately equal dirty (every dirty produced a candidate).
            // Cap at 1.0 = 1<<32. Compute in u128 to avoid overflow on large dirty counts.
            let r = ((candidate as u128) << 32) / dirty as u128;
            r.min(1u128 << 32) as u64
        } else {
            1u64 << 32 // no observations yet — unit
        };
        // revert_rate_penalty = reverted / submitted (Q32.32).
        inputs.revert_rate_penalty = if submitted > 0 {
            let r = ((reverted as u128) << 32) / submitted as u128;
            r.min(1u128 << 32) as u64
        } else {
            0
        };
        let out = score_pool(&inputs);
        scores.push((
            idx as u32,
            out.pool_score,
            ScoreBreakdown {
                dirty,
                candidate,
                submitted,
                reverted,
                volume: inputs.volume_ewma,
                hit_rate: inputs.historical_hit_rate,
                revert_rate: inputs.revert_rate_penalty,
            },
        ));
    }
    // Sort by score, log top 8.
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    let top = scores.iter().take(8);
    info!(pool_count = n, "§6.2 background score recompute");
    for (i, (pool_id, score, b)) in top.enumerate() {
        let meta = detection
            .universe
            .pools
            .meta(engine_state::PoolId(*pool_id));
        info!(
            rank = i + 1,
            pool_id,
            pool = ?meta.address,
            family = ?meta.kind,
            score,
            dirty = b.dirty,
            candidate = b.candidate,
            submitted = b.submitted,
            reverted = b.reverted,
            volume_seed = b.volume,
            hit_rate_q32 = b.hit_rate,
            revert_rate_q32 = b.revert_rate,
            "score row"
        );
    }
}

struct ScoreBreakdown {
    dirty: u64,
    candidate: u64,
    submitted: u64,
    reverted: u64,
    volume: u64,
    hit_rate: u64,
    revert_rate: u64,
}
