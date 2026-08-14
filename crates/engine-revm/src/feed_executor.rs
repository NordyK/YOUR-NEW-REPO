//! §13 EventReady-(b) feed executor — the node-free, warm-cache layer that turns a
//! sequencer-feed transaction into "which of our resident pools changed".
//!
//! Wraps [`RevmValidator`] (a real `revm` over a warm `CacheDB`) with:
//! * a **resident-pool registry** (`address -> opaque pool id`) so callers learn which
//!   of *their* pools a tx mutated (and can emit dirty events for exactly those),
//! * **pre-warming** of pool/token/router code + storage (so reads hit the cache, not a
//!   node — Alchemy is touched only at snapshot, never on the hot path),
//! * a **sender override** so re-executing an arbitrary third-party tx never reverts on
//!   gas funds.
//!
//! Generic over the pool-id type (`u32` in the engine = `PoolId.0`) so this crate stays
//! free of `engine-state`. The pipeline calls [`FeedExecutor::apply_feed_tx`] for each
//! feed tx and routes the returned ids into the dirty queue.

use ahash::AHashMap;
use alloy_primitives::U256;

use crate::revm_validator::{RevmError, RevmValidator};

/// Default gas ceiling for re-executing a feed tx locally (generous; we only care that
/// it doesn't run out, not the exact metered cost — that's modeled separately).
const FEED_GAS_LIMIT: u64 = 30_000_000;

/// Feed-execution layer over a warm REVM DB.
pub struct FeedExecutor {
    validator: RevmValidator,
    /// Resident pool address -> caller's opaque id (e.g. `PoolId.0`).
    resident: AHashMap<[u8; 20], u32>,
}

impl FeedExecutor {
    /// Empty executor. Pre-warm with `register_pool` / `preload_contract` after snapshot.
    pub fn new() -> Self {
        Self {
            validator: RevmValidator::new(),
            resident: AHashMap::new(),
        }
    }

    /// Register a resident pool: record its id and pre-warm its code + storage slots so
    /// REVM can execute swaps against it without any RPC read.
    pub fn register_pool(
        &mut self,
        address: [u8; 20],
        pool_id: u32,
        code: Option<Vec<u8>>,
        slots: &[([u8; 32], [u8; 32])],
    ) -> Result<(), RevmError> {
        self.validator.preload_account(address, U256::ZERO, 0, code);
        for (slot, value) in slots {
            self.validator.preload_storage(
                address,
                U256::from_be_bytes(*slot),
                U256::from_be_bytes(*value),
            )?;
        }
        self.resident.insert(address, pool_id);
        Ok(())
    }

    /// Pre-warm a non-pool contract (token / router / library) the swap paths call.
    pub fn preload_contract(
        &mut self,
        address: [u8; 20],
        code: Option<Vec<u8>>,
        slots: &[([u8; 32], [u8; 32])],
    ) -> Result<(), RevmError> {
        self.validator.preload_account(address, U256::ZERO, 0, code);
        for (slot, value) in slots {
            self.validator.preload_storage(
                address,
                U256::from_be_bytes(*slot),
                U256::from_be_bytes(*value),
            )?;
        }
        Ok(())
    }

    /// Re-execute a sequencer-feed tx against the warm DB and return the **resident pool
    /// ids that changed**. The diff is committed (the warm cache self-updates), and the
    /// sender is funded with gas so an arbitrary third-party tx doesn't revert. Returns
    /// an empty vec on revert / no resident pool touched.
    pub fn apply_feed_tx(
        &mut self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        calldata: Vec<u8>,
    ) -> Vec<u32> {
        // Fund the sender for gas so re-execution never reverts on balance.
        self.validator
            .preload_account(from, U256::MAX >> 1, 0, None);
        match self.validator.execute_feed_tx(
            from,
            to,
            value,
            calldata,
            FEED_GAS_LIMIT,
            U256::from(1u64),
        ) {
            Ok((res, changed)) if res.success => changed
                .iter()
                .filter_map(|a| self.resident.get(a).copied())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Number of resident pools registered.
    pub fn resident_count(&self) -> usize {
        self.resident.len()
    }

    /// Read a resident pool's storage slot from the warm DB (post-execution state).
    pub fn pool_storage(&mut self, address: [u8; 20], slot: [u8; 32]) -> Result<U256, RevmError> {
        self.validator.storage(address, U256::from_be_bytes(slot))
    }
}

impl Default for FeedExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resident "pool" whose bytecode SSTOREs calldata[0..32] into slot 0. A feed tx
    /// hitting it must come back as that pool's id, and the warm state must reflect the
    /// write — the end-to-end feed→resident-pool-diff path.
    #[test]
    fn feed_tx_maps_to_resident_pool_and_updates_state() {
        let bytecode = vec![0x60, 0x00, 0x35, 0x60, 0x00, 0x55, 0x00];
        let mut fx = FeedExecutor::new();
        fx.register_pool([0xee; 20], 7, Some(bytecode), &[])
            .unwrap();
        assert_eq!(fx.resident_count(), 1);

        let mut calldata = vec![0u8; 32];
        calldata[31] = 99;
        let changed = fx.apply_feed_tx([0x11; 20], [0xee; 20], U256::ZERO, calldata);
        assert_eq!(changed, vec![7], "feed tx must map to resident pool id 7");
        // warm DB reflects the post-tx state (self-feeding)
        assert_eq!(
            fx.pool_storage([0xee; 20], [0u8; 32]).unwrap(),
            U256::from(99u64)
        );
    }

    /// A tx touching a NON-resident contract returns no pool ids (we skip it — it's not
    /// in our universe), without error.
    #[test]
    fn non_resident_tx_returns_empty() {
        let bytecode = vec![0x60, 0x00, 0x35, 0x60, 0x00, 0x55, 0x00];
        let mut fx = FeedExecutor::new();
        // contract exists in the warm DB but is NOT registered as a resident pool
        fx.preload_contract([0xab; 20], Some(bytecode), &[])
            .unwrap();
        let mut calldata = vec![0u8; 32];
        calldata[31] = 1;
        let changed = fx.apply_feed_tx([0x11; 20], [0xab; 20], U256::ZERO, calldata);
        assert!(
            changed.is_empty(),
            "non-resident change must not emit a pool id"
        );
    }
}
