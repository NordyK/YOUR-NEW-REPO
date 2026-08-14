//! Curve StableSwap-NG plain-pool state store.
//!
//! Per-pool `ArcSwap<CurveSlot>` — lock-free reads + atomic-swap writes, matching the
//! V2/V3/DODO/Balancer pattern. Swap math lives in `engine_amm::curve`; this module stores
//! the kernel snapshot plus the pool's token-address list so the hot path can resolve a
//! route's `(token_in, token_out)` pair to the kernel's coin indices `i`/`j`.
//!
//! Curve plain pools are multi-coin (2..=8). The route layer is pairwise, so a pool is
//! registered once per tradable coin pair (all sharing one `state_slot`); `index_of` maps
//! each hop's endpoint token to its kernel coin index.

use arc_swap::ArcSwap;
use std::sync::Arc;

use alloy_primitives::U256;
use engine_amm::curve::{precompute_d, StableSwapState, MAX_COINS};

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};
use crate::token::Address;

const _CURVE_SLOT_ALIGN_64: () = assert!(std::mem::align_of::<CurveSlot>() == 64);

/// A registered Curve StableSwap-NG pool: the kernel math state + the token addresses
/// aligned with `kernel.balances` / `kernel.rates` at each coin index.
///
/// B7: `#[repr(C, align(64))]` so the slot's leading kernel head loads as one cache line.
#[repr(C, align(64))]
#[derive(Debug, Clone)]
pub struct CurveSlot {
    /// Math state consumed by `engine_amm::curve::get_dy`.
    pub kernel: StableSwapState,
    /// Token address at each kernel coin index `0..kernel.n_coins`.
    pub tokens: [Address; MAX_COINS],
    /// T2.1: cached StableSwap invariant `D` for `kernel` (`precompute_d`), so the quote hot path
    /// skips the size-independent Newton solve. `ZERO` means "not cached" → the quote recomputes it
    /// (the `get_dy_cached` sentinel). Swaps atomically with `kernel`, so it can never mismatch the
    /// balances it was derived from.
    pub d_cached: U256,
}

impl CurveSlot {
    /// Build a slot, precomputing the cached invariant `D` for `kernel`. `D` is `ZERO` (the
    /// recompute sentinel) for non-quotable states (e.g. the empty placeholder), exactly matching
    /// `get_dy`'s own fallback — never wrong, only un-accelerated.
    pub fn new(kernel: StableSwapState, tokens: [Address; MAX_COINS]) -> Self {
        let d_cached = precompute_d(&kernel).unwrap_or(U256::ZERO);
        Self {
            kernel,
            tokens,
            d_cached,
        }
    }

    /// Resolve a token address to its kernel coin index.
    #[inline]
    pub fn index_of(&self, token: &Address) -> Option<usize> {
        (0..self.kernel.n_coins).find(|&k| &self.tokens[k] == token)
    }
}

fn placeholder() -> CurveSlot {
    CurveSlot {
        kernel: StableSwapState {
            n_coins: 0,
            balances: [0u128; MAX_COINS],
            rates: [0u128; MAX_COINS],
            a_param: 0,
            fee: 0,
            offpeg_fee_multiplier: 0,
        },
        tokens: [[0u8; 20]; MAX_COINS],
        d_cached: U256::ZERO,
    }
}

/// Dense Curve state store. One slot per registered pool address (NOT per pair — pairs
/// sharing a pool share the slot).
pub struct CurveStateStore {
    slots: Vec<ArcSwap<CurveSlot>>,
    /// B16: per-slot recycle bin (previous generation `Arc<CurveSlot>`) reused by `set_recycled`
    /// when no reader holds it — eliminates the per-`TokenExchange` `Arc::new` on the Curve hot
    /// write path.
    spares: Vec<arc_swap::ArcSwapOption<CurveSlot>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

impl CurveStateStore {
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

