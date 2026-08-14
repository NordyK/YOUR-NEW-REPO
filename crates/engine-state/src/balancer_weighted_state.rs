//! Per-pool state slots for Balancer V2 **Weighted Pools**.
//!
//! Mirror of [`crate::balancer_state`] but storing per-coin (balance, normalized weight) pairs
//! that feed `engine-amm::balancer_weighted::calc_out_given_in`. Two-token Weighted Pools are
//! the dominant Arbitrum shape ($4M / 30d per Dune in 2026-05); the `MAX_BALANCER_TOKENS = 8`
//! upper bound also covers ComposableWeighted (which Balancer V2 caps at 8 tokens).
//!
//! Lock-free reads via `ArcSwap<BalancerWeightedSlot>`, same single-writer pattern as the rest
//! of the state crate.

use alloy_primitives::U256;
use arc_swap::ArcSwap;
use std::sync::Arc;

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};
use crate::token::Address;

/// Upper bound on coins per Balancer Weighted Pool. Vault caps at 8 (`_MAX_POOL_TOKENS` in
/// `BasePool.sol`).
pub const MAX_BALANCER_TOKENS: usize = 8;

/// One Balancer V2 Weighted Pool's quotable state.
#[derive(Debug, Clone)]
pub struct BalancerWeightedSlot {
    /// Per-token balance at 10^18 fixed-point (Balancer's canonical scale).
    pub balances_d18: [U256; MAX_BALANCER_TOKENS],
    /// Per-token upscale factor `10^(18 - decimals)` (raw token units → the 1e18 domain). Lets the
    /// feed predictor convert a raw swap amount to `balances_d18` units without the token table.
    pub scaling_d18: [U256; MAX_BALANCER_TOKENS],
    /// Per-token normalized weight at 10^18 fixed-point (sum across active tokens = 1e18).
    pub weights_d18: [U256; MAX_BALANCER_TOKENS],
    /// Token addresses, indexed in the same order as `balances_d18` / `weights_d18`.
    pub tokens: [Address; MAX_BALANCER_TOKENS],
    /// Active token count (`n_tokens <= MAX_BALANCER_TOKENS`). The first `n_tokens` slots are
    /// live; the tail must be zero.
    pub n_tokens: u8,
    /// Swap fee in 1e18 fixed-point (per Balancer's `getSwapFeePercentage`).
    pub swap_fee_d18: U256,
    /// Balancer V2 `poolId` (bytes32) — required by the Vault single-swap the executor issues.
    pub pool_id: [u8; 32],
}

impl BalancerWeightedSlot {
    /// Resolve a token address to its slot index.
    #[inline]
    pub fn index_of(&self, token: &Address) -> Option<usize> {
        (0..self.n_tokens as usize).find(|&k| &self.tokens[k] == token)
    }
}

fn placeholder() -> BalancerWeightedSlot {
    BalancerWeightedSlot {
        balances_d18: [U256::ZERO; MAX_BALANCER_TOKENS],
        scaling_d18: [U256::ZERO; MAX_BALANCER_TOKENS],
        weights_d18: [U256::ZERO; MAX_BALANCER_TOKENS],
        tokens: [[0u8; 20]; MAX_BALANCER_TOKENS],
        n_tokens: 0,
        swap_fee_d18: U256::ZERO,
        pool_id: [0u8; 32],
    }
}

