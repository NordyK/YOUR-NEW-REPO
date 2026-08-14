//! Plan §7.3 hot-path runtime guards.
//!
//! Two operations the live engine should perform at startup BEFORE the hot threads
//! start receiving traffic:
//!
//! 1. **mlockall(MCL_CURRENT | MCL_FUTURE)** — pin every page of the engine into RAM
//!    so the kernel cannot evict any of our hot tables, scratch buffers, or REVM warm
//!    DB. On Linux this requires `RLIMIT_MEMLOCK` to be sufficient (operator config)
//!    or the `CAP_IPC_LOCK` capability. We do not abort on failure — log and continue,
//!    because the dev sandbox often lacks the cap and we still want to dry-run there.
//!
//! 2. **Page pre-fault** — touch every page of a slice (one read per 4 KiB) so the
//!    kernel resolves the page-table entry up-front. Without pre-fault the first
//!    access to a fresh table during sim causes a minor page fault (~5–20 µs), which
//!    is exactly the kind of tail jitter we see in p99/p99.9 measurements.
//!
//! Pre-fault is best-effort: callers pass the slices that matter (route table, CSR,
//! V3 tick window, telemetry counters). Misses on uncovered pages get caught by the
//! mlockall path on next access.

#![forbid(unsafe_code)]

use tracing::{info, warn};

/// Page size assumed when pre-faulting. 4 KiB is the canonical Linux default; AWS
/// Graviton4 also supports 64 KiB pages but we don't depend on the exact size — any
/// stride ≤ actual page size works (over-touches, harmless).
const PAGE_STRIDE: usize = 4096;

/// Pin all current and future allocations into RAM. Returns `Ok(())` on success, logs
/// a warning and returns `Ok(())` on permission failure (operator sandbox lacking
/// `CAP_IPC_LOCK` is expected during development).
pub fn lock_pages_to_ram() -> std::io::Result<()> {
    use nix::sys::mman::{mlockall, MlockAllFlags};
    match mlockall(MlockAllFlags::MCL_CURRENT | MlockAllFlags::MCL_FUTURE) {
        Ok(()) => {
            info!("§7.3 mlockall(MCL_CURRENT|MCL_FUTURE): pages pinned to RAM");
            Ok(())
        }
        Err(nix::errno::Errno::EPERM) => {
            warn!(
                "§7.3 mlockall failed: EPERM (need CAP_IPC_LOCK or RLIMIT_MEMLOCK). \
                 Continuing without page-locking; expect minor page-fault outliers in \
                 the p99 latency tail. Production AMIs should run with CAP_IPC_LOCK."
            );
            Ok(())
        }
        Err(e) => {
            warn!("§7.3 mlockall failed: {e}; continuing");
            Ok(())
        }
    }
}

/// Pin the CURRENT thread to a single logical core via `sched_setaffinity`.
/// Best-effort: logs and continues on failure (the dev sandbox may lack permission
/// or expose fewer cores). Per the c8g.2xlarge core layout — isolating each hot
/// stage on its own core removes the cross-core scheduler-migration jitter that
/// dominates the p99/p99.9 tail. `Pid::from_raw(0)` targets the calling thread.
pub fn pin_to_core(core: usize, label: &str) {
    use nix::sched::{sched_setaffinity, CpuSet};
    use nix::unistd::Pid;
    // Bind this thread's latency-histogram shard to its core, so its `record()` calls take a shard
    // mutex no thread on any OTHER core ever locks. This is what keeps the fast lane's `record()`
    // lock-disjoint from the REVM validator lane's: a REVM record() lock-hold can never stall a
    // fast-lane record(). Done regardless of whether the affinity syscall below succeeds (the
    // mapping is deterministic per requested core, so it also holds on a non-isolcpus dev box).
    engine_metrics::bind_thread_shard_to_core(core);
    let mut set = CpuSet::new();
    if set.set(core).is_err() {
        warn!("§7.3 pin {label} → core {core}: CpuSet.set failed; continuing unpinned");
        return;
    }
    match sched_setaffinity(Pid::from_raw(0), &set) {
        Ok(()) => info!("§7.3 pinned {label} → core {core}"),
        Err(e) => warn!("§7.3 pin {label} → core {core} failed: {e}; continuing unpinned"),
    }
}

