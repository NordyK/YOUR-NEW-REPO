//! B20: software prefetch hints for the hot scoring scan.
//!
//! The scoring loop in `pipeline.rs` iterates `seen` routes, and for each route chases pointers
//! into `route_table` + per-hop pool `meta`/state. Those loads miss L1/L2 when the route set is
//! large and cold (right after a burst of dirty pools). Issuing a `prefetcht0` for the NEXT
//! route's `Route` struct while the CURRENT route is still being scored hides part of that miss
//! latency: the line is in-flight to L1 by the time the loop reaches it.
//!
//! ## Why this is safe to expose as a *safe* fn
//! A software prefetch is a pure hint. On x86_64 (`_mm_prefetch`) and aarch64 (`prfm`) the
//! instruction **never faults** — passing a wild, unmapped, or misaligned address is defined to
//! do nothing rather than trap. So the internal `unsafe` carries no precondition the caller must
//! uphold, and the function is sound to call with any pointer (including dangling). We still take
//! a typed `*const T` so callers pass a real element address by reference.
//!
//! On targets without a stable prefetch intrinsic this compiles to a no-op (the optimizer drops
//! the call), so the hot path is never penalised on unsupported hardware.

/// Prefetch the cache line containing `*p` into L1 (temporal, all cache levels — `T0` hint).
///
/// A hint only: it never faults and never changes observable program state, so it is sound to
/// call with any pointer, including one that does not point to a live `T`. Use it to warm the
/// line for an imminent read (e.g. the next loop iteration's data).
#[inline(always)]
pub fn prefetch_read_t0<T>(p: *const T) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `_mm_prefetch` is a non-faulting hint; the pointer is never dereferenced by the
        // CPU for the prefetch and any address (mapped or not) is defined behaviour.
        unsafe {
            std::arch::x86_64::_mm_prefetch(p as *const i8, std::arch::x86_64::_MM_HINT_T0);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: `prfm pldl1keep` is a non-faulting hint on AArch64 — an unmapped/misaligned
        // address is architecturally defined to be ignored. No memory is read or written.
        unsafe {
            std::arch::asm!(
                "prfm pldl1keep, [{addr}]",
                addr = in(reg) p,
                options(nostack, preserves_flags, readonly),
            );
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        // No stable prefetch intrinsic on this target — no-op. Touch `p` so it isn't flagged
        // unused; the optimizer removes this.
        let _ = p;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefetch_live_pointer_is_a_noop_and_does_not_corrupt() {
        // Prefetching a live element must not change its value (it's a pure hint).
        let data = [11u64, 22, 33, 44];
        for x in &data {
            prefetch_read_t0(x as *const u64);
        }
        assert_eq!(data, [11, 22, 33, 44]);
    }

    #[test]
    fn prefetch_dangling_pointer_does_not_fault() {
        // The whole point: a wild/dangling address must be tolerated (non-faulting hint). If this
        // ever traps, the test process dies — which is exactly the regression we want to catch.
        let wild = 0xdead_beef_0000usize as *const u8;
        prefetch_read_t0(wild);
        // A null pointer too.
        prefetch_read_t0(std::ptr::null::<u64>());
    }

    #[test]
    fn prefetch_next_element_pattern() {
        // Mirrors the pipeline usage: while reading element i, prefetch element i+1.
        let v: Vec<u64> = (0..256).collect();
        let mut acc = 0u64;
        for i in 0..v.len() {
            if i + 1 < v.len() {
                prefetch_read_t0(&v[i + 1] as *const u64);
            }
            acc = acc.wrapping_add(v[i]);
        }
        assert_eq!(acc, (0..256u64).sum());
    }
}
