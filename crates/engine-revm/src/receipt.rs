//! `LocalReceiptCache` (§10.10): bounded, lock-free indexed by candidate id.

use std::collections::VecDeque;

use alloy_primitives::U256;

use engine_state::{Generation, RouteId};

/// Local receipt entry.
#[derive(Debug, Clone)]
pub struct LocalReceipt {
    /// Per-process candidate id.
    pub candidate_id: u64,
    /// Route id.
    pub route_id: RouteId,
    /// Generations seen for every touched pool.
    pub state_generations: Vec<Generation>,
    /// REVM-reported gas.
    pub gas_used: u64,
    /// Base-token balance delta.
    pub output_balances: U256,
    /// Footprint hash from the execution.
    pub footprint_hash: [u8; 32],
    /// Monotonic timestamp.
    pub validated_at_ns: u64,
}

/// Bounded receipt ring.
pub struct LocalReceiptCache {
    capacity: usize,
    inner: VecDeque<LocalReceipt>,
}

impl LocalReceiptCache {
    /// New with capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            inner: VecDeque::with_capacity(capacity),
        }
    }

    /// Insert a receipt. Drops the oldest if full.
    pub fn push(&mut self, r: LocalReceipt) {
        if self.inner.len() == self.capacity {
            self.inner.pop_front();
        }
        self.inner.push_back(r);
    }

    /// Look up by candidate id.
    pub fn find(&self, candidate_id: u64) -> Option<&LocalReceipt> {
        self.inner.iter().find(|r| r.candidate_id == candidate_id)
    }

    /// Iterate entries (control-plane only).
    pub fn iter(&self) -> impl Iterator<Item = &LocalReceipt> {
        self.inner.iter()
    }

    /// Drop entries older than `cutoff_ns`.
    pub fn evict_older_than(&mut self, cutoff_ns: u64) {
        while let Some(front) = self.inner.front() {
            if front.validated_at_ns < cutoff_ns {
                self.inner.pop_front();
            } else {
                break;
            }
        }
    }

    /// Current length.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: u64, ts: u64) -> LocalReceipt {
        LocalReceipt {
            candidate_id: id,
            route_id: RouteId(0),
            state_generations: vec![],
            gas_used: 0,
            output_balances: U256::ZERO,
            footprint_hash: [0u8; 32],
            validated_at_ns: ts,
        }
    }

    #[test]
    fn push_and_find() {
        let mut c = LocalReceiptCache::new(4);
        c.push(r(1, 100));
        c.push(r(2, 200));
        assert!(c.find(2).is_some());
        assert!(c.find(99).is_none());
    }

    #[test]
    fn drops_oldest_when_full() {
        let mut c = LocalReceiptCache::new(2);
        c.push(r(1, 1));
        c.push(r(2, 2));
        c.push(r(3, 3));
        assert!(c.find(1).is_none());
        assert!(c.find(2).is_some());
        assert!(c.find(3).is_some());
    }

    #[test]
    fn eviction_by_timestamp() {
        let mut c = LocalReceiptCache::new(8);
        c.push(r(1, 100));
        c.push(r(2, 200));
        c.push(r(3, 300));
        c.evict_older_than(250);
        assert!(c.find(1).is_none());
        assert!(c.find(2).is_none());
        assert!(c.find(3).is_some());
    }
}
