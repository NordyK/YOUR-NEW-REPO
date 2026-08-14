//! Curve **TwoCrypto-NG** (2-coin) per-pool state snapshot reader.
//!
//! The 2-coin sibling of [`crate::cryptoswap_snapshot`]. The 3-coin Tricrypto fetcher rejects
//! pools whose `coins(2)` reverts; this reader handles exactly those 2-coin pools, assembling
//! `engine_amm::curve_twocrypto::TwoCryptoSwapState`.
//!
//! View ABI (Curve twocrypto-ng `Twocrypto.vy`):
//! ```text
//!   coins(uint256 i)    -> address   (i in 0..2)
//!   balances(uint256 i) -> uint256
//!   A()                 -> uint256   (ANN-form)
//!   gamma()             -> uint256
//!   mid_fee()/out_fee()/fee_gamma() -> uint256
//!   price_scale()       -> uint256   (single value: coin1/coin0, 1e18-scaled — NOT indexed)
//!   D()                 -> uint256
//! ```
//! `precisions[i] = 10^(18 - decimals(coins[i]))`.

use alloy_primitives::U256;
use engine_amm::curve_twocrypto::TwoCryptoSwapState;
use engine_feed::Address;
use tiny_keccak::{Hasher, Keccak};

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

fn selector(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

fn indexed_call(sel: [u8; 4], idx: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(36);
    data.extend_from_slice(&sel);
    let mut word = [0u8; 32];
    word[24..32].copy_from_slice(&(idx as u64).to_be_bytes());
    data.extend_from_slice(&word);
    data
}

fn decode_word(resp: &[u8]) -> Result<U256, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode("twocrypto word short".into()));
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&resp[0..32]);
    Ok(U256::from_be_bytes(b))
}

fn decode_address(resp: &[u8]) -> Result<Address, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode("twocrypto address short".into()));
    }
    if resp[0..12].iter().any(|&b| b != 0) {
        return Err(RpcError::Decode("twocrypto address padding".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Ok(a)
}

fn precision_for_decimals(decimals: u8) -> U256 {
    let exp = 18u32.saturating_sub(decimals as u32);
    let mut p = U256::from(1u64);
    for _ in 0..exp {
        p *= U256::from(10u64);
    }
    p
}

/// Fetch a 2-coin Curve TwoCrypto-NG pool state at `block`.
pub fn fetch_twocrypto_state(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<(TwoCryptoSwapState, [[u8; 20]; 2]), RpcError> {
    let coins_sel = selector(b"coins(uint256)");
    let balances_sel = selector(b"balances(uint256)");
    let decimals_sel = selector(b"decimals()");

    let mut coins = [[0u8; 20]; 2];
    for i in 0..2 {
        let resp = client.eth_call(pool, &indexed_call(coins_sel, i), block)?;
        let addr = decode_address(&resp)?;
        if addr == [0u8; 20] {
            return Err(RpcError::Decode("twocrypto coin is zero".into()));
        }
        coins[i] = addr;
    }

    let mut balances = [U256::ZERO; 2];
    let mut precisions = [U256::ZERO; 2];
    for i in 0..2 {
        balances[i] =
            decode_word(&client.eth_call(pool, &indexed_call(balances_sel, i), block)?)?;
        let dec_u = decode_word(&client.eth_call(&coins[i], decimals_sel.as_ref(), block)?)?;
        let dec: u8 = dec_u
            .try_into()
            .map_err(|_| RpcError::Decode("twocrypto decimals > u8".into()))?;
        if dec > 18 {
            return Err(RpcError::Decode("twocrypto coin decimals > 18".into()));
        }
        precisions[i] = precision_for_decimals(dec);
    }

    let ann = decode_word(&client.eth_call(pool, &selector(b"A()"), block)?)?;
    let gamma = decode_word(&client.eth_call(pool, &selector(b"gamma()"), block)?)?;
    let mid_fee = decode_word(&client.eth_call(pool, &selector(b"mid_fee()"), block)?)?;
    let out_fee = decode_word(&client.eth_call(pool, &selector(b"out_fee()"), block)?)?;
    let fee_gamma = decode_word(&client.eth_call(pool, &selector(b"fee_gamma()"), block)?)?;
    let d = decode_word(&client.eth_call(pool, &selector(b"D()"), block)?)?;
    // TwoCrypto exposes a SINGLE (non-indexed) price_scale().
    let price_scale = decode_word(&client.eth_call(pool, &selector(b"price_scale()"), block)?)?;

    Ok((
        TwoCryptoSwapState {
            balances,
            precisions,
            price_scale,
            ann,
            gamma,
            mid_fee,
            out_fee,
            fee_gamma,
            d,
            last_k0_prev: U256::ZERO,
        },
        coins,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_for_decimals_basic() {
        assert_eq!(precision_for_decimals(18), U256::from(1u64));
        assert_eq!(precision_for_decimals(6), U256::from(1_000_000_000_000u64));
    }

    #[test]
    fn indexed_call_encodes_index() {
        let sel = selector(b"coins(uint256)");
        let c = indexed_call(sel, 1);
        assert_eq!(c.len(), 36);
        assert_eq!(c[35], 1);
    }

    #[test]
    fn decode_address_rejects_dirty_padding() {
        let mut w = [0u8; 32];
        w[0] = 1;
        assert!(decode_address(&w).is_err());
        w[0] = 0;
        w[12..32].copy_from_slice(&[0xcd; 20]);
        assert_eq!(decode_address(&w).unwrap(), [0xcd; 20]);
    }

    #[test]
    fn assembled_state_feeds_twocrypto_kernel() {
        // Build the state the way fetch_twocrypto_state would (the kernel's own balanced
        // reference) and prove it solves D + quotes — parity gate for the struct layout.
        let state = TwoCryptoSwapState {
            balances: [
                U256::from(5_000_000_000_000_000_000_000u128),
                U256::from(18_000_000_000u64),
            ],
            precisions: [U256::ONE, U256::from(10_000_000_000u64)],
            price_scale: U256::from(28_000_000_000_000_000_000u128),
            ann: U256::from(200_000u64),
            gamma: U256::from(100_000_000_000_000u64),
            mid_fee: U256::from(5_000_000u64),
            out_fee: U256::from(45_000_000u64),
            fee_gamma: U256::from(3_000_000_000_000_000u64),
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        };
        let mut s = state;
        let d = s.recompute_d().expect("balanced twocrypto converges");
        assert_eq!(d, U256::from(10_039_990_829_384_809_225_748u128));
        let (out, _fee, _) = s
            .get_amount_out_exact(1, 0, U256::from(100_000_000u64))
            .unwrap();
        assert_eq!(out, U256::from(27_888_594_035_301_968_550u128));
    }
}
