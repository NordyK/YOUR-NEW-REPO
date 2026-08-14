//! Plan §6.1 — per-pool live telemetry that feeds the dynamic score.
//!
//! `PoolTelemetry` owns per-pool atomic counters keyed by `PoolId.0` (`u32`). The hot
//! path increments them with `Ordering::Relaxed` (cheap on aarch64 / x86_64) — they are
//! observation-only counters, no synchronisation guarantee required between writers.
//!
//! The background scorer reads them every §6.2 cadence (5–30 min) and feeds the EWMA
//! aggregates into the `PoolScoreInputs::historical_hit_rate`, `volume_ewma`, and
//! `revert_rate_penalty` slots.

use std::sync::atomic::{AtomicU64, Ordering};

/// Per-pool live telemetry. Sized at startup from the registered pool count; the inner
/// `Vec`s are never resized so concurrent `load`/`fetch_add` are safe without locking.
#[derive(Debug)]
pub struct PoolTelemetry {
    /// `dirty_count[pool_id]` — number of dirty-pool events observed for this pool.
    /// Bumped once per `dirty_producer.push` (which itself fires after a successful
    /// `classify_and_predict`).
    dirty: Vec<AtomicU64>,
    /// `candidate_count[pool_id]` — number of positive-EV candidates produced *touching*
    /// this pool. Multi-hop candidates bump every pool in the cycle.
    candidate: Vec<AtomicU64>,
    /// `submitted_count[pool_id]` — number of submissions through this pool.
    submitted: Vec<AtomicU64>,
    /// `reverted_count[pool_id]` — number of reverts observed on this pool's submissions.
    reverted: Vec<AtomicU64>,
    /// Initial on-chain volume seed (Q32.32). Used as the cold-start value of
    /// `volume_ewma` until live data accumulates.
    initial_volume_seed: Vec<u64>,
}

impl PoolTelemetry {
    /// New telemetry sized to `n_pools` with per-pool initial volume seeds.
    pub fn new(initial_volume_seeds: Vec<u64>) -> Self {
        let n = initial_volume_seeds.len();
        Self {
            dirty: (0..n).map(|_| AtomicU64::new(0)).collect(),
            candidate: (0..n).map(|_| AtomicU64::new(0)).collect(),
            submitted: (0..n).map(|_| AtomicU64::new(0)).collect(),
            reverted: (0..n).map(|_| AtomicU64::new(0)).collect(),
            initial_volume_seed: initial_volume_seeds,
        }
    }

    /// Number of tracked pools.
    pub fn len(&self) -> usize {
        self.dirty.len()
    }

    /// Whether telemetry has zero pools (boot mode).
    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }

    /// Record a dirty-pool event for pool `idx`. Silent no-op if `idx` is out of range.
    pub fn record_dirty(&self, idx: usize) {
        if let Some(c) = self.dirty.get(idx) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a candidate touching pool `idx`.
    pub fn record_candidate(&self, idx: usize) {
        if let Some(c) = self.candidate.get(idx) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a submission through pool `idx`.
    pub fn record_submitted(&self, idx: usize) {
        if let Some(c) = self.submitted.get(idx) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a revert on pool `idx`.
    pub fn record_reverted(&self, idx: usize) {
        if let Some(c) = self.reverted.get(idx) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Snapshot counters for pool `idx`. Returns `(dirty, candidate, submitted,
    /// reverted)`; ignores out-of-range indices (zeros).
    pub fn snapshot(&self, idx: usize) -> (u64, u64, u64, u64) {
        (
            self.dirty
                .get(idx)
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0),
            self.candidate
                .get(idx)
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0),
            self.submitted
                .get(idx)
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0),
            self.reverted
                .get(idx)
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0),
        )
    }

    /// Initial seed `volume_ewma` for pool `idx` (Q32.32). Used when historical EWMA
    /// has no observations yet.
    pub fn initial_volume(&self, idx: usize) -> u64 {
        self.initial_volume_seed
            .get(idx)
            .copied()
            .unwrap_or(1u64 << 32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_increment() {
        let t = PoolTelemetry::new(vec![1u64 << 32; 3]);
        t.record_dirty(0);
        t.record_dirty(0);
        t.record_candidate(1);
        t.record_submitted(2);
        t.record_reverted(2);
        assert_eq!(t.snapshot(0), (2, 0, 0, 0));
        assert_eq!(t.snapshot(1), (0, 1, 0, 0));
        assert_eq!(t.snapshot(2), (0, 0, 1, 1));
    }

    #[test]
    fn out_of_range_is_silent() {
        let t = PoolTelemetry::new(vec![1u64 << 32; 2]);
        t.record_dirty(99); // no panic
        assert_eq!(t.snapshot(99), (0, 0, 0, 0));
    }
}
