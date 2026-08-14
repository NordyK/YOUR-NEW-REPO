//! `RouteFootprintManifest` (§10.10).
//!
//! Names the exact set of accounts, storage slots, and external call targets a route is
//! allowed to touch during execution. Any access outside this allowlist fails closed and
//! demotes the route to `ReplayOnly`.

use ahash::AHashSet;

use engine_routes::RouteKind;
use engine_state::RouteId;

/// Predicate the executor verifies on a callback.
#[derive(Debug, Clone, Copy)]
pub enum CallbackAuth {
    /// V3-style: `msg.sender ∈ {named pool addresses}`.
    PoolSet,
    /// Balancer V2: `msg.sender == VAULT` plus a signed nonce on `userData`.
    BalancerVault,
}

/// External call target. The allowlist is exhaustive — no other external addresses can be
/// called during validation.
#[derive(Debug, Clone, Copy)]
pub struct ExternalCall {
    /// 20-byte target address.
    pub target: [u8; 20],
    /// First 4 bytes of allowed selector.
    pub selector: [u8; 4],
}

/// Route footprint manifest.
#[derive(Debug, Clone)]
pub struct RouteFootprintManifest {
    /// Dense route id this manifest belongs to.
    pub route_id: RouteId,
    /// Executor route kind.
    pub route_kind: RouteKind,
    /// Allowed account set.
    pub accounts_touched: AHashSet<[u8; 20]>,
    /// Allowed storage slot set: `(address, slot_lsb_4)`.
    pub storage_slots: AHashSet<([u8; 20], u32)>,
    /// Allowed external calls.
    pub external_calls: Vec<ExternalCall>,
    /// Callback authentication checks the executor enforces.
    pub callbacks: Vec<CallbackAuth>,
    /// Stable footprint hash from the last verified dry run.
    pub last_footprint_hash: [u8; 32],
    /// Whether replay parity has been proven over ≥100,000 events.
    pub footprint_verified_in_replay: bool,
}

impl RouteFootprintManifest {
    /// Build an empty manifest. Builders should explicitly add every account + storage slot
    /// expected to be touched.
    pub fn new(route_id: RouteId, route_kind: RouteKind) -> Self {
        Self {
            route_id,
            route_kind,
            accounts_touched: AHashSet::default(),
            storage_slots: AHashSet::default(),
            external_calls: Vec::new(),
            callbacks: Vec::new(),
            last_footprint_hash: [0u8; 32],
            footprint_verified_in_replay: false,
        }
    }

    /// Add an allowed account.
    pub fn allow_account(&mut self, address: [u8; 20]) {
        self.accounts_touched.insert(address);
    }

    /// Add an allowed storage slot.
    pub fn allow_slot(&mut self, address: [u8; 20], slot: u32) {
        self.storage_slots.insert((address, slot));
    }

    /// Check whether an account access is allowed.
    pub fn is_account_allowed(&self, address: &[u8; 20]) -> bool {
        self.accounts_touched.contains(address)
    }

    /// Check whether a storage slot access is allowed.
    pub fn is_slot_allowed(&self, address: &[u8; 20], slot: u32) -> bool {
        self.storage_slots.contains(&(*address, slot))
    }
}
