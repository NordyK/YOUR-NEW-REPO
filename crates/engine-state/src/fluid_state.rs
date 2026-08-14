//! Fluid DEX v1/T1 + v2 D3/D4 dense state store.
//!
//! Plan §0.4 rule: no hot-path allocations, no locks. Same `arc_swap::ArcSwap`
//! pattern as `V2StateStore` and `V3StateStore` — writers build an
//! `Arc<FluidPoolSnapshot>` off the hot path and atomically swap it in; readers
//! load a `Guard` for the duration of the sim.
//!
//! `FluidPoolSnapshot` is an enum because v1 (constant-product collateral/debt
//! split) and v2 (concentrated-liquidity sqrt-price-X96 segment traversal) have
//! disjoint state shapes. The dispatch happens at quote time inside the kernel
//! via `FluidAmmPoolRef::V1 | V2`.

use arc_swap::ArcSwap;
use std::sync::Arc;

use alloy_primitives::U256;
use engine_amm::fluid::{
    TokenScale, V1LimitsNative, V1PoolState, V1SideReservesAdjusted, V2Boundary, V2PoolState,
    V2QuoteOptions,
};

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};

/// Per-slot Fluid pool snapshot. Either a v1/T1 pool (collateral/debt split
/// adjusted-reserve math) or a v2 D3/D4 pool (sqrt-price-X96 segment traversal).
///
/// v2 carries the directional boundary windows the kernel walks during a swap.
/// Boundaries are pre-sorted for the quote direction by the discovery /
/// resolver layer; the kernel does not re-sort on the hot path.
#[derive(Clone, Debug)]
pub enum FluidPoolSnapshot {
    /// Fluid v1 / T1 pool. `V1PoolState` holds adjusted (1e12) reserves on both
    /// collateral and debt sides, fee_1e6, token0/token1 scaling, native limits
    /// with snapshot timestamp, and enable/pause flags.
    V1(V1PoolState),
    /// Fluid v2 D3/D4 pool. State includes current sqrtPrice + activeLiquidity
    /// + lpFee. Boundaries are the pre-sorted liquidity-delta windows on each
    /// side of the current tick.
    V2 {
        /// Current pool state — sqrtPriceX96, activeLiquidity, lpFee.
        pool: V2PoolState,
        /// Boundaries for token0→token1 (descending sqrt prices).
        boundaries_zero_to_one: Vec<V2Boundary>,
        /// Boundaries for token1→token0 (ascending sqrt prices).
        boundaries_one_to_zero: Vec<V2Boundary>,
        /// Quote options (`V2QuoteOptions`) for both directions.
        options: V2QuoteOptions,
    },
}

impl Default for FluidPoolSnapshot {
    /// Empty / placeholder snapshot — V1 with zero reserves, paused. The Fluid
    /// kernel returns `AmmError::PoolPaused` / `ZeroReserves` / `NoLiquidity`
    /// on this state, so it's safe to leave a slot uninitialised until the
    /// writer commits a real snapshot. We cannot use `#[derive(Default)]` on
    /// V1PoolState upstream because `TokenScale` has no Default (the canonical
    /// "1/1" scale would have to be picked explicitly).
    fn default() -> Self {
        let unit_scale = TokenScale {
            numerator: U256::from(1u8),
            denominator: U256::from(1u8),
        };
        Self::V1(V1PoolState {
            fee_1e6: 0,
            token0_scale: unit_scale,
            token1_scale: unit_scale,
            collateral: V1SideReservesAdjusted::default(),
            debt: V1SideReservesAdjusted::default(),
            limits: V1LimitsNative::default(),
            smart_collateral_enabled: false,
            smart_debt_enabled: false,
            // Paused so the kernel cannot accidentally quote against zero
            // reserves before the writer commits a real snapshot.
            swaps_paused: true,
        })
    }
}

/// Dense Fluid state store backed by per-slot `ArcSwap<FluidPoolSnapshot>` —
/// lock-free reads, atomic-swap writes.
pub struct FluidStateStore {
    slots: Vec<ArcSwap<FluidPoolSnapshot>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

impl FluidStateStore {
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
            s.slots
                .push(ArcSwap::from_pointee(FluidPoolSnapshot::default()));
        }
        s
    }

    /// Append a slot and return its index.
    // WI-06 round 3 (codex item 1 residual): pub(crate) — slot allocation is a pre-seal builder
    // path, never a public post-seal &mut self mutation surface.
    pub(crate) fn allocate(&mut self) -> StateSlot {
        let i = self.slots.len() as u32;
        self.slots
            .push(ArcSwap::from_pointee(FluidPoolSnapshot::default()));
        StateSlot(i)
    }

    /// Single-writer set. Caller bumps `PoolTable` generation separately.
    pub fn set(&self, slot: StateSlot, value: FluidPoolSnapshot, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// Snapshot read (clones the Arc). Hot-path callers prefer `with_snapshot`
    /// or `load` to skip the clone.
    pub fn snapshot(&self, slot: StateSlot) -> Arc<FluidPoolSnapshot> {
        self.slots[slot.0 as usize].load_full()
    }

    /// Zero-allocation hot-path read API. Loads a `Guard` and passes it to the
    /// closure. The reader can borrow the inner `V1PoolState` / `V2PoolState`
    /// + boundaries directly.
    #[inline]
    pub fn with_snapshot<R>(&self, slot: StateSlot, f: impl FnOnce(&FluidPoolSnapshot) -> R) -> R {
        let guard = self.slots[slot.0 as usize].load();
        f(&guard)
    }

    /// Load a guard the caller can hold across multiple operations.
    #[inline]
    pub fn load(&self, slot: StateSlot) -> arc_swap::Guard<Arc<FluidPoolSnapshot>> {
        self.slots[slot.0 as usize].load()
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
impl Default for FluidStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_set_v1() {
        let mut s = FluidStateStore::new();
        let slot = s.allocate();
        // Default placeholder snapshot — paused V1 (kernel returns PoolPaused
        // on any quote).
        s.set(slot, FluidPoolSnapshot::default(), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        match &*s.snapshot(slot) {
            FluidPoolSnapshot::V1(_) => {}
            _ => panic!("expected V1 variant"),
        }
    }

    #[test]
    fn default_snapshot_is_v1() {
        let mut s = FluidStateStore::new();
        let slot = s.allocate();
        match &*s.snapshot(slot) {
            FluidPoolSnapshot::V1(_) => {}
            _ => panic!("default should be V1"),
        }
    }
}