    /// Single-writer commit.
    pub fn set(&self, slot: StateSlot, value: CurveSlot, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// B16: allocation-recycling commit — drop-in replacement for [`set`](Self::set). Reuses the
    /// previous generation's allocation when every reader has released it (`Arc::get_mut`), else
    /// allocates fresh. Observably identical to `set`; zero per-update allocation in steady state.
    pub fn set_recycled(&self, slot: StateSlot, value: CurveSlot, _witness: &StateWriteTxn<'_>) {
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
    pub fn snapshot(&self, slot: StateSlot) -> Arc<CurveSlot> {
        self.slots[slot.0 as usize].load_full()
    }

    /// Zero-allocation hot-path read.
    #[inline]
    pub fn with_snapshot<R>(&self, slot: StateSlot, f: impl FnOnce(&CurveSlot) -> R) -> R {
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
impl Default for CurveStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_amm::curve::A_PRECISION;

    fn sample_slot() -> CurveSlot {
        let mut balances = [0u128; MAX_COINS];
        let mut rates = [0u128; MAX_COINS];
        let mut tokens = [[0u8; 20]; MAX_COINS];
        balances[0] = 1_000_000_000_000_000_000_000;
        balances[1] = 1_000_000_000;
        rates[0] = 1_000_000_000_000_000_000; // 18-dec coin
        rates[1] = 1_000_000_000_000_000_000_000_000_000_000; // 6-dec coin → 1e30
        tokens[0] = [0xaa; 20];
        tokens[1] = [0xbb; 20];
        CurveSlot::new(
            StableSwapState {
                n_coins: 2,
                balances,
                rates,
                a_param: 1000 * A_PRECISION,
                fee: 1_000_000,
                offpeg_fee_multiplier: 0,
            },
            tokens,
        )
    }

    #[test]
    fn store_set_and_snapshot_round_trip() {
        let mut store = CurveStateStore::new();
        let slot = store.allocate();
        store.set(slot, sample_slot(), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let snap = store.snapshot(slot);
        assert_eq!(snap.kernel.n_coins, 2);
        assert_eq!(snap.index_of(&[0xaa; 20]), Some(0));
        assert_eq!(snap.index_of(&[0xbb; 20]), Some(1));
        assert_eq!(snap.index_of(&[0xcc; 20]), None);
    }

    #[test]
    fn placeholder_is_empty() {
        let store = CurveStateStore::with_capacity(3);
        assert_eq!(store.len(), 3);
        let snap = store.snapshot(StateSlot(0));
        assert_eq!(snap.kernel.n_coins, 0);
    }

    fn cv(b0: u128) -> CurveSlot {
        let mut s = sample_slot();
        s.kernel.balances[0] = b0;
        s
    }

    #[test]
    fn set_recycled_is_observably_identical_to_set() {
        let store = CurveStateStore::with_capacity(1);
        let slot = StateSlot(0);
        store.set_recycled(slot, cv(111), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(store.snapshot(slot).kernel.balances[0], 111);
        store.set_recycled(slot, cv(222), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(store.snapshot(slot).kernel.balances[0], 222);
    }

    #[test]
    fn steady_state_reuses_the_allocation() {
        let store = CurveStateStore::with_capacity(1);
        let slot = StateSlot(0);
        store.set_recycled(slot, cv(1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        store.set_recycled(slot, cv(2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let gen_b = Arc::as_ptr(&store.slots[0].load_full());
        store.set_recycled(slot, cv(3), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        store.set_recycled(slot, cv(4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(
            Arc::as_ptr(&store.slots[0].load_full()),
            gen_b,
            "reuses prior allocation"
        );
        assert_eq!(store.snapshot(slot).kernel.balances[0], 4);
    }

    #[test]
    fn reader_holding_generation_forces_fresh_alloc() {
        let store = CurveStateStore::with_capacity(1);
        let slot = StateSlot(0);
        store.set_recycled(slot, cv(1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        store.set_recycled(slot, cv(2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let held = store.slots[0].load_full(); // pin gen B
        store.set_recycled(slot, cv(3), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        store.set_recycled(slot, cv(4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(held.kernel.balances[0], 2, "held reader untouched");
        assert_eq!(store.snapshot(slot).kernel.balances[0], 4);
    }
}
