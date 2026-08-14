//! Tx classifier with a dense `(to_id, selector) -> action` jump table built at startup.

use ahash::AHashMap;
use engine_state::{AddressRegistry, PoolId};

use crate::selectors::{SelectorClass, SelectorRegistry};

/// Output of the classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxClass {
    /// Drop.
    Ignored,
    /// Direct call to a known pool whose calldata is predictable.
    KnownPoolSwap(PoolId),
    /// Known router calldata; route info should be decoded.
    KnownRouterSwap {
        /// Classification of the router's selector.
        router_class: SelectorClass,
    },
    /// Aggregator/router hits a hot pool; route to local REVM for state update.
    NeedsRevm,
    /// One of our own EOAs.
    OwnTx,
}

/// Classifier with pre-built lookup tables.
pub struct Classifier {
    pool_addresses: AHashMap<[u8; 20], PoolId>,
    router_addresses: AHashMap<[u8; 20], u16>,
    selectors: SelectorRegistry,
    own_eoas: Vec<[u8; 20]>,
}

impl Classifier {
    /// Classify a transaction by `(to, selector_first_4_bytes)`.
    pub fn classify(&self, to: &[u8; 20], calldata: &[u8]) -> TxClass {
        if self.own_eoas.iter().any(|e| e == to) {
            return TxClass::OwnTx;
        }
        if let Some(&pool) = self.pool_addresses.get(to) {
            return TxClass::KnownPoolSwap(pool);
        }
        if calldata.len() >= 4 {
            let mut sel = [0u8; 4];
            sel.copy_from_slice(&calldata[..4]);
            if let Some(class) = self.selectors.lookup(&sel) {
                if self.router_addresses.contains_key(to) {
                    return TxClass::KnownRouterSwap {
                        router_class: class,
                    };
                }
                // Selector is known but `to` is not in our router registry — route through
                // REVM so the warm DB picks up any pool state changes.
                return TxClass::NeedsRevm;
            }
        }
        TxClass::Ignored
    }
}

/// Builder for `Classifier`.
pub struct ClassifierBuilder {
    pool_addresses: AHashMap<[u8; 20], PoolId>,
    router_addresses: AHashMap<[u8; 20], u16>,
    own_eoas: Vec<[u8; 20]>,
}

impl ClassifierBuilder {
    /// Empty builder.
    pub fn new() -> Self {
        Self {
            pool_addresses: AHashMap::default(),
            router_addresses: AHashMap::default(),
            own_eoas: Vec::new(),
        }
    }

    /// Add a pool address.
    pub fn add_pool(&mut self, address: [u8; 20], id: PoolId) -> &mut Self {
        self.pool_addresses.insert(address, id);
        self
    }

    /// Add a router address.
    pub fn add_router(&mut self, address: [u8; 20], id: u16) -> &mut Self {
        self.router_addresses.insert(address, id);
        self
    }

    /// Add an own EOA.
    pub fn add_own_eoa(&mut self, address: [u8; 20]) -> &mut Self {
        self.own_eoas.push(address);
        self
    }

    /// Seed from an `AddressRegistry`.
    pub fn from_registry(reg: &AddressRegistry) -> Self {
        let mut b = Self::new();
        for (addr, id) in reg.pools() {
            b.add_pool(*addr, *id);
        }
        b
    }

    /// Build.
    pub fn build(self) -> Classifier {
        Classifier {
            pool_addresses: self.pool_addresses,
            router_addresses: self.router_addresses,
            selectors: SelectorRegistry::canonical(),
            own_eoas: self.own_eoas,
        }
    }
}

impl Default for ClassifierBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_pool_swap() {
        let mut b = ClassifierBuilder::new();
        b.add_pool([7u8; 20], PoolId(0));
        let c = b.build();
        assert_eq!(
            c.classify(&[7u8; 20], &[]),
            TxClass::KnownPoolSwap(PoolId(0))
        );
    }

    #[test]
    fn known_router_classification() {
        let mut b = ClassifierBuilder::new();
        b.add_router([9u8; 20], 1);
        let c = b.build();
        let mut data = vec![0u8; 8];
        data[..4].copy_from_slice(&[0xc0, 0x4b, 0x8d, 0x59]);
        let tc = c.classify(&[9u8; 20], &data);
        assert!(matches!(tc, TxClass::KnownRouterSwap { .. }));
    }

    #[test]
    fn unknown_to_with_known_selector_is_revm() {
        let b = ClassifierBuilder::new();
        let c = b.build();
        let mut data = vec![0u8; 8];
        data[..4].copy_from_slice(&[0xc0, 0x4b, 0x8d, 0x59]);
        assert_eq!(c.classify(&[1u8; 20], &data), TxClass::NeedsRevm);
    }

    #[test]
    fn own_tx_priority() {
        let mut b = ClassifierBuilder::new();
        b.add_own_eoa([5u8; 20]);
        b.add_pool([5u8; 20], PoolId(0));
        let c = b.build();
        assert_eq!(c.classify(&[5u8; 20], &[]), TxClass::OwnTx);
    }
}
