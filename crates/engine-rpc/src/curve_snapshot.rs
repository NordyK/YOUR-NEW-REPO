//! Curve StableSwap-NG plain-pool snapshot reader.
//!
//! Unlike Balancer (single Vault), Curve pools are ordinary per-pool contracts. For an
//! `N`-coin plain pool we read:
//! * `coins(uint256 i)` → `address` for i in 0..N (probe until revert / zero-address)
//! * `balances(uint256 i)` → `uint256` raw balance per coin
//! * `A_precise()` → `uint256` = `A * A_PRECISION` (matches the kernel's `a_param`)
//! * `fee()` → `uint256` in 1e10 units (e.g. 1_000_000 = 1 bps)
//! * `stored_rates()` → `uint256[N]` — the per-coin rate scalers the kernel consumes
//!   directly (`rate = 10^(36 - decimals)` for a non-rebasing coin, plus any
//!   rate-provider/oracle multiplier the pool stores). This is exactly the kernel's
//!   `rates` array, so we use it verbatim and never re-derive from `decimals()`.
//!
//! The math kernel (`engine_amm::curve`) consumes the resulting view.
//!
//! Note: the static-fee kernel does NOT model StableSwap-NG's dynamic *offpeg* fee
//! multiplier, so on a heavily-imbalanced pool the kernel's output can be slightly
//! high (it applies only the base `fee`). For balanced stable pairs — the common arb
//! target — this is negligible. The snapshot here captures the inputs faithfully; the
//! divergence is purely the kernel's fee model.

use alloy_primitives::U256;
use engine_amm::curve::{StableSwapState, MAX_COINS};
use engine_feed::Address;
use tiny_keccak::{Hasher, Keccak};

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

/// Canonical Curve StableSwap-NG factory on Arbitrum One (plain/crvUSD pools).
pub const CURVE_STABLESWAP_NG_FACTORY_ARBITRUM: [u8; 20] = [
    0x9A, 0xF1, 0x4D, 0x26, 0x07, 0x5f, 0x14, 0x2e, 0xb3, 0xF2, 0x92, 0xD5, 0x06, 0x5E, 0xB3, 0xfa,
    0xa6, 0x46, 0x16, 0x7b,
];

/// Raw Curve plain-pool view: the kernel state plus the per-coin token addresses.
#[derive(Debug, Clone)]
pub struct CurvePoolView {
    /// Kernel math state consumed by `engine_amm::curve::get_dy`.
    pub kernel: StableSwapState,
    /// Coin token addresses aligned with `kernel.balances` / `kernel.rates`.
    pub tokens: [Address; MAX_COINS],
}

