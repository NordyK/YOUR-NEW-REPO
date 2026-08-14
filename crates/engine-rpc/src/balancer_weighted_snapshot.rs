//! Balancer V2 **Weighted Pool** per-pool state snapshot reader.
//!
//! Fetches the live state a Balancer Weighted pool needs to quote and assembles the
//! `engine_state::BalancerWeightedSlot` the kernel (`engine_amm::balancer_weighted`) consumes.
//! This is the live-fetch half of the Weighted support whose math + state-store already shipped.
//!
//! View ABI (Balancer V2 `WeightedPool` + `Vault`):
//! ```text
//!   pool.getPoolId()                  -> bytes32
//!   Vault.getPoolTokens(bytes32)      -> (address[] tokens, uint256[] balances, uint256 block)
//!   pool.getNormalizedWeights()       -> uint256[]   (1e18-scaled, sum = 1e18)
//!   pool.getSwapFeePercentage()       -> uint256     (1e18-scaled fraction)
//!   token.decimals()                  -> uint8       (per token, to upscale balances → 1e18)
//! ```
//!
//! Balancer's weighted math operates in the **upscaled 1e18 domain**: a raw balance of `b` in a
//! token with `d` decimals upscales to `b * 10^(18 - d)`. We fetch each token's `decimals()`
//! and upscale so the kernel's `calc_out_given_in` (which assumes 1e18 balances) is exact.

use alloy_primitives::U256;
use engine_feed::Address;
use engine_state::balancer_weighted_state::{BalancerWeightedSlot, MAX_BALANCER_TOKENS};
use tiny_keccak::{Hasher, Keccak};

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

