//! Common types for the discovery pipeline.

use engine_state::PoolKind;

/// 20-byte EVM address (alias of `engine_feed::Address` so callers can use either).
pub type Address = [u8; 20];

/// One pool emitted by a factory's `PoolCreated` / `PairCreated` event after passing the
/// token-allowlist filter. Subsequent gates (validation, snapshot, score) operate on a
/// vector of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredPool {
    /// Pool / pair address.
    pub address: Address,
    /// AMM family.
    pub family: PoolKind,
    /// Lower-address token (per Uniswap-style ordering).
    pub token0: Address,
    /// Higher-address token.
    pub token1: Address,
    /// V3-style fee in pips (10⁻⁶); 0 for V2.
    pub fee_pips: u32,
    /// V2-style fee in bps (10⁻⁴); 0 for V3.
    pub fee_bps: u32,
    /// V3 tick spacing; 0 for V2.
    pub tick_spacing: i32,
    /// Block at which the factory emitted the create event.
    pub created_block: u64,
}

/// Discovery error surface.
#[derive(thiserror::Error, Debug)]
pub enum DiscoveryError {
    /// Underlying RPC failure.
    #[error("rpc: {0}")]
    Rpc(#[from] engine_rpc::RpcError),
    /// Topic / data field had unexpected length.
    #[error("malformed log: {0}")]
    MalformedLog(&'static str),
    /// Block range chunk was zero (programmer error).
    #[error("block range chunk must be > 0")]
    ZeroChunk,
    /// Hex parse failure on a topic/data field.
    #[error("hex parse: {0}")]
    Hex(String),
}
