//! §5.6 codehash verification.
//!
//! For every pool / executor / token address we plan to interact with, the operator
//! records the expected keccak256 of the deployed runtime bytecode in `engine.json`. At
//! startup we fetch the on-chain bytecode via `eth_getCode`, hash it, and compare. A
//! mismatch fails closed (the pool is held at `ResearchOnly` and never enters the live
//! route table).

use tiny_keccak::{Hasher, Keccak};

use engine_feed::Address;

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

/// Fetch the deployed code at `address` and return its keccak256 hash. Empty / nonexistent
/// accounts return the canonical empty-keccak `c5d2…a470` value.
pub fn fetch_pool_code_hash(
    client: &ArbitrumRpcClient,
    address: &Address,
    block: BlockTag,
) -> Result<[u8; 32], RpcError> {
    let code = client.get_code(address, block)?;
    let mut hasher = Keccak::v256();
    hasher.update(&code);
    let mut hash = [0u8; 32];
    hasher.finalize(&mut hash);
    Ok(hash)
}

/// Verify that the deployed code at `address` hashes to `expected`. Returns `Ok(true)` on
/// match, `Ok(false)` on mismatch (with the actual hash logged via `tracing::warn`), or
/// the underlying RPC error.
pub fn verify_pool_code_hash(
    client: &ArbitrumRpcClient,
    address: &Address,
    expected: &[u8; 32],
    block: BlockTag,
) -> Result<bool, RpcError> {
    let actual = fetch_pool_code_hash(client, address, block)?;
    if &actual == expected {
        Ok(true)
    } else {
        tracing::warn!(
            address = ?address,
            actual = ?actual,
            expected = ?expected,
            "code hash mismatch — pool refused live entry"
        );
        Ok(false)
    }
}