fn selector(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

fn word(resp: &[u8], off: usize) -> Result<U256, RpcError> {
    // Checked `off+32`: `off` may come from an attacker-influenced getPoolTokens offset word, so a
    // crafted astronomical value must Err (not overflow usize → slice panic). See multicall::read_offset.
    let end = off
        .checked_add(32)
        .ok_or_else(|| RpcError::Decode("balancer-weighted word offset overflow".into()))?;
    if resp.len() < end {
        return Err(RpcError::Decode("balancer-weighted word short".into()));
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&resp[off..end]);
    Ok(U256::from_be_bytes(b))
}

/// Decode a dynamic `uint256[]` whose head offset word is at `head_off`.
fn decode_u256_array(resp: &[u8], head_off: usize) -> Result<Vec<U256>, RpcError> {
    let off: usize = word(resp, head_off)?
        .try_into()
        .map_err(|_| RpcError::Decode("balancer-weighted array offset overflow".into()))?;
    let len: usize = word(resp, off)?
        .try_into()
        .map_err(|_| RpcError::Decode("balancer-weighted array len overflow".into()))?;
    // Cap before allocating: a crafted length must not drive `Vec::with_capacity` into an OOM abort
    // (a real Weighted pool has ≤ MAX_BALANCER_TOKENS entries). Mirrors balancer_snapshot's guard.
    if len > MAX_BALANCER_TOKENS {
        return Err(RpcError::Decode("balancer-weighted array too long".into()));
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(word(resp, off + 32 + i * 32)?);
    }
    Ok(out)
}

/// Decode a dynamic `address[]` whose head offset word is at `head_off`.
fn decode_address_array(resp: &[u8], head_off: usize) -> Result<Vec<[u8; 20]>, RpcError> {
    let off: usize = word(resp, head_off)?
        .try_into()
        .map_err(|_| RpcError::Decode("balancer-weighted addr-array offset overflow".into()))?;
    let len: usize = word(resp, off)?
        .try_into()
        .map_err(|_| RpcError::Decode("balancer-weighted addr-array len overflow".into()))?;
    if len > MAX_BALANCER_TOKENS {
        return Err(RpcError::Decode("balancer-weighted addr-array too long".into()));
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let w = word(resp, off + 32 + i * 32)?;
        let bytes = w.to_be_bytes::<32>();
        if bytes[0..12].iter().any(|&b| b != 0) {
            return Err(RpcError::Decode("balancer-weighted addr padding".into()));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&bytes[12..32]);
        out.push(a);
    }
    Ok(out)
}

/// Upscale a raw token balance to the 1e18 domain: `balance * 10^(18 - decimals)`.
///
/// `saturating_mul`: a real token balance (`< ~2^128`) with `decimals ≤ 18` (enforced by the
/// caller) never approaches U256 overflow, so this is identical to `*` for every real pool. Only a
/// crafted/hostile balance (`> ~2^196`) could overflow — that saturates to U256::MAX rather than
/// panicking, and such an impossible balance is rejected downstream by REVM validation anyway.
fn upscale_to_d18(raw: U256, decimals: u8) -> U256 {
    let exp = 18u32.saturating_sub(decimals as u32);
    raw.saturating_mul(U256::from(10u8).pow(U256::from(exp)))
}

/// Fetch a Balancer V2 Weighted pool snapshot. `vault` is the Balancer Vault; `pool` is the
/// WeightedPool (BPT) contract.
pub fn fetch_balancer_weighted(
    client: &ArbitrumRpcClient,
    vault: &Address,
    pool: &Address,
    block: BlockTag,
) -> Result<BalancerWeightedSlot, RpcError> {
    // 0) GUARD: reject Gyroscope E-CLP pools. They are registered on the Balancer V2 Vault and
    // answer getNormalizedWeights() with notional weights (e.g. 50/50), so they would be
    // MIS-CLASSIFIED as WeightedPools — but their swap math is elliptic (E-CLP), not WeightedMath,
    // so the kernel mis-quotes them by 30-45% (proven in recorded live parity evidence against
    // "Gyroscope Dyn ECLP WETH/USDC"). E-CLP pools are the only Balancer-Vault pools that expose
    // getECLPParams(); a successful (non-empty) response means "not a WeightedPool" → reject so the
    // engine never admits one. Plain WeightedPools revert this call (Err) and proceed normally.
    if let Ok(resp) = client.eth_call(pool, &selector(b"getECLPParams()"), block) {
        if !resp.is_empty() {
            return Err(RpcError::Decode(
                "balancer pool exposes getECLPParams() — Gyroscope E-CLP, not WeightedMath".into(),
            ));
        }
    }

    // 1) poolId
    let pid_resp = client.eth_call(pool, &selector(b"getPoolId()"), block)?;
    if pid_resp.len() < 32 {
        return Err(RpcError::Decode("balancer-weighted poolId short".into()));
    }
    let mut pool_id = [0u8; 32];
    pool_id.copy_from_slice(&pid_resp[0..32]);

    // 2) Vault.getPoolTokens(poolId) → (tokens[], balances[], lastChangeBlock)
    let mut gpt = Vec::with_capacity(36);
    gpt.extend_from_slice(&selector(b"getPoolTokens(bytes32)"));
    gpt.extend_from_slice(&pool_id);
    let pt_resp = client.eth_call(vault, &gpt, block)?;
    let tokens = decode_address_array(&pt_resp, 0)?;
    let balances_raw = decode_u256_array(&pt_resp, 32)?;
    if tokens.is_empty() || tokens.len() != balances_raw.len() {
        return Err(RpcError::Decode(
            "balancer-weighted tokens/balances mismatch".into(),
        ));
    }
    if tokens.len() > MAX_BALANCER_TOKENS {
        return Err(RpcError::Decode(
            "balancer-weighted token count > MAX".into(),
        ));
    }

    // 3) normalized weights (must align 1:1 with tokens; Weighted pools have no BPT in the list)
    let weights = decode_u256_array(
        &client.eth_call(pool, &selector(b"getNormalizedWeights()"), block)?,
        0,
    )?;
    if weights.len() != tokens.len() {
        return Err(RpcError::Decode(
            "balancer-weighted weights/tokens mismatch".into(),
        ));
    }

    // 4) swap fee
    let swap_fee_d18 = word(
        &client.eth_call(pool, &selector(b"getSwapFeePercentage()"), block)?,
        0,
    )?;

    // 5) per-token decimals → upscale balances to the 1e18 domain.
    let decimals_sel = selector(b"decimals()");
    let mut balances_d18 = [U256::ZERO; MAX_BALANCER_TOKENS];
    let mut scaling_d18 = [U256::ZERO; MAX_BALANCER_TOKENS];
    let mut weights_d18 = [U256::ZERO; MAX_BALANCER_TOKENS];
    let mut tok_arr = [[0u8; 20]; MAX_BALANCER_TOKENS];
    for (i, tok) in tokens.iter().enumerate() {
        let dec_u = word(&client.eth_call(tok, decimals_sel.as_ref(), block)?, 0)?;
        let dec: u8 = dec_u
            .try_into()
            .map_err(|_| RpcError::Decode("balancer-weighted decimals > u8".into()))?;
        if dec > 18 {
            return Err(RpcError::Decode(
                "balancer-weighted token decimals > 18".into(),
            ));
        }
        balances_d18[i] = upscale_to_d18(balances_raw[i], dec);
        // scaling = 10^(18 - decimals) (= upscale_to_d18(1, dec)).
        scaling_d18[i] = upscale_to_d18(U256::from(1u64), dec);
        weights_d18[i] = weights[i];
        tok_arr[i] = *tok;
    }

    Ok(BalancerWeightedSlot {
        balances_d18,
        scaling_d18,
        weights_d18,
        tokens: tok_arr,
        n_tokens: tokens.len() as u8,
        swap_fee_d18,
        pool_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ABI-encode helpers for fixtures.
    fn w_u256(v: u128) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[16..32].copy_from_slice(&v.to_be_bytes());
        b
    }
    fn w_addr(a: [u8; 20]) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[12..32].copy_from_slice(&a);
        b
    }

    /// Build a `getPoolTokens` return: (address[] tokens, uint256[] balances, uint256 block).
    fn build_get_pool_tokens(tokens: &[[u8; 20]], balances: &[u128]) -> Vec<u8> {
        // head: 3 words — offset(tokens), offset(balances), lastChangeBlock.
        // tokens[] at 0x60; balances[] right after.
        let n = tokens.len();
        let mut out = Vec::new();
        let tokens_off = 0x60u128;
        let tokens_words = 1 + n; // len + elements
        let balances_off = tokens_off + (tokens_words as u128) * 32;
        out.extend_from_slice(&w_u256(tokens_off));
        out.extend_from_slice(&w_u256(balances_off));
        out.extend_from_slice(&w_u256(123456)); // lastChangeBlock
                                                // tokens[]
        out.extend_from_slice(&w_u256(n as u128));
        for t in tokens {
            out.extend_from_slice(&w_addr(*t));
        }
        // balances[]
        out.extend_from_slice(&w_u256(n as u128));
        for b in balances {
            out.extend_from_slice(&w_u256(*b));
        }
        out
    }

    fn build_u256_array(vals: &[u128]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&w_u256(0x20)); // offset
        out.extend_from_slice(&w_u256(vals.len() as u128));
        for v in vals {
            out.extend_from_slice(&w_u256(*v));
        }
        out
    }

    #[test]
    fn upscale_matches_balancer_domain() {
        // 6-decimal USDC balance 1000.00 → 1000e18.
        assert_eq!(
            upscale_to_d18(U256::from(1_000_000_000u64), 6),
            U256::from(1_000_000_000_000_000_000_000u128)
        );
        // 18-decimal token → unchanged.
        assert_eq!(upscale_to_d18(U256::from(42u64), 18), U256::from(42u64));
    }

    #[test]
    fn decode_get_pool_tokens_extracts_tokens_and_balances() {
        let toks = [[0xaa; 20], [0xbb; 20]];
        let bals = [1_000u128, 2_000u128];
        let resp = build_get_pool_tokens(&toks, &bals);
        let tokens = decode_address_array(&resp, 0).unwrap();
        let balances = decode_u256_array(&resp, 32).unwrap();
        assert_eq!(tokens, vec![[0xaa; 20], [0xbb; 20]]);
        assert_eq!(balances, vec![U256::from(1_000u64), U256::from(2_000u64)]);
    }

    #[test]
    fn decode_normalized_weights_array() {
        let resp = build_u256_array(&[800_000_000_000_000_000, 200_000_000_000_000_000]);
        let w = decode_u256_array(&resp, 0).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], U256::from(800_000_000_000_000_000u128));
        assert_eq!(w[1], U256::from(200_000_000_000_000_000u128));
    }

    #[test]
    fn decode_address_array_rejects_dirty_padding() {
        // A word with nonzero high bytes is not a clean address → reject.
        let mut resp = Vec::new();
        resp.extend_from_slice(&w_u256(0x20)); // offset
        resp.extend_from_slice(&w_u256(1)); // len 1
        let mut dirty = [0u8; 32];
        dirty[0] = 0xff;
        resp.extend_from_slice(&dirty);
        assert!(decode_address_array(&resp, 0).is_err());
    }

    #[test]
    fn assembled_slot_feeds_weighted_kernel() {
        // Build the slot the fetcher would for an 80/20 WETH/USDC pool and confirm the kernel
        // quotes it — parity gate between the decoder's struct layout and the math.
        let mut tokens = [[0u8; 20]; MAX_BALANCER_TOKENS];
        tokens[0] = [0x82; 20];
        tokens[1] = [0xaf; 20];
        let mut balances = [U256::ZERO; MAX_BALANCER_TOKENS];
        balances[0] = U256::from(1_000_000_000_000_000_000_000u128); // 1000e18
        balances[1] = U256::from(100_000_000_000_000_000_000u128); // 100e18
        let mut weights = [U256::ZERO; MAX_BALANCER_TOKENS];
        weights[0] = U256::from(800_000_000_000_000_000u128);
        weights[1] = U256::from(200_000_000_000_000_000u128);
        let slot = BalancerWeightedSlot {
            balances_d18: balances,
            scaling_d18: [U256::from(1u64); MAX_BALANCER_TOKENS],
            weights_d18: weights,
            tokens,
            n_tokens: 2,
            swap_fee_d18: U256::from(1_000_000_000_000_000u128),
            pool_id: [0u8; 32],
        };
        let out = engine_amm::balancer_weighted::calc_out_given_in(
            slot.balances_d18[0],
            slot.weights_d18[0],
            slot.balances_d18[1],
            slot.weights_d18[1],
            U256::from(10_000_000_000_000_000_000u128),
        )
        .expect("8020 quote");
        let expected = U256::from(3_901_965_517_241_379_310u128);
        let drift = if out >= expected {
            out - expected
        } else {
            expected - out
        };
        assert!(
            drift < U256::from(100_000_000_000u128),
            "slot-fed quote drifted: {out} vs {expected}"
        );
    }
}