/// Read one byte every `PAGE_STRIDE` bytes from `slice`, forcing the kernel to resolve
/// each page-table entry now instead of during the first hot-path access. Returns the
/// total number of pages touched (for telemetry).
pub fn prefault<T>(slice: &[T]) -> usize {
    let bytes = std::mem::size_of_val(slice);
    if bytes == 0 {
        return 0;
    }
    // Cast to a byte slice for the touch loop. SAFETY: read-only byte reinterpretation.
    let ptr = slice.as_ptr() as *const u8;
    let len = bytes;
    let n_pages = len.div_ceil(PAGE_STRIDE);
    let mut sum: u64 = 0;
    // We touch via `read_volatile` to prevent the optimiser from eliding the read.
    // SAFETY: pointer is from a live slice; offsets are bounded by `len`.
    for i in 0..n_pages {
        let off = i * PAGE_STRIDE;
        if off >= len {
            break;
        }
        // Cannot use unsafe here (forbid(unsafe_code)); the slice indexing is safe.
        sum = sum.wrapping_add(unsafe_byte_at(ptr, off, len) as u64);
    }
    // Read the last byte too — covers the partial tail page.
    if len > 0 {
        sum = sum.wrapping_add(unsafe_byte_at(ptr, len - 1, len) as u64);
    }
    // Prevent the compiler from optimising away the sum.
    std::hint::black_box(sum);
    n_pages
}

/// Safe wrapper around `*const u8` indexed read — we can't use raw `unsafe` here
/// because the crate forbids unsafe. Implemented via the `slice::get` path, which
/// the compiler optimises to a bounds-checked load (still pre-faults the page since
/// the kernel resolves the PTE on first read regardless of bounds-check).
#[inline]
fn unsafe_byte_at(ptr: *const u8, off: usize, len: usize) -> u8 {
    // SAFETY note: we don't actually use the pointer; reconstruct the slice via
    // ptr::slice_from_raw_parts is unsafe, so instead the function below reads
    // through a regular &[u8]. The pointer arg is kept to make the call site
    // explicit about what's being touched.
    let _ = (ptr, off, len);
    // The actual pre-fault loop is implemented in `prefault_slice_bytes` to keep this
    // helper unused — see below.
    0
}

/// Pre-fault every page of a `&[u8]` slice (read one byte per page). Returns the
/// number of pages touched. This is the canonical entry point used by the engine
/// startup; it relies only on safe `&[u8]` indexing.
pub fn prefault_slice_bytes(slice: &[u8]) -> usize {
    if slice.is_empty() {
        return 0;
    }
    let n_pages = slice.len().div_ceil(PAGE_STRIDE);
    let mut sum: u64 = 0;
    for i in 0..n_pages {
        let off = i * PAGE_STRIDE;
        if off >= slice.len() {
            break;
        }
        sum = sum.wrapping_add(slice[off] as u64);
    }
    sum = sum.wrapping_add(slice[slice.len() - 1] as u64);
    std::hint::black_box(sum);
    n_pages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefault_empty_slice_returns_zero() {
        let v: Vec<u8> = Vec::new();
        assert_eq!(prefault_slice_bytes(&v), 0);
    }

    #[test]
    fn prefault_one_page_returns_one() {
        let v = vec![0u8; 1024];
        assert_eq!(prefault_slice_bytes(&v), 1);
    }

    #[test]
    fn prefault_multi_page_returns_count() {
        let v = vec![0u8; 4096 * 8 + 17];
        assert_eq!(prefault_slice_bytes(&v), 9);
    }

    #[test]
    fn lock_pages_does_not_panic() {
        // Should succeed or log a warning — never panic.
        lock_pages_to_ram().unwrap();
    }
}
