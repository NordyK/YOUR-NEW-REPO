//! Warm in-memory DB the harness reads from. Holds ERC-20 balances/allowances for tokens
//! the engine cares about, plus pool storage slots.

use ahash::AHashMap;
use alloy_primitives::U256;

/// Per-token balance and per-spender allowance for an account.
#[derive(Debug, Clone, Default)]
pub struct TokenBalance {
    /// Balance per (token, account).
    pub balance: U256,
    /// Allowance per (token, owner, spender).
    pub allowances: AHashMap<[u8; 20], U256>,
}

/// Warm DB.
#[derive(Debug, Default)]
pub struct WarmDb {
    /// Balances keyed by `(token_address, holder_address)`.
    balances: AHashMap<([u8; 20], [u8; 20]), U256>,
    /// Allowances keyed by `(token, owner, spender)`.
    allowances: AHashMap<([u8; 20], [u8; 20], [u8; 20]), U256>,
}

impl WarmDb {
    /// Empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a balance.
    pub fn balance_of(&self, token: &[u8; 20], holder: &[u8; 20]) -> U256 {
        *self.balances.get(&(*token, *holder)).unwrap_or(&U256::ZERO)
    }

    /// Set a balance.
    pub fn set_balance(&mut self, token: [u8; 20], holder: [u8; 20], amount: U256) {
        self.balances.insert((token, holder), amount);
    }

    /// Transfer tokens (debit `from`, credit `to`). Returns `false` if `from` has insufficient
    /// balance — the harness rejects such transitions because it indicates the route would
    /// revert on chain.
    pub fn transfer(
        &mut self,
        token: &[u8; 20],
        from: &[u8; 20],
        to: &[u8; 20],
        amount: U256,
    ) -> bool {
        let fb = self.balance_of(token, from);
        if fb < amount {
            return false;
        }
        self.balances.insert((*token, *from), fb - amount);
        let tb = self.balance_of(token, to);
        self.balances.insert((*token, *to), tb + amount);
        true
    }

    /// Read an allowance.
    pub fn allowance_of(&self, token: &[u8; 20], owner: &[u8; 20], spender: &[u8; 20]) -> U256 {
        *self
            .allowances
            .get(&(*token, *owner, *spender))
            .unwrap_or(&U256::ZERO)
    }

    /// Set an allowance.
    pub fn set_allowance(
        &mut self,
        token: [u8; 20],
        owner: [u8; 20],
        spender: [u8; 20],
        amount: U256,
    ) {
        self.allowances.insert((token, owner, spender), amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_set_and_get() {
        let mut db = WarmDb::new();
        db.set_balance([1u8; 20], [2u8; 20], U256::from(100u64));
        assert_eq!(db.balance_of(&[1u8; 20], &[2u8; 20]), U256::from(100u64));
    }

    #[test]
    fn transfer_succeeds() {
        let mut db = WarmDb::new();
        db.set_balance([1u8; 20], [2u8; 20], U256::from(100u64));
        assert!(db.transfer(&[1u8; 20], &[2u8; 20], &[3u8; 20], U256::from(30u64)));
        assert_eq!(db.balance_of(&[1u8; 20], &[2u8; 20]), U256::from(70u64));
        assert_eq!(db.balance_of(&[1u8; 20], &[3u8; 20]), U256::from(30u64));
    }

    #[test]
    fn transfer_fails_on_underflow() {
        let mut db = WarmDb::new();
        db.set_balance([1u8; 20], [2u8; 20], U256::from(10u64));
        assert!(!db.transfer(&[1u8; 20], &[2u8; 20], &[3u8; 20], U256::from(11u64)));
    }
}
