//! Balancer V2 stable-pool state store.
//!
//! Per-pool `ArcSwap<BalancerStableSlot>` — lock-free reads + atomic-swap writes,
//! matching the V2/V3/DODO/Fluid pattern. Swap math lives in
//! `engine_amm::balancer_stable`; this module stores the snapshot plus the pool's
//! token-address list so the hot path can resolve a route's `(token_in, token_out)`
//! pair to the kernel's coin indices.
//!
//! Balancer pools are multi-token (2..=5 + BPT). The route layer is pairwise, but there is exactly
//! ONE pool id and ONE `state_slot` per Vault ADDRESS (carrying the full multi-token state):
//! `add_balancer_stable_pool` registers the first pair and reuses that id+slot for every sibling
//! pair, so `index_of` maps each hop's endpoint token to its kernel index against the shared slot.
//! A single Vault-Swap (resolved 1:1 by address) therefore refreshes all pairs at once.

use arc_swap::ArcSwap;
use std::sync::Arc;

use engine_amm::balancer_stable::{BalancerStableState, MAX_TOKENS};

use crate::ids::StateSlot;
use crate::publication::{StateWriteTxn, UniverseId};
use crate::token::Address;

/// A registered Balancer stable pool: the kernel math state + the token addresses
/// aligned with `kernel.balances` / `kernel.scaling_factors` (BPT already excluded).
#[derive(Debug, Clone)]
pub struct BalancerStableSlot {
    /// Math state consumed by `engine_amm::balancer_stable::calc_out_given_in`.
    pub kernel: BalancerStableState,
    /// Token address at each kernel index `0..kernel.n`.
    pub tokens: [Address; MAX_TOKENS],
    /// Balancer `poolId` (bytes32) — the Vault key used to refresh balances.
    pub pool_id: [u8; 32],
}

impl BalancerStableSlot {
    /// Resolve a token address to its kernel coin index.
    #[inline]
    pub fn index_of(&self, token: &Address) -> Option<usize> {
        (0..self.kernel.n).find(|&k| &self.tokens[k] == token)
    }
}

fn placeholder() -> BalancerStableSlot {
    BalancerStableSlot {
        kernel: BalancerStableState {
            n: 0,
            balances: [0u128; MAX_TOKENS],
            scaling_factors: [alloy_primitives::U256::ZERO; MAX_TOKENS],
            amp: 0,
            swap_fee_1e18: 0,
        },
        tokens: [[0u8; 20]; MAX_TOKENS],
        pool_id: [0u8; 32],
    }
}

/// Dense Balancer stable state store. One slot per registered pool address (NOT per
/// pair — pairs sharing a pool share the slot).
pub struct BalancerStateStore {
    slots: Vec<ArcSwap<BalancerStableSlot>>,
    /// WI-06 F1 universe brand (WILDCARD until `UniverseBuilder::new` brands the store).
    universe_id: UniverseId,
}

impl BalancerStateStore {
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
            s.slots.push(ArcSwap::from_pointee(placeholder()));
        }
        s
    }

    /// Append a slot and return its index.
    // WI-06 round 3 (codex item 1 residual): pub(crate) — slot allocation is a pre-seal builder
    // path, never a public post-seal &mut self mutation surface.
    pub(crate) fn allocate(&mut self) -> StateSlot {
        let i = self.slots.len() as u32;
        self.slots.push(ArcSwap::from_pointee(placeholder()));
        StateSlot(i)
    }

    /// Single-writer commit.
    pub fn set(&self, slot: StateSlot, value: BalancerStableSlot, _witness: &StateWriteTxn<'_>) {
        _witness.assert_universe(self.universe_id);
        self.slots[slot.0 as usize].store(Arc::new(value));
    }

    /// Snapshot read.
    pub fn snapshot(&self, slot: StateSlot) -> Arc<BalancerStableSlot> {
        self.slots[slot.0 as usize].load_full()
    }

    /// Zero-allocation hot-path read.
    #[inline]
    pub fn with_snapshot<R>(&self, slot: StateSlot, f: impl FnOnce(&BalancerStableSlot) -> R) -> R {
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
impl Default for BalancerStateStore {
    fn default() -> Self {
        Self::new()
    }
}
