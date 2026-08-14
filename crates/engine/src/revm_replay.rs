//! Bounded, observable building blocks for the REVM trigger-state replay path.
//!
//! These types fix three latent defects identified in `TAIL_LATENCY_PLAN.md` §1 (rows 10 & 12)
//! and provide the budget accounting for §2 A4:
//!
//! * [`BoundedTxDedup`] — replaces the unbounded `AHashSet<[u8;32]>` that
//!   `drain_revm_state_replay_queue` used to dedup replayed feed txs. That set grew monotonically
//!   for the engine's whole uptime (~77 MB/day at 100k tx/h, plus rehash latency spikes). This is
//!   a FIFO-bounded set: at most `capacity` hashes are remembered, oldest evicted first.
//!
//! * [`ReplayFailTracker`] — counts consecutive/total `replay_tx_commit` failures so a fork whose
//!   trigger replays keep reverting (a sign its pinned state has drifted) can be flagged *suspect*
//!   and surfaced, instead of silently validating later candidates against stale parent state.
//!
//! * [`DrainStats`] — the accounting a single drain pass returns, including the new `deferred` and
//!   `budget_hit` fields that make the A4 time-budget observable.
//!
//! All three are pure (no I/O, no allocation on the hot insert path beyond the bounded ring) and
//! exhaustively unit-tested below, so the hot-path drain in `pipeline.rs` is a thin wiring layer
//! over proven logic.

use std::collections::VecDeque;

use ahash::AHashSet;

/// A FIFO-bounded set of transaction hashes used to dedup trigger-state replays.
///
/// `insert` returns `true` the first time a hash is seen and `false` for a repeat (same contract
/// as `HashSet::insert`), but memory is capped: once `capacity` distinct hashes are held, inserting
/// a new one evicts the oldest. The eviction window (`capacity`) must comfortably exceed the most
/// in-flight replays that can be pending at once (bounded by the queue's `RING_CAPACITY`), so a
/// still-relevant hash is never evicted early — see the `does_not_evict_within_window` test.
#[derive(Debug)]
pub struct BoundedTxDedup {
    seen: AHashSet<[u8; 32]>,
    order: VecDeque<[u8; 32]>,
    capacity: usize,
}

impl BoundedTxDedup {
    /// Create a dedup set holding at most `capacity` hashes (clamped to ≥1).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            seen: AHashSet::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Record `hash`. Returns `true` if it was newly inserted, `false` if already present.
    /// On a new insert that would exceed `capacity`, the oldest remembered hash is evicted first.
    pub fn insert(&mut self, hash: [u8; 32]) -> bool {
        if self.seen.contains(&hash) {
            return false;
        }
        if self.order.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.order.push_back(hash);
        self.seen.insert(hash);
        true
    }

    /// True if `hash` is currently remembered.
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.seen.contains(hash)
    }

    /// Number of hashes currently remembered (≤ capacity).
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// True if no hashes are remembered.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The configured maximum number of remembered hashes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Forget everything. Called when the fork is repinned (a fresh pin starts a fresh replay
    /// history), keeping memory flat by construction.
    pub fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }
}

/// Tracks `replay_tx_commit` outcomes so a drifting fork can be flagged before it silently
/// mis-validates. A run of `suspect_threshold` consecutive failures (or, depending on the caller,
/// the failure of a candidate's *own* trigger tx) marks the fork suspect; the caller then forces an
/// early repin (once double-buffered repin lands) and/or vetoes affected candidates.
#[derive(Debug, Clone)]
pub struct ReplayFailTracker {
    consecutive: u32,
    total_failures: u64,
    total_successes: u64,
    suspect_threshold: u32,
}

impl ReplayFailTracker {
    /// New tracker that flags suspect after `suspect_threshold` consecutive failures (clamped ≥1).
    pub fn new(suspect_threshold: u32) -> Self {
        Self {
            consecutive: 0,
            total_failures: 0,
            total_successes: 0,
            suspect_threshold: suspect_threshold.max(1),
        }
    }

    /// Record a successful commit. Resets the consecutive-failure run.
    pub fn record_success(&mut self) {
        self.consecutive = 0;
        self.total_successes += 1;
    }

    /// Record a failed commit. Returns `true` if this failure crosses the suspect threshold.
    pub fn record_failure(&mut self) -> bool {
        self.consecutive += 1;
        self.total_failures += 1;
        self.is_suspect()
    }

    /// True if the current consecutive-failure run has reached the suspect threshold.
    pub fn is_suspect(&self) -> bool {
        self.consecutive >= self.suspect_threshold
    }

