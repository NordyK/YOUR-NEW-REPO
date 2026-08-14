//! Curve **CryptoSwap NG** (Tricrypto-NG) per-pool state snapshot reader.
//!
//! Fetches the live view state a 3-coin Curve CryptoSwap pool needs to quote, and assembles the
//! `engine_amm::curve_cryptoswap::CryptoSwapState` kernel input. This is the live-fetch half of
//! the CryptoSwap support whose math + state-store + registrar already shipped.
//!
//! View ABI (verified against Curve tricrypto-ng `CurveTricryptoOptimizedWETH.vy`):
//! ```text
//!   coins(uint256 i)       -> address     // coin at index i (i in 0..3)
//!   balances(uint256 i)    -> uint256      // pool balance of coin i (token-decimals units)
//!   A()                    -> uint256      // ANN-form A (fed directly into newton_D)
//!   gamma()                -> uint256      // 1e18-scaled gamma
//!   mid_fee()              -> uint256      // 1e10-scaled
//!   out_fee()              -> uint256      // 1e10-scaled
//!   fee_gamma()            -> uint256      // 1e18-scaled
//!   price_scale(uint256 k) -> uint256      // 1e18-scaled, k in 0..2 (coin1/coin0, coin2/coin0)
//!   D()                    -> uint256      // current invariant (warm-start)
//! ```
//! `precisions[i] = 10^(18 - decimals(coins[i]))` (Curve's xp normalization), read from each
//! coin's `decimals()`.
//!
//! Scope: **3-coin Tricrypto-NG only** — the `CryptoSwapState` kernel is 3-coin. A pool whose
//! `coins(2)` reverts (a 2-coin TwoCrypto-NG pool) returns `Err(TwoCoinUnsupported)` rather than
//! mis-quoting through the 3-coin math. (TwoCrypto needs its own 2-coin kernel — tracked
//! separately; near-zero Arbitrum volume per Dune.)

use alloy_primitives::U256;
use engine_amm::curve_cryptoswap::CryptoSwapState;
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
        return Err(RpcError::Decode("cryptoswap word short".into()));
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&resp[0..32]);
    Ok(U256::from_be_bytes(b))
}

