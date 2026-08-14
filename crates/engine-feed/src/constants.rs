//! Canonical Arbitrum One constants.
//!
//! The hot path consumes these as compile-time constants; the local relay's upstream URL
//! is informational here because the engine itself never reaches out to the public feed.

/// Arbitrum One chain id.
pub const ARB_ONE_CHAIN_ID: u64 = 42_161;

/// Arbitrum Nitro genesis L2 block number — L2 block = `sequence_number + GENESIS`.
pub const NITRO_GENESIS_BLOCK_NUMBER: u64 = 22_207_817;

/// Canonical public Arbitrum One sequencer feed endpoint. The engine never connects here
/// directly — the local feed relay does.
pub const ARB_ONE_FEED_WSS: &str = "wss://arb1-feed.arbitrum.io/feed";

/// Arbitrum One sequencer endpoint (only `eth_sendRawTransaction` and
/// `eth_sendRawTransactionConditional` are accepted).
pub const ARB_ONE_SEQUENCER_RPC: &str = "https://arb1-sequencer.arbitrum.io/rpc";

/// Balancer V2 Vault address (same on every chain), used for flash loans.
pub const BALANCER_VAULT_ARBITRUM: [u8; 20] = [
    0xBA, 0x12, 0x22, 0x22, 0x22, 0x22, 0x8d, 0x8B, 0xa4, 0x45, 0x95, 0x8a, 0x75, 0xa0, 0x70, 0x4d,
    0x56, 0x6B, 0xF2, 0xC8,
];

/// Convert a sequence number to its corresponding L2 block.
#[inline]
pub fn l2_block_from_seq(seq: u64) -> u64 {
    seq + NITRO_GENESIS_BLOCK_NUMBER
}
