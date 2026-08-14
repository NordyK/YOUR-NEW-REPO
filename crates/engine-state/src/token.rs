//! Token table: dense SoA storage indexed by `TokenId`.

use alloy_primitives::U256;

use crate::ids::TokenId;

/// Address bytes (20).
pub type Address = [u8; 20];

/// Risk classification of a token. Used to gate which routes admit a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskClass {
    /// Permissionless ERC-20 with no admin upgrades or freeze hooks.
    Permissionless,
    /// Centralised stablecoin (USDC, USDT-style). Allowed under the constrained class.
    CentralisedStable,
    /// Excluded outright (FoT, rebasing, honeypot).
    Excluded,
}

/// Metadata for a token slot, queried at startup and immutable thereafter.
#[derive(Debug, Clone, Copy)]
pub struct TokenMeta {
    /// 20-byte address.
    pub address: Address,
    /// ERC-20 `decimals` value.
    pub decimals: u8,
    /// Risk classification (see §5.1).
    pub risk_class: RiskClass,
    /// `10^decimals` as U256, precomputed so the hot path never pow().
    pub scale: U256,
    /// ETH value of ONE smallest unit of this token, as a Q96 fixed-point
    /// (`wei_per_unit * 2^96`). Lets the hot path convert a base-token raw amount
    /// to ETH-wei via `(amount * q96) >> 96` without floats. Profit accounting is
    /// denominated in ETH-wei; a cycle funded in any base token must convert its
    /// gross with this factor — otherwise a non-WETH base (ARB, USDC, …) is mis-valued
    /// because raw token units are NOT 1:1 with wei. `ZERO` means "unpriced" → the
    /// evaluator fails closed rather than mis-value the cycle.
    pub eth_wei_per_unit_q96: U256,
}

impl TokenMeta {
    /// Construct a token entry with precomputed scale. `eth_wei_per_unit_q96`
    /// defaults to `ZERO` (unpriced) — set it at startup via
    /// [`TokenTable::set_eth_wei_per_unit_q96`].
    pub fn new(address: Address, decimals: u8, risk_class: RiskClass) -> Self {
        // AUD-TOKEN-DECIMALS: `decimals` is a `u8` (≤255) read from an untrusted `decimals()` call.
        // `10^decimals` overflows U256 for `decimals ≥ 78` (10^78 > 2^256), which would WRAP to a
        // garbage scale under `pow`. Saturate to U256::MAX instead — no real ERC-20 exceeds 18
        // decimals (and such an absurd token is rejected upstream by the risk-class allowlist), so
        // this is byte-identical for every real token and merely fail-safe on hostile input.
        let scale = U256::from(10u64)
            .checked_pow(U256::from(decimals))
            .unwrap_or(U256::MAX);
        Self {
            address,
            decimals,
            risk_class,
            scale,
            eth_wei_per_unit_q96: U256::ZERO,
        }
    }
}

/// Dense SoA token table.
pub struct TokenTable {
    addresses: Vec<Address>,
    decimals: Vec<u8>,
    risk_class: Vec<RiskClass>,
    scale: Vec<U256>,
    eth_wei_per_unit_q96: Vec<U256>,
}

impl TokenTable {
    /// Empty table.
    pub fn new() -> Self {
        Self {
            addresses: Vec::new(),
            decimals: Vec::new(),
            risk_class: Vec::new(),
            scale: Vec::new(),
            eth_wei_per_unit_q96: Vec::new(),
        }
    }

    /// Insert a token, returning its dense id.
    pub fn insert(&mut self, meta: TokenMeta) -> TokenId {
        let id = TokenId(self.addresses.len() as u16);
        self.addresses.push(meta.address);
        self.decimals.push(meta.decimals);
        self.risk_class.push(meta.risk_class);
        self.scale.push(meta.scale);
        self.eth_wei_per_unit_q96.push(meta.eth_wei_per_unit_q96);
        id
    }

    /// Read meta by id.
    pub fn get(&self, id: TokenId) -> TokenMeta {
        let i = id.0 as usize;
        TokenMeta {
            address: self.addresses[i],
            decimals: self.decimals[i],
            risk_class: self.risk_class[i],
            scale: self.scale[i],
            eth_wei_per_unit_q96: self.eth_wei_per_unit_q96[i],
        }
    }

    /// Set the Q96 ETH-per-unit factor for a token (startup price wiring).
    pub fn set_eth_wei_per_unit_q96(&mut self, id: TokenId, q96: U256) {
        self.eth_wei_per_unit_q96[id.0 as usize] = q96;
    }

    /// Q96 ETH-per-unit factor; `ZERO` if the token was never priced.
    pub fn eth_wei_per_unit_q96(&self, id: TokenId) -> U256 {
        self.eth_wei_per_unit_q96[id.0 as usize]
    }

    /// Number of tokens in the table.
    pub fn len(&self) -> usize {
        self.addresses.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty()
    }

    /// Direct slice access (for cache-line walking when scoring).
    pub fn risk_classes(&self) -> &[RiskClass] {
        &self.risk_class
    }

    /// Linear address lookup. Used during startup wiring only; hot path uses the
    /// `AddressRegistry`.
    pub fn position(&self, address: &Address) -> Option<TokenId> {
        self.addresses
            .iter()
            .position(|a| a == address)
            .map(|i| TokenId(i as u16))
    }
}

impl Default for TokenTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let mut t = TokenTable::new();
        let id = t.insert(TokenMeta::new([1u8; 20], 18, RiskClass::Permissionless));
        assert_eq!(id, TokenId(0));
        let m = t.get(id);
        assert_eq!(m.decimals, 18);
        assert_eq!(m.scale, U256::from(10u64).pow(U256::from(18)));
    }

    #[test]
    fn position_returns_id() {
        let mut t = TokenTable::new();
        t.insert(TokenMeta::new([1u8; 20], 6, RiskClass::CentralisedStable));
        t.insert(TokenMeta::new([2u8; 20], 18, RiskClass::Permissionless));
        assert_eq!(t.position(&[2u8; 20]), Some(TokenId(1)));
        assert_eq!(t.position(&[3u8; 20]), None);
    }
}

#[cfg(test)]
mod decimals_overflow_tests {
    //! AUD-TOKEN-DECIMALS: absurd `decimals` must saturate the scale, never wrap/panic.
    use super::*;
    #[test]
    fn token_meta_saturates_absurd_decimals() {
        let t = TokenMeta::new([0u8; 20], 200, RiskClass::Excluded);
        assert_eq!(t.scale, U256::MAX, "decimals>=78 must saturate to U256::MAX");
    }
    #[test]
    fn token_meta_exact_for_real_decimals() {
        assert_eq!(TokenMeta::new([0u8; 20], 6, RiskClass::CentralisedStable).scale, U256::from(1_000_000u64));
        assert_eq!(TokenMeta::new([0u8; 20], 18, RiskClass::Permissionless).scale, U256::from(1_000_000_000_000_000_000u64));
    }
}
