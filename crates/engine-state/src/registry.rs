//! Address → dense-id registry. Built once at startup, then immutable for the hot path.

use ahash::AHashMap;
use thiserror::Error;

use crate::ids::{PoolId, TokenId};
use crate::token::Address;

/// Registry build errors.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// Duplicate address insert.
    #[error("duplicate address")]
    DuplicateAddress,
    /// Caller tried to register a pool whose `PoolKind` has no matching off-chain quote curve
    /// implementation in `engine-amm`. The pool would silently fail to be quoted and could be
    /// included in a route the validator would later reject — better to refuse at registration.
    /// As of 2026-05 this applies to `PoolKind::CurveCryptoSwap` (TwoCrypto-NG / Tricrypto-NG):
    /// the executor has an exchangeId branch and the snapshot decoder exists, but the AMM curve
    /// is not implemented (Dune 30d Arbitrum DEX volume puts Curve outside the top 50, so the
    /// math was deprioritized; if Arbitrum-side Curve liquidity materializes, port the Newton
    /// kernels from the Shadow/Fulcrum reference implementations and remove this guard).
    #[error("pool kind {0:?} has no off-chain quote implementation; rejected at registration")]
    UnsupportedKind(crate::pool::PoolKind),
}

/// Read-only registry mapping pool addresses to dense ids and pool addresses to a per-pool
/// dense id. Sized for ~hundreds of pools so AHash is well within budget for startup; the
/// hot path uses dense `PoolId` lookups instead.
pub struct AddressRegistry {
    pool_by_address: AHashMap<Address, PoolId>,
    routers: AHashMap<Address, u16>,
    token_by_address: AHashMap<Address, TokenId>,
    /// CSR-style direct lookup `(min(token0_addr), max(token0_addr)) → [pool_id]`. The
    /// engine canonicalises the pair lexicographically before insertion so callers
    /// regardless of token0/token1 ordering resolve identically.
    pools_by_pair: AHashMap<(Address, Address), Vec<PoolId>>,
}

impl AddressRegistry {
    /// Look up a pool by its 20-byte address.
    #[inline]
    pub fn pool_of(&self, address: &Address) -> Option<PoolId> {
        self.pool_by_address.get(address).copied()
    }

    /// Look up a router classifier id.
    #[inline]
    pub fn router_of(&self, address: &Address) -> Option<u16> {
        self.routers.get(address).copied()
    }

    /// Iterate all pool entries (cold-path only).
    pub fn pools(&self) -> impl Iterator<Item = (&Address, &PoolId)> {
        self.pool_by_address.iter()
    }

    /// Look up a token by its 20-byte address.
    #[inline]
    pub fn token_of(&self, address: &Address) -> Option<TokenId> {
        self.token_by_address.get(address).copied()
    }

    /// Look up every pool registered for the unordered `(token_a, token_b)` pair.
    #[inline]
    pub fn pools_for_pair(&self, token_a: &Address, token_b: &Address) -> &[PoolId] {
        let key = canonical_pair(token_a, token_b);
        self.pools_by_pair
            .get(&key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Count of pools indexed.
    pub fn len(&self) -> usize {
        self.pool_by_address.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.pool_by_address.is_empty()
    }
}

/// Builder for `AddressRegistry`. Use during startup only.
pub struct RegistryBuilder {
    pool_by_address: AHashMap<Address, PoolId>,
    routers: AHashMap<Address, u16>,
    token_by_address: AHashMap<Address, TokenId>,
    pools_by_pair: AHashMap<(Address, Address), Vec<PoolId>>,
}

impl RegistryBuilder {
    /// Empty builder.
    pub fn new() -> Self {
        Self {
            pool_by_address: AHashMap::default(),
            routers: AHashMap::default(),
            token_by_address: AHashMap::default(),
            pools_by_pair: AHashMap::default(),
        }
    }

    /// Register a token address → id mapping.
    pub fn register_token(&mut self, address: Address, id: TokenId) -> Result<(), RegistryError> {
        if self.token_by_address.insert(address, id).is_some() {
            return Err(RegistryError::DuplicateAddress);
        }
        Ok(())
    }

    /// Register a pool's pair index: `(token_a_addr, token_b_addr) → pool_id`. Canonicalised
    /// lexicographically so callers don't need to know which side is token0.
    pub fn register_pair(&mut self, token_a: Address, token_b: Address, pool: PoolId) {
        let key = canonical_pair(&token_a, &token_b);
        self.pools_by_pair.entry(key).or_default().push(pool);
    }

    /// Register a pool address → id mapping. Fail-closed on a duplicate WITHOUT overwriting the
    /// existing mapping (uses `entry`, not `insert`): a second registration for the same address
    /// keeps the first id and returns `DuplicateAddress`, so a caller that ignores the error can
    /// never silently corrupt the 1:1 address→id map. (See `add_balancer_stable_pool`, which checks
    /// `pool_of` first and reuses the existing pool for a multi-token pool's sibling pairs.)
    pub fn register_pool(&mut self, address: Address, id: PoolId) -> Result<(), RegistryError> {
        use std::collections::hash_map::Entry;
        match self.pool_by_address.entry(address) {
            Entry::Occupied(_) => Err(RegistryError::DuplicateAddress),
            Entry::Vacant(v) => {
                v.insert(id);
                Ok(())
            }
        }
    }

    /// Look up an already-registered pool by address (startup builder only). Used to enforce one
    /// internal pool per Balancer Vault address before allocating a second state slot.
    pub fn pool_of(&self, address: &Address) -> Option<PoolId> {
        self.pool_by_address.get(address).copied()
    }

    /// Register a router address → classifier id.
    pub fn register_router(&mut self, address: Address, id: u16) -> Result<(), RegistryError> {
        if self.routers.insert(address, id).is_some() {
            return Err(RegistryError::DuplicateAddress);
        }
        Ok(())
    }

    /// Freeze into an immutable registry.
    pub fn build(self) -> AddressRegistry {
        AddressRegistry {
            pool_by_address: self.pool_by_address,
            routers: self.routers,
            token_by_address: self.token_by_address,
            pools_by_pair: self.pools_by_pair,
        }
    }
}

impl Default for RegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn canonical_pair(a: &Address, b: &Address) -> (Address, Address) {
    if a <= b {
        (*a, *b)
    } else {
        (*b, *a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let mut b = RegistryBuilder::new();
        b.register_pool([7u8; 20], PoolId(0)).unwrap();
        b.register_router([8u8; 20], 42).unwrap();
        let r = b.build();
        assert_eq!(r.pool_of(&[7u8; 20]), Some(PoolId(0)));
        assert_eq!(r.pool_of(&[9u8; 20]), None);
        assert_eq!(r.router_of(&[8u8; 20]), Some(42));
    }

    #[test]
    fn duplicate_pool_errors() {
        let mut b = RegistryBuilder::new();
        b.register_pool([1u8; 20], PoolId(0)).unwrap();
        let err = b.register_pool([1u8; 20], PoolId(1));
        assert!(err.is_err());
    }
}
