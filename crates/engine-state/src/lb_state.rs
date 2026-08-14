//! Trader Joe Liquidity Book (LB v2.1/v2.2) bin-AMM state store.
//!
//! Per-pool `ArcSwap<LbSlot>` — lock-free reads + atomic-swap writes,
//! matching the V2/V3/DODO/Balancer/Fluid pattern. Swap math lives in
//! `engine_amm::trader_joe_lb`; this module stores the kernel `LBState`
//! (active bin + the ±MAX_TRAVERSAL_BINS bin window, fee params, etc.) plus the
//! pool's token0/token1 addresses so the hot path can resolve a hop's
//! `(token_in, token_out)` direction to the kernel's `zero_for_one` flag.
//!
//! In LB, tokenX is always the lower-address token (== token0); so selling
//! token0 (`zero_for_one == true`) == swapForY in the kernel.

use arc_swap::ArcSwap;
use std::collections::BTreeMap;
use std::sync::Arc;

use engine_amm::trader_joe_lb::LBState;

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};
use crate::token::Address;

/// A registered LB pool: the kernel math state + the canonical token0/token1
/// addresses (token0 == kernel tokenX, token1 == kernel tokenY).
#[derive(Debug, Clone)]
pub struct LbSlot {
    /// Bin-AMM state consumed by `engine_amm::trader_joe_lb::simulate_lb_swap_at`.
    pub kernel: LBState,
    /// Lower-address token (== kernel tokenX). Selling this is `zero_for_one`.
    pub token0: Address,
    /// Higher-address token (== kernel tokenY).
    pub token1: Address,
}

fn placeholder_kernel() -> LBState {
    LBState {
        active_id: 0,
        bin_step: 0,
        reserve_x: 0,
        reserve_y: 0,
        base_factor: 0,
        filter_period: 0,
        decay_period: 0,
        reduction_factor: 0,
        variable_fee_control: 0,
        protocol_share: 0,
        max_volatility_accumulator: 0,
        volatility_accumulator: 0,
        volatility_reference: 0,
        id_reference: 0,
        time_of_last_update: 0,
        block_number: 0,
        log_index: 0,
        stale: false,
        bin_map: Arc::new(BTreeMap::new()),
    }
}

fn placeholder() -> LbSlot {
    LbSlot {
        kernel: placeholder_kernel(),
        token0: [0u8; 20],
        token1: [0u8; 20],
    }
}

/// Dense LB state store. One slot per registered pool.
pub struct LbStateStore {
    slots: Vec<ArcSwap<LbSlot>>,
    /// B16: per-slot recycle bin (previous generation `Arc<LbSlot>`) reused by `set_recycled`
    /// when no reader holds it — eliminates the per-LB-`Swap` `Arc::new` on the LB hot write path.
    spares: Vec<arc_swap::ArcSwapOption<LbSlot>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

impl LbStateStore {
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
            s.slots.push(ArcSwap::from_pointee(placeholder()));
            s.spares.push(arc_swap::ArcSwapOption::empty());
        }
        s
    }

    /// Append a slot and return its index.
    // WI-06 round 3 (codex item 1 residual): pub(crate) — slot allocation is a pre-seal builder
    // path, never a public post-seal &mut self mutation surface.
    pub(crate) fn allocate(&mut self) -> StateSlot {
        let i = self.slots.len() as u32;
        self.slots.push(ArcSwap::from_pointee(placeholder()));
        self.spares.push(arc_swap::ArcSwapOption::empty());
        StateSlot(i)
    }

    /// Single-writer commit. Caller bumps `PoolTable` generation separately.
    pub fn set(&self, slot: StateSlot, value: LbSlot, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// B16: allocation-recycling commit — drop-in replacement for [`set`](Self::set). Reuses the
    /// previous generation's allocation when every reader has released it (`Arc::get_mut`), else
    /// allocates fresh. Observably identical to `set`; zero per-update allocation in steady state.
    pub fn set_recycled(&self, slot: StateSlot, value: LbSlot, _witness: &StateWriteTxn<'_>) {
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
    pub fn snapshot(&self, slot: StateSlot) -> Arc<LbSlot> {
        self.slots[slot.0 as usize].load_full()
    }

    /// Zero-allocation hot-path read.
    #[inline]
    pub fn with_snapshot<R>(&self, slot: StateSlot, f: impl FnOnce(&LbSlot) -> R) -> R {
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
impl Default for LbStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_set() {
        let mut s = LbStateStore::new();
        let slot = s.allocate();
        let mut value = placeholder();
        value.kernel.active_id = 8_388_608;
        value.kernel.bin_step = 20;
        value.token0 = [0x11; 20];
        value.token1 = [0x22; 20];
        s.set(slot, value, &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let snap = s.snapshot(slot);
        assert_eq!(snap.kernel.active_id, 8_388_608);
        assert_eq!(snap.kernel.bin_step, 20);
        assert_eq!(snap.token0, [0x11; 20]);
        assert_eq!(snap.token1, [0x22; 20]);
    }

    fn lb(id: u32) -> LbSlot {
        let mut v = placeholder();
        v.kernel.active_id = id;
        v
    }

    #[test]
    fn set_recycled_is_observably_identical_to_set() {
        let s = LbStateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, lb(100), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot).kernel.active_id, 100);
        s.set_recycled(slot, lb(200), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot).kernel.active_id, 200);
    }

    #[test]
    fn steady_state_reuses_the_allocation() {
        let s = LbStateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, lb(1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, lb(2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let gen_b = Arc::as_ptr(&s.slots[0].load_full());
        s.set_recycled(slot, lb(3), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, lb(4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(
            Arc::as_ptr(&s.slots[0].load_full()),
            gen_b,
            "reuses prior allocation"
        );
        assert_eq!(s.snapshot(slot).kernel.active_id, 4);
    }

    #[test]
    fn reader_holding_generation_forces_fresh_alloc() {
        let s = LbStateStore::with_capacity(1);
        let slot = StateSlot(0);
        s.set_recycled(slot, lb(1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, lb(2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let held = s.slots[0].load_full();
        s.set_recycled(slot, lb(3), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        s.set_recycled(slot, lb(4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(held.kernel.active_id, 2, "held reader untouched");
        assert_eq!(s.snapshot(slot).kernel.active_id, 4);
    }
}