fn decode_address(resp: &[u8]) -> Result<Address, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode("cryptoswap address short".into()));
    }
    if resp[0..12].iter().any(|&b| b != 0) {
        return Err(RpcError::Decode("cryptoswap address padding".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Ok(a)
}

/// Compute `10^(18 - decimals)` as a U256 precision multiplier. `decimals <= 18`.
fn precision_for_decimals(decimals: u8) -> U256 {
    let exp = 18u32.saturating_sub(decimals as u32);
    let mut p = U256::from(1u64);
    for _ in 0..exp {
        p *= U256::from(10u64);
    }
    p
}

/// Fetch a 3-coin Curve CryptoSwap NG (Tricrypto-NG) pool state at `block`, plus the 3 coin
/// addresses aligned to the kernel's coin indices (read here anyway for per-coin decimals;
/// registration stores them in the slot's `tokens` since the kernel doesn't carry them).
pub fn fetch_cryptoswap_state(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
) -> Result<(CryptoSwapState, [[u8; 20]; 3]), RpcError> {
    let coins_sel = selector(b"coins(uint256)");
    let balances_sel = selector(b"balances(uint256)");
    let decimals_sel = selector(b"decimals()");
    let price_scale_sel = selector(b"price_scale(uint256)");

    // Coins 0..3 must all resolve to a nonzero address — a revert/zero at index 2 means this
    // is a 2-coin pool we can't quote with the 3-coin kernel.
    let mut coins = [[0u8; 20]; 3];
    for i in 0..3 {
        let resp = client
            .eth_call(pool, &indexed_call(coins_sel, i), block)
            .map_err(|_| RpcError::Decode("cryptoswap coins(i) reverted (2-coin pool?)".into()))?;
        let addr = decode_address(&resp)?;
        if addr == [0u8; 20] {
            return Err(RpcError::Decode(
                "cryptoswap coin is zero (2-coin pool?)".into(),
            ));
        }
        coins[i] = addr;
    }

    // Balances + precisions per coin.
    let mut balances = [U256::ZERO; 3];
    let mut precisions = [U256::ZERO; 3];
    for i in 0..3 {
        balances[i] =
            decode_word(&client.eth_call(pool, &indexed_call(balances_sel, i), block)?)?;
        let dec_resp = client.eth_call(&coins[i], decimals_sel.as_ref(), block)?;
        let dec_u = decode_word(&dec_resp)?;
        let dec: u8 = dec_u
            .try_into()
            .map_err(|_| RpcError::Decode("cryptoswap decimals > u8".into()))?;
        if dec > 18 {
            return Err(RpcError::Decode("cryptoswap coin decimals > 18".into()));
        }
        precisions[i] = precision_for_decimals(dec);
    }

    // Scalar params.
    let ann = decode_word(&client.eth_call(pool, &selector(b"A()"), block)?)?;
    let gamma = decode_word(&client.eth_call(pool, &selector(b"gamma()"), block)?)?;
    let mid_fee = decode_word(&client.eth_call(pool, &selector(b"mid_fee()"), block)?)?;
    let out_fee = decode_word(&client.eth_call(pool, &selector(b"out_fee()"), block)?)?;
    let fee_gamma = decode_word(&client.eth_call(pool, &selector(b"fee_gamma()"), block)?)?;
    let d = decode_word(&client.eth_call(pool, &selector(b"D()"), block)?)?;

    // price_scale[0] = coin1/coin0, price_scale[1] = coin2/coin0.
    let price_scale = [
        decode_word(&client.eth_call(pool, &indexed_call(price_scale_sel, 0), block)?)?,
        decode_word(&client.eth_call(pool, &indexed_call(price_scale_sel, 1), block)?)?,
    ];

    Ok((
        CryptoSwapState {
            balances,
            precisions,
            price_scale,
            ann,
            gamma,
            mid_fee,
            out_fee,
            fee_gamma,
            d,
            // Warm-start k0 unknown from views; 0 → cold-start the newton_D iteration (the kernel
            // re-solves D anyway via recompute_d before quoting).
            last_k0_prev: U256::ZERO,
        },
        coins,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_for_decimals_matches_curve_xp_normalization() {
        assert_eq!(precision_for_decimals(18), U256::from(1u64));
        assert_eq!(precision_for_decimals(6), U256::from(1_000_000_000_000u64)); // 10^12
        assert_eq!(precision_for_decimals(8), U256::from(10_000_000_000u64)); // 10^10
        assert_eq!(
            precision_for_decimals(0),
            U256::from(10u64).pow(U256::from(18u64))
        );
    }

    #[test]
    fn indexed_call_encodes_selector_and_index() {
        let sel = selector(b"balances(uint256)");
        let c = indexed_call(sel, 2);
        assert_eq!(c.len(), 36);
        assert_eq!(&c[0..4], &sel);
        assert_eq!(c[35], 2); // index in the low byte
        assert!(c[4..35].iter().all(|&b| b == 0));
    }

    #[test]
    fn decode_address_rejects_dirty_padding() {
        let mut w = [0u8; 32];
        w[0] = 1; // nonzero in the padding region
        assert!(decode_address(&w).is_err());
        w[0] = 0;
        w[12..32].copy_from_slice(&[0xab; 20]);
        assert_eq!(decode_address(&w).unwrap(), [0xab; 20]);
    }

    #[test]
    fn decode_word_roundtrips_u256() {
        let mut w = [0u8; 32];
        w[24..32].copy_from_slice(&123_456_789u64.to_be_bytes());
        assert_eq!(decode_word(&w).unwrap(), U256::from(123_456_789u64));
        assert!(decode_word(&[0u8; 16]).is_err());
    }

    #[test]
    fn assembled_state_feeds_kernel_recompute_d() {
        // Build a CryptoSwapState the way fetch_cryptoswap_state would (the balanced reference
        // pool from the kernel's own tests), and prove the kernel can solve D + quote it. This
        // is the parity gate: the decoder's struct layout must match what the kernel expects.
        let state = CryptoSwapState {
            balances: [
                U256::from(5_000_000_000_000u64),
                U256::from(8_000_000_000u64),
                U256::from(1_600_000_000_000_000_000_000u128),
            ],
            precisions: [
                precision_for_decimals(6),  // 10^12
                precision_for_decimals(8),  // 10^10
                precision_for_decimals(18), // 1
            ],
            price_scale: [
                U256::from(60_000_000_000_000_000_000_000u128),
                U256::from(3_000_000_000_000_000_000_000u128),
            ],
            ann: U256::from(1_707_629u64),
            gamma: U256::from(11_809_167_828_997u64),
            mid_fee: U256::from(3_000_000u64),
            out_fee: U256::from(30_000_000u64),
            fee_gamma: U256::from(5_000_000_000_000_000u64),
            d: U256::ZERO,
            last_k0_prev: U256::ZERO,
        };
        let mut s = state;
        let d = s.recompute_d().expect("balanced pool must converge");
        assert_eq!(d, U256::from(14_597_972_047_721_800_479_164_646u128));
        let (out, _fee, _) = s
            .get_amount_out_exact(0, 1, U256::from(10_000_000_000u64))
            .unwrap();
        assert_eq!(out, U256::from(16_027_909u64));
    }
}
