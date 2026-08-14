//! Minimal ERC-20 helpers.

use engine_feed::Address;

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

const DECIMALS_SELECTOR: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

/// Fetch `decimals()` for an ERC-20 token.
pub fn fetch_erc20_decimals(
    client: &ArbitrumRpcClient,
    token: &Address,
    block: BlockTag,
) -> Result<u8, RpcError> {
    let resp = client.eth_call(token, &DECIMALS_SELECTOR, block)?;
    if resp.len() < 32 {
        return Err(RpcError::Decode("decimals() response too short".into()));
    }
    // uint8 in low byte of word.
    Ok(resp[31])
}
