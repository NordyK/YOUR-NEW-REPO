//! Per-thread rejection-reason ring. Hot threads write a `u8` reason code; background
//! drains and aggregates.

use core::sync::atomic::{AtomicU64, Ordering};

use crossbeam_utils::CachePadded;

/// Number of distinct reason codes the engine emits (room for growth).
pub const REJECTION_REASON_COUNT: usize = 64;

/// Bounded counter array indexed by reason code.
pub struct ReasonRing {
    inner: [CachePadded<AtomicU64>; REJECTION_REASON_COUNT],
}

impl ReasonRing {
    /// New.
    pub fn new() -> Self {
        Self {
            inner: std::array::from_fn(|_| CachePadded::new(AtomicU64::new(0))),
        }
    }

    /// Record a rejection with a reason code.
    #[inline]
    pub fn record(&self, reason_code: u8) {
        let idx = (reason_code as usize) & (REJECTION_REASON_COUNT - 1);
        self.inner[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot the count for a code.
    pub fn count(&self, reason_code: u8) -> u64 {
        let idx = (reason_code as usize) & (REJECTION_REASON_COUNT - 1);
        self.inner[idx].load(Ordering::Relaxed)
    }

    /// Drain into a vector of `(code, count)` for export.
    pub fn drain(&self) -> Vec<(u8, u64)> {
        let mut out = Vec::with_capacity(REJECTION_REASON_COUNT);
        for (i, c) in self.inner.iter().enumerate() {
            let v = c.swap(0, Ordering::AcqRel);
            if v != 0 {
                out.push((i as u8, v));
            }
        }
        out
    }
}

impl Default for ReasonRing {
    fn default() -> Self {
        Self::new()
    }
}
