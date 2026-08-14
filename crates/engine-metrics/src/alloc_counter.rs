//! Hot-path zero-allocation gate per §18.7.
//!
//! The engine installs `CountingAllocator` as the global allocator when built with
//! `--features alloc_counter`. Outside the feature flag the counter is a no-op and the
//! global allocator is `mimalloc`.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};

/// Per-thread allocation counter scratch.
pub struct CountingAllocator<A: GlobalAlloc> {
    inner: A,
    enabled: AtomicU64,
    allocs: AtomicU64,
    deallocs: AtomicU64,
    bytes: AtomicU64,
}

impl<A: GlobalAlloc> CountingAllocator<A> {
    /// Construct.
    pub const fn new(inner: A) -> Self {
        Self {
            inner,
            enabled: AtomicU64::new(0),
            allocs: AtomicU64::new(0),
            deallocs: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    /// Arm the counter (post-warmup gate).
    pub fn arm(&self) {
        self.enabled.store(1, Ordering::Release);
        self.allocs.store(0, Ordering::Release);
        self.deallocs.store(0, Ordering::Release);
        self.bytes.store(0, Ordering::Release);
    }

    /// Disarm.
    pub fn disarm(&self) {
        self.enabled.store(0, Ordering::Release);
    }

    /// Snapshot the alloc + bytes counters.
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.allocs.load(Ordering::Acquire),
            self.deallocs.load(Ordering::Acquire),
            self.bytes.load(Ordering::Acquire),
        )
    }

    /// Returns true if the counter is currently armed.
    pub fn is_armed(&self) -> bool {
        self.enabled.load(Ordering::Acquire) == 1
    }
}

// Safety: delegates everything to the inner allocator; the only side effect is bumping
// atomic counters when armed.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.is_armed() {
            self.allocs.fetch_add(1, Ordering::Relaxed);
            self.bytes
                .fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { self.inner.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if self.is_armed() {
            self.deallocs.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { self.inner.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if self.is_armed() {
            self.allocs.fetch_add(1, Ordering::Relaxed);
            self.bytes
                .fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { self.inner.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if self.is_armed() {
            self.allocs.fetch_add(1, Ordering::Relaxed);
            self.bytes.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}

/// Acceptance gate per §18.7: assert that no allocations occurred since `arm()`.
pub fn assert_zero_allocations(allocator: &CountingAllocator<impl GlobalAlloc>) {
    let (allocs, _deallocs, _bytes) = allocator.snapshot();
    assert_eq!(
        allocs, 0,
        "hot-path zero-allocation gate failed: {} alloc(s) recorded",
        allocs
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    #[test]
    fn arm_disarm_snapshot() {
        let a = CountingAllocator::new(System);
        a.arm();
        assert!(a.is_armed());
        let (allocs, deallocs, bytes) = a.snapshot();
        assert_eq!((allocs, deallocs, bytes), (0, 0, 0));
        a.disarm();
        assert!(!a.is_armed());
    }
}
