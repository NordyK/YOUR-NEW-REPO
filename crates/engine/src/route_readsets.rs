//! Learned per-route REVM read-sets (P1 of the always-warm plan).
//!
//! `warm_call_prefetch` discovers a route's cold read-set the hard way: up to 8
//! probe→load layers, because exotic families (DODO PMM, LB bins, Balancer vault,
//! Fluid) address storage data-dependently. The slots a route touches are, however,
//! highly stable across validations of the SAME route — so we remember them here and
//! replay them as ONE batched preload next time. Values are always refetched from RPC
//! at the current pin (canonical); only the ADDRESSES are learned. Never a state source.
//!
//! Owned by the validator thread (`revm_loop`) — single-threaded, no locking.

use alloy_primitives::U256;
use engine_state::RouteId;
use std::collections::VecDeque;

/// One route's learned read-set: the `(address, slot)` pairs and accounts its
/// validation touched cold, plus the pin it was last refreshed at.
#[derive(Debug, Default, Clone)]
pub struct ReadSet {
    /// Deduped `(address, slot)` pairs the route's validation reads.
    pub slots: Vec<([u8; 20], U256)>,
    /// Deduped accounts (code/balance/nonce) the route's validation touches.
    pub accounts: Vec<[u8; 20]>,
    /// Pin block this set was last observed/refreshed at.
    pub last_pin: u64,
}

/// Bounded registry of learned read-sets, insertion-order evicted.
pub struct RouteReadSets {
    max_routes: usize,
    max_slots_per_route: usize,
    map: ahash::AHashMap<u32, ReadSet>,
    order: VecDeque<u32>,
}

impl RouteReadSets {
    /// Registry bounded to `max_routes` learned routes × `max_slots_per_route` slots.
    pub fn new(max_routes: usize, max_slots_per_route: usize) -> Self {
        Self {
            max_routes: max_routes.max(1),
            max_slots_per_route: max_slots_per_route.max(1),
            map: ahash::AHashMap::with_capacity(max_routes.min(1024)),
            order: VecDeque::with_capacity(max_routes.min(1024)),
        }
    }

    /// Union `slots`/`accounts` into the route's learned set (deduped, capped) and
    /// stamp the pin it was observed at. Inserting a NEW route beyond capacity evicts
    /// the oldest-inserted route.
    pub fn record(
        &mut self,
        route: RouteId,
        pin: u64,
        slots: &[([u8; 20], U256)],
        accounts: &[[u8; 20]],
    ) {
        if !self.map.contains_key(&route.0) {
            while self.map.len() >= self.max_routes {
                match self.order.pop_front() {
                    Some(oldest) => {
                        self.map.remove(&oldest);
                    }
                    None => break,
                }
            }
            self.order.push_back(route.0);
        }
        let entry = self.map.entry(route.0).or_default();
        entry.last_pin = pin;
        entry.slots.extend_from_slice(slots);
        entry.slots.sort_unstable();
        entry.slots.dedup();
        entry.slots.truncate(self.max_slots_per_route);
        entry.accounts.extend_from_slice(accounts);
        entry.accounts.sort_unstable();
        entry.accounts.dedup();
    }

    /// The learned read-set for `route`, if any.
    pub fn get(&self, route: RouteId) -> Option<&ReadSet> {
        self.map.get(&route.0)
    }

    /// Number of routes currently learned.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when no routes are learned yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(a: u8, s: u64) -> ([u8; 20], U256) {
        ([a; 20], U256::from(s))
    }

    #[test]
    fn record_then_get_roundtrip() {
        let mut r = RouteReadSets::new(4, 8);
        r.record(RouteId(7), 100, &[slot(1, 5), slot(1, 7)], &[[0xEE; 20]]);
        let rs = r.get(RouteId(7)).expect("recorded route must be present");
        assert_eq!(rs.slots, vec![slot(1, 5), slot(1, 7)]);
        assert_eq!(rs.accounts, vec![[0xEE; 20]]);
        assert_eq!(rs.last_pin, 100);
        assert!(r.get(RouteId(8)).is_none(), "unknown route must be None");
    }

    #[test]
    fn record_twice_unions_without_dupes() {
        let mut r = RouteReadSets::new(4, 8);
        r.record(RouteId(1), 100, &[slot(1, 5)], &[[0xEE; 20]]);
        r.record(RouteId(1), 101, &[slot(1, 5), slot(1, 9)], &[[0xEE; 20], [0xDD; 20]]);
        let rs = r.get(RouteId(1)).unwrap();
        assert_eq!(rs.slots.len(), 2, "union must dedup slots: {:?}", rs.slots);
        assert!(rs.slots.contains(&slot(1, 5)) && rs.slots.contains(&slot(1, 9)));
        assert_eq!(rs.accounts.len(), 2, "union must dedup accounts");
        assert_eq!(rs.last_pin, 101, "last_pin follows the latest record");
    }

    #[test]
    fn slot_cap_is_enforced() {
        let mut r = RouteReadSets::new(4, 3);
        let many: Vec<_> = (0..10u64).map(|s| slot(1, s)).collect();
        r.record(RouteId(1), 100, &many, &[]);
        assert_eq!(
            r.get(RouteId(1)).unwrap().slots.len(),
            3,
            "per-route slot cap must hold"
        );
    }

    #[test]
    fn route_cap_evicts_oldest() {
        let mut r = RouteReadSets::new(2, 8);
        r.record(RouteId(1), 100, &[slot(1, 1)], &[]);
        r.record(RouteId(2), 100, &[slot(2, 1)], &[]);
        r.record(RouteId(3), 100, &[slot(3, 1)], &[]);
        assert_eq!(r.len(), 2, "capacity must hold");
        assert!(r.get(RouteId(1)).is_none(), "oldest route evicted");
        assert!(r.get(RouteId(2)).is_some() && r.get(RouteId(3)).is_some());
    }

    #[test]
    fn updating_existing_route_does_not_evict() {
        let mut r = RouteReadSets::new(2, 8);
        r.record(RouteId(1), 100, &[slot(1, 1)], &[]);
        r.record(RouteId(2), 100, &[slot(2, 1)], &[]);
        r.record(RouteId(1), 101, &[slot(1, 2)], &[]);
        assert_eq!(r.len(), 2);
        assert!(r.get(RouteId(1)).is_some() && r.get(RouteId(2)).is_some());
    }
}
