//! DODO V2 dense state store.
//!
//! Per-pool `ArcSwap<DodoState>` — lock-free reads + atomic-swap writes,
//! matching the V2/V3/Fluid pattern. Quote math lives in
//! `engine_amm::dodo_v2`; this module only stores and publishes snapshots.

use arc_swap::ArcSwap;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use engine_amm::dodo_v2::{DodoPoolClass, DodoRState, DodoState};

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};

/// Dense DODO V2 state store. One slot per registered pool.
pub struct DodoStateStore {
    slots: Vec<ArcSwap<DodoState>>,
    /// B16: per-slot recycle bin (previous generation `Arc<DodoState>`) reused by `set_recycled`
    /// when no reader holds it — eliminates the per-`DODOSwap` `Arc::new` on the DODO hot write
    /// path.
    spares: Vec<arc_swap::ArcSwapOption<DodoState>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

fn placeholder_state() -> DodoState {
    DodoState {
        address: Address::ZERO,
        pool_class: DodoPoolClass::Dvm,
        base_token: Address::ZERO,
        quote_token: Address::ZERO,
        i: U256::ZERO,
        k: U256::ZERO,
        base_reserve: U256::ZERO,
        quote_reserve: U256::ZERO,
        base_target: U256::ZERO,
        quote_target: U256::ZERO,
        r_state: DodoRState::One,
        lp_fee_rate_d18: U256::ZERO,
        mt_fee_rate_d18: U256::ZERO,
        fee_ppm: 0,
        block_number: 0,
        log_index: 0,
    }
}

impl DodoStateStore {
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
            s.slots.push(ArcSwap::from_pointee(placeholder_state()));
            s.spares.push(arc_swap::ArcSwapOption::empty());
        }
        s
    }

    /// Append a slot and return its index.
    // WI-06 round 3 (codex item 1 residual): pub(crate) — slot allocation is a pre-seal builder
    // path, never a public post-seal &mut self mutation surface.
    pub(crate) fn allocate(&mut self) -> StateSlot {
        let i = self.slots.len() as u32;
        self.slots.push(ArcSwap::from_pointee(placeholder_state()));
        self.spares.push(arc_swap::ArcSwapOption::empty());
        StateSlot(i)
    }

    /// Single-writer commit. Caller bumps `PoolTable` generation separately.
    pub fn set(&self, slot: StateSlot, value: DodoState, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// B16: allocation-recycling commit — drop-in replacement for [`set`](Self::set). Reuses the
    /// previous generation's allocation when every reader has released it (`Arc::get_mut`), else
    /// allocates fresh. Observably identical to `set`; zero per-update allocation in steady state.
    pub fn set_recycled(&self, slot: StateSlot, value: DodoState, _witness: &StateWriteTxn<'_>) {
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

    /// Snapshot read.
    pub fn snapshot(&self, slot: StateSlot) -> Arc<DodoState> {
        self.slots[slot.0 as usize].load_full()
    }

    /// Zero-allocation hot-path read.
    #[inline]
    pub fn with_snapshot<R>(&self, slot: StateSlot, f: impl FnOnce(&DodoState) -> R) -> R {
        let guard = self.slots[slot.0 as usize].load();
        f(&guard)
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
impl Default for DodoStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_set() {
        let mut s = DodoStateStore::new();
        let slot = s.allocate();
        let mut state = placeholder_state();
        state.fee_ppm = 4_000;
        s.set(slot, state, &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot).fee_ppm, 4_000);
    }

    fn dd(fee: u32) -> DodoState {
        let mut s = placeholder_state();
        s.fee_ppm = fee;
        s
    }

    #[test]
    fn set_recycled_is_observably_identical_to_set() {
        let s = DodoStateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, dd(11), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot).fee_ppm, 11);
        s.set_recycled(slot, dd(22), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot).fee_ppm, 22);
    }

    #[test]
    fn steady_state_reuses_the_allocation() {
        let s = DodoStateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, dd(1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, dd(2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let gen_b = Arc::as_ptr(&s.slots[0].load_full());
        s.set_recycled(slot, dd(3), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, dd(4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(
            Arc::as_ptr(&s.slots[0].load_full()),
            gen_b,
            "reuses prior allocation"
        );
        assert_eq!(s.snapshot(slot).fee_ppm, 4);
    }

    #[test]
    fn reader_holding_generation_forces_fresh_alloc() {
        let s = DodoStateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, dd(1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, dd(2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let held = s.slots[0].load_full();
        s.set_recycled(slot, dd(3), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, dd(4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(held.fee_ppm, 2, "held reader untouched");
        assert_eq!(s.snapshot(slot).fee_ppm, 4);
    }
}
