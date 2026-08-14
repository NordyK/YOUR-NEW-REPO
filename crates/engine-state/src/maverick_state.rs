//! Maverick V2 dense state store.
//!
//! Per-pool `ArcSwap<MaverickState>` — same pattern as DODO/Fluid/V2/V3 stores.
//! Quote math lives in `engine_amm::maverick_v2`.

use arc_swap::ArcSwap;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use engine_amm::maverick_v2::{MaverickState, TokenScale};

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};

/// Dense Maverick V2 state store.
pub struct MaverickStateStore {
    slots: Vec<ArcSwap<MaverickState>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

fn placeholder_state() -> MaverickState {
    let unit_scale = TokenScale {
        raw: 1,
        factor: U256::from(1u8),
        more_than_18: false,
    };
    MaverickState {
        address: Address::ZERO,
        token0: Address::ZERO,
        token1: Address::ZERO,
        token0_scale: unit_scale,
        token1_scale: unit_scale,
        fee_zto_d18: U256::ZERO,
        fee_otz_d18: U256::ZERO,
        tick_spacing: 1,
        lookback: 60,
        kinds: 0,
        lending_fee_rate_d18: U256::ZERO,
        protocol_fee_ratio_d3: 0,
        // Paused so the kernel returns InvariantViolation on any quote before
        // the writer commits a real snapshot.
        permissioned_swap: true,
        sqrt_price_d18: U256::ZERO,
        active_tick: 0,
        reserve0_d18: U256::ZERO,
        reserve1_d18: U256::ZERO,
        last_timestamp: 0,
        block_number: 0,
        log_index: 0,
        tick_radius: 0,
        ticks: Vec::new(),
    }
}

impl MaverickStateStore {
    /// Production constructor: the store is born BRANDED to `universe` (WI-06 F1 totality, codex
    /// item 1). `UniverseBuilder::new` is the SOLE caller — a resident store can never be
    /// constructed WILDCARD in a production build, so a foreign permit can never witness it.
    pub(crate) fn new_branded(universe: UniverseId) -> Self {
        Self {
            slots: Vec::new(),
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
        for _ in 0..n {
            s.slots.push(ArcSwap::from_pointee(placeholder_state()));
        }
        s
    }

    /// Append a slot and return its index.
    // WI-06 round 3 (codex item 1 residual): pub(crate) — slot allocation is a pre-seal builder
    // path, never a public post-seal &mut self mutation surface.
    pub(crate) fn allocate(&mut self) -> StateSlot {
        let i = self.slots.len() as u32;
        self.slots.push(ArcSwap::from_pointee(placeholder_state()));
        StateSlot(i)
    }

    /// Single-writer commit.
    pub fn set(&self, slot: StateSlot, value: MaverickState, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// Snapshot read.
    pub fn snapshot(&self, slot: StateSlot) -> Arc<MaverickState> {
        self.slots[slot.0 as usize].load_full()
    }

    /// Zero-allocation hot-path read.
    #[inline]
    pub fn with_snapshot<R>(&self, slot: StateSlot, f: impl FnOnce(&MaverickState) -> R) -> R {
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
impl Default for MaverickStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_set() {
        let mut s = MaverickStateStore::new();
        let slot = s.allocate();
        let mut state = placeholder_state();
        state.tick_spacing = 10;
        s.set(slot, state, &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(s.snapshot(slot).tick_spacing, 10);
    }
}
