//! Cache-padded atomic counter array. Per-thread instances avoid cross-thread contention.

use core::sync::atomic::{AtomicU64, Ordering};

use crossbeam_utils::CachePadded;

/// Single cache-padded counter.
pub struct Counter(CachePadded<AtomicU64>);

impl Counter {
    /// New, zero-initialised.
    pub const fn new() -> Self {
        Self(CachePadded::new(AtomicU64::new(0)))
    }

    /// Increment by `n`.
    #[inline]
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    /// Read value.
    #[inline]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Reset to zero.
    pub fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed-size counter array.
pub struct CountersArray<const N: usize> {
    inner: [Counter; N],
}

impl<const N: usize> CountersArray<N> {
    /// New.
    pub fn new() -> Self {
        Self {
            inner: std::array::from_fn(|_| Counter::new()),
        }
    }

    /// Increment counter `i` by `n`.
    #[inline]
    pub fn add(&self, i: usize, n: u64) {
        self.inner[i].add(n);
    }

    /// Read counter `i`.
    #[inline]
    pub fn get(&self, i: usize) -> u64 {
        self.inner[i].get()
    }

    /// Sum across the array.
    pub fn sum(&self) -> u64 {
        self.inner.iter().map(|c| c.get()).sum()
    }
}

impl<const N: usize> Default for CountersArray<N> {
    fn default() -> Self {
        Self::new()
    }
}
