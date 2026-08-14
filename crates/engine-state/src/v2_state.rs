//! V2-family dense state store.
//!
//! One state slot per V2 pool, accessed via `StateSlot`. Updates increment the pool
//! generation (held in `PoolTable`); readers re-check generation when copying out.
//!
//! Lock-free reads via `arc_swap::ArcSwap` — same pattern as `V3StateStore`. The
//! previous `Mutex<V2State>` design held a `parking_lot::Mutex` lock per read, which
//! contended with the classifier's writes during dirty events.

use arc_swap::ArcSwap;
use std::sync::Arc;

use engine_amm::V2Reserves;

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};

// B7: lock the cache-line alignment of every hot slot at compile time. If anyone changes the
// repr or grows the struct past a line, the compile fails and the regression is visible.
const _V2_STATE_ALIGN_64: () = assert!(std::mem::align_of::<V2State>() == 64);

/// V2 pool live state. `Copy` so it can be read out under the seqlock.
///
/// B7: `#[repr(C, align(64))]` — every snapshot read brings exactly one cache line into L1
/// (no straddle). On Graviton4 the LLC line is 64 B; the V2 reserves head fits in 32 B so
/// the alignment costs ~32 B of internal padding per heap-allocated state, paying for itself
/// at the first dirty fanout that touches >1 V2 slot.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct V2State {
    /// Reserves at the last accepted sequencer event.
    pub reserves: V2Reserves256,
}

/// Local mirror of `engine_amm::V2Reserves` with the same fields (avoids a circular
/// `Default` requirement on the upstream type).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct V2Reserves256 {
    /// Token0 reserve.
    pub reserve0: u128,
    /// Token1 reserve.
    pub reserve1: u128,
    /// Block timestamp last (V2 oracle field).
    pub block_timestamp_last: u32,
}

impl From<V2Reserves256> for V2Reserves {
    fn from(r: V2Reserves256) -> Self {
        V2Reserves {
            reserve0: r.reserve0,
            reserve1: r.reserve1,
            block_timestamp_last: r.block_timestamp_last,
        }
    }
}

impl From<V2Reserves> for V2Reserves256 {
    fn from(r: V2Reserves) -> Self {
        Self {
            reserve0: r.reserve0,
            reserve1: r.reserve1,
            block_timestamp_last: r.block_timestamp_last,
        }
    }
}

/// V2 dense state store backed by per-slot `ArcSwap<V2State>` — lock-free reads,
/// writers swap a fresh Arc atomically.
pub struct V2StateStore {
    slots: Vec<ArcSwap<V2State>>,
    /// B16: per-slot recycle bin holding the previous generation `Arc<V2State>` after a
    /// `set_recycled`. V2 `Sync` events are the highest-frequency update on Arbitrum, so
    /// eliminating their per-update `Arc::new` (control block + 64-byte state) is the biggest
    /// allocator win of the family. Lock-free via `ArcSwapOption` — the writer reuses the prior
    /// allocation IFF every reader has released it (`Arc::get_mut` succeeds).
    spares: Vec<arc_swap::ArcSwapOption<V2State>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

impl V2StateStore {
    /// Production constructor: the store is born BRANDED to `universe` (WI-06 F1 totality, codex
    /// item 1). `UniverseBuilder::new` is the SOLE caller — a resident store can never be
    /// constructed WILDCARD in a production build, so a foreign permit can never witness it.
    pub(crate) fn new_branded(universe: UniverseId) -> Self {
        Self {
            slots: Vec::new(),
            spares: Vec::new(),
            universe_id: universe,
        }
    }

    /// Empty WILDCARD store — TEST ONLY (compiled out of every production build via the same cfg
    /// as `StateWriterPermit::test_mint`). Substrate/loom/downstream tests witness it with a
    /// WILDCARD `test_mint` permit.
    #[cfg(any(test, loom, feature = "test-permit"))]
    pub fn new() -> Self {
        Self::new_branded(UniverseId::WILDCARD)
    }

    /// Allocate `n` empty slots — TEST ONLY (WILDCARD constructor path; gated with `new`).
    #[cfg(any(test, loom, feature = "test-permit"))]
    pub fn with_capacity(n: usize) -> Self {
        let mut s = Self::new();
        s.slots.reserve_exact(n);
        s.spares.reserve_exact(n);
        for _ in 0..n {
            s.slots.push(ArcSwap::from_pointee(V2State::default()));
            s.spares.push(arc_swap::ArcSwapOption::empty());
        }
        s
    }

    /// Append a slot and return its index.
    // WI-06 round 3 (codex item 1 residual): pub(crate) — slot allocation is a pre-seal builder
    // path, never a public post-seal &mut self mutation surface.
    pub(crate) fn allocate(&mut self) -> StateSlot {
        let i = self.slots.len() as u32;
        self.slots.push(ArcSwap::from_pointee(V2State::default()));
        self.spares.push(arc_swap::ArcSwapOption::empty());
        StateSlot(i)
    }