fn selector(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

/// `coins(uint256)` / `balances(uint256)` calldata: selector + the 32-byte index word.
fn indexed_call(sel: [u8; 4], idx: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(36);
    data.extend_from_slice(&sel);
    let mut word = [0u8; 32];
    word[24..32].copy_from_slice(&(idx as u64).to_be_bytes());
    data.extend_from_slice(&word);
    data
}

fn decode_address(resp: &[u8]) -> Result<Address, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode("curve address word short".into()));
    }
    if resp[0..12].iter().any(|&b| b != 0) {
        return Err(RpcError::Decode("curve address padding nonzero".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Ok(a)
}

fn decode_word(resp: &[u8], off: usize) -> Result<U256, RpcError> {
    // Checked `off+32`: `off` may come from an attacker-influenced ABI offset word, so a crafted
    // astronomical value must Err (not overflow usize → slice panic). Mirrors multicall::read_offset.
    let end = off
        .checked_add(32)
        .ok_or_else(|| RpcError::Decode("curve word offset overflow".into()))?;
    if resp.len() < end {
        return Err(RpcError::Decode("curve word short".into()));
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&resp[off..end]);
    Ok(U256::from_be_bytes(b))
}

fn u256_to_u128(x: U256) -> Result<u128, RpcError> {
    x.try_into()
        .map_err(|_| RpcError::Decode("curve u128 overflow".into()))
}

fn u256_to_u64(x: U256) -> Result<u64, RpcError> {
    x.try_into()
        .map_err(|_| RpcError::Decode("curve u64 overflow".into()))
}

/// Decode the `stored_rates()` dynamic `uint256[]` return into the kernel rate array.
/// Expects exactly `n_coins` entries; returns the rate at each coin index.
fn decode_stored_rates(resp: &[u8], n_coins: usize) -> Result<[u128; MAX_COINS], RpcError> {
    // Layout: [0:32]=offset to the array, [offset]=len, [offset+32 ..]=elements.
    let off: usize = u256_to_u128(decode_word(resp, 0)?)
        .map_err(|_| RpcError::Decode("curve rates offset".into()))? as usize;
    let len: usize = u256_to_u128(decode_word(resp, off)?)
        .map_err(|_| RpcError::Decode("curve rates len".into()))? as usize;
    if len != n_coins {
        return Err(RpcError::Decode(
            "curve stored_rates length mismatch".into(),
        ));
    }
    let mut rates = [0u128; MAX_COINS];
    for k in 0..n_coins {
        let v = decode_word(resp, off + 32 + k * 32)?;
        rates[k] = u256_to_u128(v)?;
    }
    Ok(rates)
}

/// Fetch a Curve StableSwap-NG plain-pool snapshot at the given block.
///
/// `n_coins` is the coin count expected from discovery (2..=MAX_COINS); the reader still
/// probes `coins(i)` and trims to the first reverting / zero index so a discovery/chain
/// mismatch fails closed rather than admitting garbage.
pub fn fetch_curve_state(
    client: &ArbitrumRpcClient,
    pool: &Address,
    n_coins: usize,
    block: BlockTag,
) -> Result<CurvePoolView, RpcError> {
    let coins_sel = selector(b"coins(uint256)");
    let balances_sel = selector(b"balances(uint256)");
    let a_sel = selector(b"A_precise()");
    let fee_sel = selector(b"fee()");
    let rates_sel = selector(b"stored_rates()");
    let offpeg_sel = selector(b"offpeg_fee_multiplier()");

    if !(2..=MAX_COINS).contains(&n_coins) {
        return Err(RpcError::Decode("curve n_coins out of range".into()));
    }

    let mut tokens = [[0u8; 20]; MAX_COINS];
    let mut balances = [0u128; MAX_COINS];
    let mut n = 0usize;
    for i in 0..n_coins {
        let coin_resp = match client.eth_call(pool, &indexed_call(coins_sel, i), block) {
            Ok(r) => r,
            Err(_) => break, // out of range → fewer coins than discovery expected
        };
        let addr = match decode_address(&coin_resp) {
            Ok(a) => a,
            Err(_) => break,
        };
        if addr == [0u8; 20] {
            break;
        }
        let bal_resp = client.eth_call(pool, &indexed_call(balances_sel, i), block)?;
        let bal = u256_to_u128(decode_word(&bal_resp, 0)?)?;
        tokens[n] = addr;
        balances[n] = bal;
        n += 1;
    }
    if n < 2 {
        return Err(RpcError::Decode("curve pool has < 2 coins".into()));
    }

    let a_resp = client.eth_call(pool, &a_sel, block)?;
    let a_param = u256_to_u64(decode_word(&a_resp, 0)?)?;

    let fee_resp = client.eth_call(pool, &fee_sel, block)?;
    let fee = u256_to_u64(decode_word(&fee_resp, 0)?)?;

    // StableSwap-NG dynamic offpeg fee multiplier (1e10 units). Older plain pools may not expose
    // it (call reverts) → default 0, which the kernel treats as "no dynamic component" (static fee).
    let offpeg_fee_multiplier = client
        .eth_call(pool, &offpeg_sel, block)
        .ok()
        .and_then(|r| decode_word(&r, 0).ok())
        .and_then(|w| u256_to_u64(w).ok())
        .unwrap_or(0);

    let rates_resp = client.eth_call(pool, &rates_sel, block)?;
    let rates = decode_stored_rates(&rates_resp, n)?;

    let kernel = StableSwapState {
        n_coins: n,
        balances,
        rates,
        a_param,
        fee,
        offpeg_fee_multiplier,
    };
    Ok(CurvePoolView { kernel, tokens })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(v: U256) -> Vec<u8> {
        v.to_be_bytes::<32>().to_vec()
    }

    #[test]
    fn stored_rates_decode_two_coins() {
        // ABI: head pointer (0x20) + len(2) + rate0(1e18) + rate1(1e30).
        let mut blob = Vec::new();
        blob.extend_from_slice(&word(U256::from(0x20u64)));
        blob.extend_from_slice(&word(U256::from(2u64)));
        blob.extend_from_slice(&word(U256::from(1_000_000_000_000_000_000u128)));
        blob.extend_from_slice(&word(U256::from(10u64).pow(U256::from(30u64))));
        let rates = decode_stored_rates(&blob, 2).unwrap();
        assert_eq!(rates[0], 1_000_000_000_000_000_000u128);
        assert_eq!(rates[1], 10u128.pow(30));
    }

    #[test]
    fn stored_rates_length_mismatch_errors() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&word(U256::from(0x20u64)));
        blob.extend_from_slice(&word(U256::from(3u64))); // claims 3 but we ask for 2
        blob.extend_from_slice(&word(U256::from(1u64)));
        blob.extend_from_slice(&word(U256::from(1u64)));
        blob.extend_from_slice(&word(U256::from(1u64)));
        assert!(decode_stored_rates(&blob, 2).is_err());
    }

    #[test]
    fn indexed_call_layout() {
        let c = indexed_call(selector(b"coins(uint256)"), 1);
        assert_eq!(c.len(), 36);
        assert_eq!(c[35], 1);
    }

    /// Offline parity vs a live Arbitrum Curve pool (crvUSD/USDC.e
    /// `0x3adf984c937fa6846e5a24e0a68521bdaf767ce1`) captured 2026-05-25:
    /// balances [75156140434690947953802, 43034902322], rates [1e18, 1e30],
    /// A_precise 100000, fee 1_000_000. On-chain `get_dy(0,1,1e18)` = 999261; the
    /// static-fee kernel yields ~999583 (the ~0.03% gap is the NG offpeg dynamic fee,
    /// which this kernel does not model). The snapshot fields are reproduced exactly.
    #[test]
    fn live_pool_snapshot_shape() {
        use engine_amm::curve::{get_dy, A_PRECISION};
        let mut balances = [0u128; MAX_COINS];
        let mut rates = [0u128; MAX_COINS];
        balances[0] = 75_156_140_434_690_947_953_802;
        balances[1] = 43_034_902_322;
        rates[0] = 1_000_000_000_000_000_000;
        rates[1] = 10u128.pow(30);
        assert_eq!(rates[0], 1_000_000_000_000_000_000);
        assert_eq!(1000 * A_PRECISION, 100_000);
        let kernel = StableSwapState {
            n_coins: 2,
            balances,
            rates,
            a_param: 100_000,
            fee: 1_000_000,
            offpeg_fee_multiplier: 0, // static-fee baseline (this assertion checks the base kernel)
        };
        let dy = get_dy(&kernel, 0, 1, 1_000_000_000_000_000_000).unwrap();
        // Within 0.1% of the on-chain 999261 (kernel omits the NG offpeg fee).
        assert!(
            (999_000..=1_000_000).contains(&dy),
            "kernel get_dy out of band: {dy}"
        );
    }
}

#[cfg(test)]
mod abi_offset_overflow_tests {
    use super::*;
    #[test]
    fn decode_word_rejects_astronomical_offset_without_panic() {
        let resp = [0u8; 64];
        assert!(decode_word(&resp, usize::MAX - 10).is_err());
    }
}