#[cfg(test)]
mod abi_offset_overflow_tests {
    use super::*;
    #[test]
    fn word_rejects_astronomical_offset_without_panic() {
        let resp = [0u8; 64];
        assert!(word(&resp, usize::MAX - 10).is_err());
    }
}

#[cfg(test)]
mod upscale_overflow_tests {
    //! AUD-ABI-OFFSET (sibling): upscale_to_d18 must not panic (U256 mul overflow) on a
    //! crafted/hostile balance. Real token balances never approach this; a hostile RPC could.
    use super::*;
    #[test]
    fn upscale_to_d18_saturates_instead_of_panicking() {
        // raw near U256::MAX with exp=18 → raw * 1e18 overflows U256.
        let raw = U256::MAX;
        let scaled = upscale_to_d18(raw, 0); // exp = 18
        assert_eq!(scaled, U256::MAX, "overflow must saturate, not panic");
    }
    #[test]
    fn upscale_to_d18_exact_for_real_balances() {
        // Regression: a normal 6-decimal balance scales to 18 decimals exactly.
        let raw = U256::from(1_000_000u64); // 1.0 USDC (6 dp)
        let scaled = upscale_to_d18(raw, 6); // exp = 12
        assert_eq!(scaled, U256::from(1_000_000_000_000_000_000u64));
    }
}