    /// Single-writer set. Caller is responsible for bumping the pool generation
    /// in `PoolTable` (the pool table's atomic counter is the seqlock generation).
    pub fn set(&self, slot: StateSlot, value: V2State, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// B16: allocation-recycling single-writer set — drop-in replacement for [`set`](Self::set)
    /// with identical observable behaviour, but reuses the previous generation's heap allocation
    /// when no reader still holds it, so the steady state does **zero** allocations per update.
    ///
    /// The previous `set_recycled` stashed the just-replaced `Arc` in this slot's recycle bin. We
    /// take it back; if `Arc::get_mut` succeeds (every reader released its `Guard`), we overwrite
    /// the value in place (`V2State` is `Copy`, so this is a single store reusing the allocation).
    /// If a reader still holds it, we fall back to a fresh `Arc::new` — never mutating a value a
    /// reader is observing. Either way the now-replaced `Arc` becomes the next recycle candidate.
    pub fn set_recycled(&self, slot: StateSlot, value: V2State, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        let i = slot.0 as usize;
        let new_arc = match self.spares[i].swap(None) {
            Some(mut spare) => match Arc::get_mut(&mut spare) {
                Some(st) => {
                    *st = value;
                    spare
                }
                None => Arc::new(value),
            },
            None => Arc::new(value),
        };
        let prev = self.slots[i].swap(new_arc);
        self.spares[i].store(Some(prev));
    }

    /// Snapshot read. `V2State` is `Copy` (small fixed size), so we dereference
    /// the `Guard` and copy out. Zero allocations on read path.
    pub fn snapshot(&self, slot: StateSlot) -> V2State {
        **self.slots[slot.0 as usize].load()
    }

    /// Number of slots.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(any(test, loom, feature = "test-permit"))]
impl Default for V2StateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_set() {
        let mut s = V2StateStore::new();
        let slot = s.allocate();
        let val = V2State {
            reserves: V2Reserves256 {
                reserve0: 100,
                reserve1: 200,
                block_timestamp_last: 0,
            },
        };
        s.set(slot, val, &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot), val);
    }

    fn v(r0: u128, r1: u128) -> V2State {
        V2State {
            reserves: V2Reserves256 {
                reserve0: r0,
                reserve1: r1,
                block_timestamp_last: 0,
            },
        }
    }

    #[test]
    fn set_recycled_is_observably_identical_to_set() {
        let s = V2StateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, v(1, 2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot), v(1, 2));
        s.set_recycled(slot, v(3, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot), v(3, 4));
    }

    #[test]
    fn steady_state_reuses_the_allocation_no_new_alloc() {
        // Once the recycle bin has cycled and no reader holds the spare, a set_recycled reuses the
        // SAME allocation (pointer identity) — proving zero per-Sync allocation in steady state.
        let s = V2StateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, v(1, 1), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen A published; spare = gen 0
        s.set_recycled(slot, v(2, 2), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen B published; spare = gen A
        let gen_b_ptr = Arc::as_ptr(&s.slots[0].load_full());
        s.set_recycled(slot, v(3, 3), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen C reuses gen A; spare = gen B
        s.set_recycled(slot, v(4, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen D reuses gen B's allocation
        let gen_d_ptr = Arc::as_ptr(&s.slots[0].load_full());
        assert_eq!(
            gen_d_ptr, gen_b_ptr,
            "set_recycled reuses a prior allocation"
        );
        assert_eq!(s.snapshot(slot), v(4, 4));
    }

    // FAST-LANE ↔ REVM ISOLATION: the fast lane (sim@3) reads pool reserves via `snapshot()` — an
    // ArcSwap load, which is WAIT-FREE. Neither the single writer (state-writer@5) nor a concurrent
    // REVM-style reader on the same slot may block or tear a fast-lane read. This stress asserts the
    // fast-lane reader makes full progress under a hammering writer + a second reader and NEVER
    // observes a torn state. It is a regression tripwire: swapping the per-slot ArcSwap back for a
    // Mutex (as the pre-B7 design had) would reintroduce writer/reader blocking on the read path.
    #[test]
    fn fast_lane_reads_are_wait_free_under_concurrent_writer_and_revm_reader() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let store = Arc::new(V2StateStore::with_capacity(1));
        let slot = StateSlot(0);
        let stop = Arc::new(AtomicBool::new(false));
        // Writer publishes INTERNALLY-CONSISTENT states (reserve0 == reserve1 == n): a torn read
        // would surface as reserve0 != reserve1.
        let w = {
            let s = Arc::clone(&store);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut n: u128 = 1;
                while !stop.load(Ordering::Relaxed) {
                    s.set_recycled(slot, v(n, n), &crate::publication::StateWriterPermit::test_mint().begin_txn());
                    n = n.wrapping_add(1).max(1);
                }
            })
        };
        // A second, independent lane (models the REVM validator) hammering the same slot's reads.
        let r = {
            let s = Arc::clone(&store);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::hint::black_box(s.snapshot(slot));
                }
            })
        };
        // Fast-lane reader: complete a large number of reads (progress ⇒ not blocked out) with no
        // torn value ever observed.
        for _ in 0..200_000u32 {
            let st = store.snapshot(slot);
            assert_eq!(
                st.reserves.reserve0, st.reserves.reserve1,
                "fast-lane snapshot must never observe a torn (cross-generation) state"
            );
        }
        stop.store(true, Ordering::Relaxed);
        w.join().unwrap();
        r.join().unwrap();
    }

    #[test]
    fn reader_holding_previous_generation_forces_fresh_alloc() {
        // If a reader still holds the spare's generation, set_recycled must NOT mutate it in place
        // (would corrupt the reader); it allocates fresh and the held snapshot stays intact.
        let s = V2StateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, v(1, 1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, v(2, 2), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // published gen B; spare = gen A
        let held = s.slots[0].load_full(); // pin gen B (strong_count >= 2)
        assert_eq!(held.reserves.reserve0, 2);
        s.set_recycled(slot, v(3, 3), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // gen B moves to spare while held
        s.set_recycled(slot, v(4, 4), &crate::publication::StateWriterPermit::test_mint().begin_txn()); // tries to recycle gen B → get_mut fails → fresh alloc
        assert_eq!(
            held.reserves.reserve0, 2,
            "held reader's snapshot untouched"
        );
        assert_eq!(s.snapshot(slot), v(4, 4));
    }
}