    /// Current consecutive-failure run length.
    pub fn consecutive(&self) -> u32 {
        self.consecutive
    }

    /// Cumulative failed commits since construction/reset.
    pub fn total_failures(&self) -> u64 {
        self.total_failures
    }

    /// Cumulative successful commits since construction/reset.
    pub fn total_successes(&self) -> u64 {
        self.total_successes
    }

    /// Reset the consecutive run (e.g. after a repin clears the suspect state). Cumulative
    /// totals are preserved for telemetry.
    pub fn reset_run(&mut self) {
        self.consecutive = 0;
    }
}

/// Bounded evidence for the first irreversible replay event in one drain pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayEventDiagnostic {
    /// Canonical outer feed sequence of the affected replay event.
    pub seq: u64,
    /// Canonical leaf ordinal within [`Self::seq`].
    pub inner_ordinal: u32,
    /// Signed transaction hash of the affected replay event.
    pub tx_hash: [u8; 32],
}

/// Accounting returned by one drain pass of the trigger-state replay queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainStats {
    /// Events moved from the lock-free queue into the ordered pending buffer this pass.
    pub drained: u64,
    /// Trigger txs successfully replayed+committed into the fork this pass.
    pub committed: u64,
    /// Trigger txs whose replay reverted/failed (fork left at parent state for those).
    pub failed: u64,
    /// Oldest pending events dropped because the pending buffer overflowed `RING_CAPACITY`.
    pub dropped_pending: u64,
    /// First pending event irreversibly dropped in this pass, if any.
    pub first_dropped_pending: Option<ReplayEventDiagnostic>,
    /// First event whose replay irreversibly failed in this pass, if any.
    pub first_failed_replay: Option<ReplayEventDiagnostic>,
    /// Eligible events left in `pending` because the per-candidate time budget was hit (A4).
    /// These are *deferred*, not dropped — the next pass (or idle drain) will apply them.
    pub deferred: u64,
    /// True if the time budget stopped this pass before all eligible events were applied.
    pub budget_hit: bool,
    /// Buffered future replays whose read-set was probe→batch WARMED during an idle pass
    /// (no commit — just cache hydration so the later eligible replay is zero-fetch).
    pub idle_prewarmed: u64,
    /// Lazy fetcher (Alchemy) round-trips paid INSIDE this drain pass — cold replay work
    /// on the validation thread. This was the dominant hidden `revm_ns` cost (measured
    /// ~331 ms per cold replay on AWS, 2026-07-04), previously invisible because the
    /// `val_rpc_cold` sample window opened only AFTER the drain. With the probe→batched
    /// prefetch it should approach 0.
    pub rpc_cold: u64,
}