/// Dense Weighted-pool state store, one slot per pool address.
pub struct BalancerWeightedStateStore {
    slots: Vec<ArcSwap<BalancerWeightedSlot>>,
    /// B16: per-slot recycle bin (previous generation `Arc<BalancerWeightedSlot>`) reused by
    /// `set_recycled` when no reader holds it — eliminates the per-update `Arc::new` (the slot
    /// carries fixed `[U256; MAX_BALANCER_TOKENS]` arrays, so the allocation is sizeable).
    spares: Vec<arc_swap::ArcSwapOption<BalancerWeightedSlot>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

impl BalancerWeightedStateStore {
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
    pub fn set(&self, slot: StateSlot, value: BalancerWeightedSlot, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// B16: allocation-recycling commit — drop-in replacement for [`set`](Self::set). Reuses the
    /// previous generation's allocation when every reader has released it (`Arc::get_mut`), else
    /// allocates fresh. Observably identical to `set`; zero per-update allocation in steady state.
    pub fn set_recycled(&self, slot: StateSlot, value: BalancerWeightedSlot, _witness: &StateWriteTxn<'_>) {
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
    pub fn snapshot(&self, slot: StateSlot) -> Arc<BalancerWeightedSlot> {
        self.slots[slot.0 as usize].load_full()
    }

    /// Zero-allocation hot-path read.
    #[inline]
    pub fn with_snapshot<R>(
        &self,
        slot: StateSlot,
        f: impl FnOnce(&BalancerWeightedSlot) -> R,
    ) -> R {
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
impl Default for BalancerWeightedStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_8020_slot() -> BalancerWeightedSlot {
        let mut tokens = [[0u8; 20]; MAX_BALANCER_TOKENS];
        tokens[0] = [0x82; 20]; // WETH
        tokens[1] = [0xaf; 20]; // USDC
        let mut balances = [U256::ZERO; MAX_BALANCER_TOKENS];
        balances[0] = U256::from(1_000_000_000_000_000_000_000u128); // 1000 WETH in 1e18
        balances[1] = U256::from(100_000_000_000_000_000_000u128); // 100 USDC normalized to 1e18
        let mut weights = [U256::ZERO; MAX_BALANCER_TOKENS];
        weights[0] = U256::from(800_000_000_000_000_000u128); // 0.8
        weights[1] = U256::from(200_000_000_000_000_000u128); // 0.2
        BalancerWeightedSlot {
            balances_d18: balances,
            scaling_d18: [U256::from(1_000_000_000_000_000_000u128); MAX_BALANCER_TOKENS],
            weights_d18: weights,
            tokens,
            n_tokens: 2,
            swap_fee_d18: U256::from(1_000_000_000_000_000u128), // 0.1% fee
            pool_id: [0x7c; 32],
        }
    }

    #[test]
    fn store_allocates_and_commits_atomically() {
        let mut store = BalancerWeightedStateStore::new();
        let slot = store.allocate();
        let placeholder = store.snapshot(slot);
        assert_eq!(placeholder.n_tokens, 0);

        let s = sample_8020_slot();
        store.set(slot, s.clone(), &crate::publication::StateWriterPermit::test_mint().begin_txn());

        let read = store.snapshot(slot);
        assert_eq!(read.n_tokens, 2);
        assert_eq!(read.tokens[0], [0x82; 20]);
        assert_eq!(read.weights_d18[0], U256::from(800_000_000_000_000_000u128));
    }

    #[test]
    fn index_of_resolves_token_to_slot_index() {
        let s = sample_8020_slot();
        assert_eq!(s.index_of(&[0x82; 20]), Some(0));
        assert_eq!(s.index_of(&[0xaf; 20]), Some(1));
        assert_eq!(s.index_of(&[0xff; 20]), None);
    }

    #[test]
    fn slot_drives_engine_amm_calc_out_given_in() {
        // End-to-end: a committed slot's per-token balances + weights feed the engine-amm
        // Balancer Weighted kernel; the integer output matches the standalone math.
        let s = sample_8020_slot();
        // Quote: 10 WETH in (token 0) → ? USDC out (token 1).
        let amt_in = U256::from(10_000_000_000_000_000_000u128);
        let out = engine_amm::balancer_weighted::calc_out_given_in(
            s.balances_d18[0],
            s.weights_d18[0],
            s.balances_d18[1],
            s.weights_d18[1],
            amt_in,
        )
        .expect("8020 quote should succeed");
        // The kernel returns ~3.9019e18 — same value verified in engine-amm's own tests.
        let expected = U256::from(3_901_965_517_241_379_310u128);
        let drift = if out >= expected {
            out - expected
        } else {
            expected - out
        };
        assert!(
            drift < U256::from(100_000_000_000u128),
            "slot-driven quote drifted from kernel reference: out={out} expected≈{expected}"
        );
    }

    fn bw(b0: u128) -> BalancerWeightedSlot {
        let mut s = sample_8020_slot();
        s.balances_d18[0] = U256::from(b0);
        s
    }

    #[test]
    fn set_recycled_is_observably_identical_to_set() {
        let store = BalancerWeightedStateStore::with_capacity(1);
        let slot = StateSlot(0);
        store.set_recycled(slot, bw(111), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(store.snapshot(slot).balances_d18[0], U256::from(111u64));
        store.set_recycled(slot, bw(222), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(store.snapshot(slot).balances_d18[0], U256::from(222u64));
    }

    #[test]
    fn steady_state_reuses_the_allocation() {
        let store = BalancerWeightedStateStore::with_capacity(1);
        let slot = StateSlot(0);
        store.set_recycled(slot, bw(1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        store.set_recycled(slot, bw(2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let gen_b = Arc::as_ptr(&store.slots[0].load_full());
        store.set_recycled(slot, bw(3), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        store.set_recycled(slot, bw(4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(
            Arc::as_ptr(&store.slots[0].load_full()),
            gen_b,
            "reuses prior allocation"
        );
        assert_eq!(store.snapshot(slot).balances_d18[0], U256::from(4u64));
    }

    #[test]
    fn reader_holding_generation_forces_fresh_alloc() {
        let store = BalancerWeightedStateStore::with_capacity(1);
        let slot = StateSlot(0);
        store.set_recycled(slot, bw(1), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        store.set_recycled(slot, bw(2), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        let held = store.slots[0].load_full();
        store.set_recycled(slot, bw(3), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        store.set_recycled(slot, bw(4), &crate::publication::StateWriterPermit::test_mint().begin_txn());
        assert_eq!(
            held.balances_d18[0],
            U256::from(2u64),
            "held reader untouched"
        );
        assert_eq!(store.snapshot(slot).balances_d18[0], U256::from(4u64));
    }
}