impl DrainStats {
    /// True if anything happened worth logging.
    pub fn is_active(&self) -> bool {
        self.drained > 0
            || self.committed > 0
            || self.failed > 0
            || self.dropped_pending > 0
            || self.deferred > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn dedup_insert_reports_new_then_repeat() {
        let mut d = BoundedTxDedup::new(8);
        assert!(d.insert(h(1)), "first insert is new");
        assert!(!d.insert(h(1)), "second insert of same hash is a repeat");
        assert!(d.contains(&h(1)));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn dedup_evicts_oldest_when_over_capacity() {
        let mut d = BoundedTxDedup::new(3);
        for i in 0..3 {
            assert!(d.insert(h(i)));
        }
        assert_eq!(d.len(), 3);
        // Inserting a 4th evicts the oldest (hash 0).
        assert!(d.insert(h(3)));
        assert_eq!(d.len(), 3, "stays bounded at capacity");
        assert!(!d.contains(&h(0)), "oldest evicted");
        assert!(d.contains(&h(1)) && d.contains(&h(2)) && d.contains(&h(3)));
        // Hash 0 is now treated as new again (it was evicted) — acceptable: it can only be
        // re-replayed, never double-counted within the live window.
        assert!(d.insert(h(0)), "evicted hash is new again");
    }

    #[test]
    fn dedup_does_not_evict_within_window() {
        // A hash inserted stays remembered for at least `capacity-1` further distinct inserts.
        let cap = 16;
        let mut d = BoundedTxDedup::new(cap);
        assert!(d.insert(h(200)));
        for i in 0..(cap as u8 - 1) {
            d.insert(h(i));
        }
        assert!(d.contains(&h(200)), "still remembered within the window");
        // The very next distinct insert evicts it (it's now the oldest).
        d.insert(h(250));
        assert!(!d.contains(&h(200)));
    }

    #[test]
    fn dedup_repeat_does_not_consume_window() {
        // Re-inserting an existing hash must NOT push anything out (no order churn).
        let mut d = BoundedTxDedup::new(2);
        assert!(d.insert(h(1)));
        assert!(d.insert(h(2)));
        assert!(!d.insert(h(1)), "repeat");
        assert!(!d.insert(h(2)), "repeat");
        // Both still present; capacity not exceeded by repeats.
        assert!(d.contains(&h(1)) && d.contains(&h(2)));
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn dedup_clear_resets() {
        let mut d = BoundedTxDedup::new(4);
        d.insert(h(1));
        d.insert(h(2));
        assert_eq!(d.len(), 2);
        d.clear();
        assert!(d.is_empty());
        assert!(d.insert(h(1)), "after clear, previously-seen hash is new");
    }

    #[test]
    fn dedup_capacity_clamped_to_one() {
        let mut d = BoundedTxDedup::new(0);
        assert_eq!(d.capacity(), 1);
        assert!(d.insert(h(1)));
        assert!(d.insert(h(2))); // evicts h(1)
        assert!(!d.contains(&h(1)));
        assert!(d.contains(&h(2)));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn dedup_memory_stays_bounded_under_flood() {
        // The whole point: 1e5 distinct inserts never exceed capacity.
        let cap = 4096;
        let mut d = BoundedTxDedup::new(cap);
        for i in 0..100_000u32 {
            let mut hash = [0u8; 32];
            hash[..4].copy_from_slice(&i.to_be_bytes());
            d.insert(hash);
            assert!(d.len() <= cap);
        }
        assert_eq!(d.len(), cap);
    }

    #[test]
    fn fail_tracker_flags_after_threshold() {
        let mut t = ReplayFailTracker::new(3);
        assert!(!t.record_failure());
        assert!(!t.record_failure());
        assert!(
            t.record_failure(),
            "third consecutive failure trips suspect"
        );
        assert!(t.is_suspect());
        assert_eq!(t.consecutive(), 3);
        assert_eq!(t.total_failures(), 3);
    }

    #[test]
    fn fail_tracker_success_resets_run() {
        let mut t = ReplayFailTracker::new(2);
        t.record_failure();
        assert_eq!(t.consecutive(), 1);
        t.record_success();
        assert_eq!(t.consecutive(), 0, "success clears the run");
        assert!(!t.is_suspect());
        // Totals preserved.
        assert_eq!(t.total_failures(), 1);
        assert_eq!(t.total_successes(), 1);
        // Need a fresh run to re-trip.
        assert!(!t.record_failure());
        assert!(t.record_failure());
        assert!(t.is_suspect());
    }

    #[test]
    fn fail_tracker_reset_run_clears_suspect_keeps_totals() {
        let mut t = ReplayFailTracker::new(1);
        assert!(t.record_failure());
        assert!(t.is_suspect());
        t.reset_run();
        assert!(!t.is_suspect());
        assert_eq!(t.total_failures(), 1, "totals survive a run reset");
    }

    #[test]
    fn fail_tracker_threshold_clamped() {
        let mut t = ReplayFailTracker::new(0);
        assert!(t.record_failure(), "threshold 0 clamps to 1");
    }

    // A2 contract: a repin starts a fresh pin, so the replay history (dedup + consecutive-failure
    // run) must reset — keeping memory bounded and clearing a stale suspect flag.
    #[test]
    fn repin_resets_replay_history() {
        let mut dedup = BoundedTxDedup::new(64);
        let mut ft = ReplayFailTracker::new(2);
        dedup.insert([1; 32]);
        ft.record_failure();
        ft.record_failure();
        assert!(ft.is_suspect() && !dedup.is_empty(), "dirtied before repin");
        // Repin reset (mirrors revm_loop's idle-repin path).
        dedup.clear();
        ft.reset_run();
        assert!(dedup.is_empty(), "fresh pin → fresh dedup");
        assert!(!ft.is_suspect(), "fresh pin → suspect cleared");
        assert_eq!(
            ft.total_failures(),
            2,
            "cumulative telemetry preserved across repin"
        );
    }

    #[test]
    fn drain_stats_active_flag() {
        assert!(!DrainStats::default().is_active());
        assert!(DrainStats {
            committed: 1,
            ..Default::default()
        }
        .is_active());
        assert!(DrainStats {
            deferred: 2,
            ..Default::default()
        }
        .is_active());
    }
}
