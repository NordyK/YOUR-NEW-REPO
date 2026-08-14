//! Balancer V2 stable-pool snapshot reader.
//!
//! Balancer V2 holds all pool balances in a single Vault
//! (`0xBA12222222228d8Ba445958a75a0704d566BF2C8` on Arbitrum). Per pool we read:
//! * `pool.getPoolId()` → bytes32
//! * `Vault.getPoolTokens(poolId)` → (address[] tokens, uint256[] balances, uint256)
//! * `pool.getScalingFactors()` → uint256[] (decimals × rate-provider rate, 18-dec)
//! * `pool.getAmplificationParameter()` → (uint256 value = A*1000, bool, uint256)
//! * `pool.getSwapFeePercentage()` → uint256 (1e18 fraction)
//! * `pool.getBptIndex()` → uint256 (ComposableStable; the BPT slot to exclude)
//!
//! The kernel (`engine_amm::balancer_stable`) consumes the resulting view with the
//! BPT entry removed by the caller.

use alloy_primitives::U256;
use engine_feed::Address;
use tiny_keccak::{Hasher, Keccak};

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

/// Canonical Balancer V2 Vault on Arbitrum One.
pub const BALANCER_VAULT_ARBITRUM: [u8; 20] = [
    0xBA, 0x12, 0x22, 0x22, 0x22, 0x22, 0x8d, 0x8B, 0xa4, 0x45, 0x95, 0x8a, 0x75, 0xa0, 0x70, 0x4d,
    0x56, 0x6B, 0xF2, 0xC8,
];

/// Raw Balancer stable-pool view (BPT still present at `bpt_index`).
#[derive(Debug, Clone)]
pub struct BalancerStableView {
    /// 32-byte poolId (Vault key).
    pub pool_id: [u8; 32],
    /// Registered tokens (includes the BPT for ComposableStable).
    pub tokens: Vec<[u8; 20]>,
    /// Raw balances aligned with `tokens`.
    pub balances: Vec<U256>,
    /// 18-dec scaling factors aligned with `tokens`.
    pub scaling_factors: Vec<U256>,
    /// Amplification parameter, raw `A * 1000`.
    pub amp: u64,
    /// Swap fee as an 18-dec fraction.
    pub swap_fee_1e18: u128,
    /// BPT index in `tokens` (None for legacy non-composable stable pools).
    pub bpt_index: Option<usize>,
}

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
        .ok_or_else(|| RpcError::Decode("balancer word offset overflow".into()))?;
    if resp.len() < end {
        return Err(RpcError::Decode("balancer word short".into()));
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&resp[off..end]);
    Ok(U256::from_be_bytes(b))
}

fn word_usize(resp: &[u8], off: usize) -> Result<usize, RpcError> {
    let w = word(resp, off)?;
    w.try_into()
        .map_err(|_| RpcError::Decode("balancer usize overflow".into()))
}

/// Decode a dynamic array (`T[]`) located via the head pointer at `head_off`.
/// Returns (offset_to_length, length).
fn array_header(resp: &[u8], head_off: usize) -> Result<(usize, usize), RpcError> {
    let data_off = word_usize(resp, head_off)?;
    let len = word_usize(resp, data_off)?;
    Ok((data_off + 32, len))
}

fn decode_u256_array(resp: &[u8], head_off: usize) -> Result<Vec<U256>, RpcError> {
    let (start, len) = array_header(resp, head_off)?;
    if len > 16 {
        return Err(RpcError::Decode("balancer array too long".into()));
    }
    let mut out = Vec::with_capacity(len);
    for k in 0..len {
        out.push(word(resp, start + k * 32)?);
    }
    Ok(out)
}

fn decode_address_array(resp: &[u8], head_off: usize) -> Result<Vec<[u8; 20]>, RpcError> {
    let (start, len) = array_header(resp, head_off)?;
    if len > 16 {
        return Err(RpcError::Decode("balancer addr array too long".into()));
    }
    let mut out = Vec::with_capacity(len);
    for k in 0..len {
        let w = &resp
            [start + k * 32..start + k * 32 + 32.min(resp.len().saturating_sub(start + k * 32))];
        if w.len() < 32 {
            return Err(RpcError::Decode("balancer addr word short".into()));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&w[12..32]);
        out.push(a);
    }
    Ok(out)
}

/// Decode a complete Balancer stable-pool view from the raw sub-returns the snapshot
/// reads: the 32-byte `pool_id` (already decoded from `getPoolId()`), the Vault
/// `getPoolTokens(poolId)` blob, `getScalingFactors()`, `getAmplificationParameter()`,
/// `getSwapFeePercentage()`, and the optional `getBptIndex()` blob (`None` when that
/// call reverted — legacy non-composable stable pools).
///
/// This is the SINGLE source of decode truth: both the per-pool [`fetch_balancer_stable`]
/// and the batched [`fetch_balancer_states_batched`] route every sub-return through here,
/// so a batched `BalancerStableView` is byte-for-byte identical to the per-pool one. A
/// non-stable pool (weighted etc.) reverts `getAmplificationParameter()`/`getScalingFactors()`
/// — that surfaces here as a `Decode` error and the caller skips the pool, exactly as the
/// per-pool path does.
/// Balancer scaling factor for a plain StablePool token with no rate provider:
/// `FixedPoint.ONE * 10^(18 - decimals) = 10^(36 - decimals)` — exactly what
/// `getScalingFactors()` returns when the price rate is 1.0 (so deriving it from decimals is
/// correct for the legacy StablePools that don't expose the getter).
pub fn scaling_factor_from_decimals(decimals: u8) -> U256 {
    let exp = 36u32.saturating_sub(decimals as u32);
    U256::from(10u8).pow(U256::from(exp))
}

/// Scaling factor for one token of a LEGACY Balancer StablePool (no `getScalingFactors()`), derived
/// from its on-chain `decimals()`. FAIL-CLOSED [Codex]: if `decimals()` is unavailable (`None`) we
/// return an error so the caller SKIPS the pool — fabricating an 18-dec default would mis-scale a
/// non-18 token (e.g. 6-dec USDC) by 10^(d-18) and corrupt the entire stable-math view for that pool.
fn legacy_scaling_factor(decimals: Option<u8>) -> Result<U256, RpcError> {
    match decimals {
        Some(d) => Ok(scaling_factor_from_decimals(d)),
        None => Err(RpcError::Decode(
            "balancer legacy-stable: token decimals() unavailable — refusing to fabricate an \
             18-dec scaling factor (would mis-scale a non-18 token); skipping pool"
                .into(),
        )),
    }
}

fn legacy_scaling_factors(
    decimals: impl IntoIterator<Item = Option<u8>>,
) -> Result<Vec<U256>, RpcError> {
    decimals.into_iter().map(legacy_scaling_factor).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_scaling_factor_fails_closed_without_decimals() {
        // Known decimals → 10^(36-d).
        assert_eq!(
            legacy_scaling_factor(Some(6)).unwrap(),
            U256::from(10u8).pow(U256::from(30u32))
        );
        assert_eq!(
            legacy_scaling_factor(Some(18)).unwrap(),
            U256::from(10u8).pow(U256::from(18u32))
        );
        // Unknown decimals → ERROR (skip the pool), NOT a fabricated 18-dec default that would
        // silently mis-scale a non-18 token.
        assert!(
            legacy_scaling_factor(None).is_err(),
            "missing decimals must fail-closed, not default to 18"
        );
    }

    #[test]
    fn legacy_scaling_factors_fail_closed_if_any_batched_decimal_is_missing() {
        let err = legacy_scaling_factors([Some(18), None, Some(6)])
            .expect_err("batched legacy scaling must reject any missing token decimals");
        assert!(
            err.to_string().contains("decimals"),
            "error should mention decimals: {err}"
        );
    }
}

fn decode_balancer_stable(
    pool_id: [u8; 32],
    pt_resp: &[u8],
    scaling_factors: Vec<U256>,
    amp_resp: &[u8],
    fee_resp: &[u8],
    bpt_resp: Option<&[u8]>,
) -> Result<BalancerStableView, RpcError> {
    // getPoolTokens head: [0]=offset tokens, [32]=offset balances, [64]=lastChangeBlock
    let tokens = decode_address_array(pt_resp, 0)?;
    let balances_u = decode_u256_array(pt_resp, 32)?;
    if tokens.is_empty() || tokens.len() != balances_u.len() {
        return Err(RpcError::Decode("balancer tokens/balances mismatch".into()));
    }

    if scaling_factors.len() != tokens.len() {
        return Err(RpcError::Decode("balancer scaling/tokens mismatch".into()));
    }

    let amp: u64 = word(amp_resp, 0)?
        .try_into()
        .map_err(|_| RpcError::Decode("balancer amp overflow".into()))?;

    let swap_fee_1e18: u128 = word(fee_resp, 0)?
        .try_into()
        .map_err(|_| RpcError::Decode("balancer fee overflow".into()))?;

    // BPT index (ComposableStable only; legacy stable reverts → None)
    let bpt_index = bpt_resp.and_then(|r| word_usize(r, 0).ok());

    Ok(BalancerStableView {
        pool_id,
        tokens,
        balances: balances_u,
        scaling_factors,
        amp,
        swap_fee_1e18,
        bpt_index,
    })
}

/// Fetch a Balancer V2 stable-pool snapshot at the given block. `vault` is the
/// Balancer Vault address; `pool` is the pool (BPT) contract.
pub fn fetch_balancer_stable(
    client: &ArbitrumRpcClient,
    vault: &Address,
    pool: &Address,
    block: BlockTag,
) -> Result<BalancerStableView, RpcError> {
    // 1) poolId
    let pid_resp = client.eth_call(pool, &selector(b"getPoolId()"), block)?;
    if pid_resp.len() < 32 {
        return Err(RpcError::Decode("balancer poolId short".into()));
    }
    let mut pool_id = [0u8; 32];
    pool_id.copy_from_slice(&pid_resp[0..32]);

    // 2) Vault.getPoolTokens(poolId)
    let mut gpt = Vec::with_capacity(36);
    gpt.extend_from_slice(&selector(b"getPoolTokens(bytes32)"));
    gpt.extend_from_slice(&pool_id);
    let pt_resp = client.eth_call(vault, &gpt, block)?;

    // 3) scaling factors. ComposableStable/MetaStable expose getScalingFactors(); plain
    // legacy StablePools do NOT (it reverts) — for those the scaling factor is purely
    // decimal-based (10^(36-d)), so derive it from each token's decimals().
    let tokens = decode_address_array(&pt_resp, 0)?;
    let sf_resp = client
        .eth_call(pool, &selector(b"getScalingFactors()"), block)
        .ok();
    let scaling_factors: Vec<U256> = match sf_resp {
        Some(ref sf)
            if decode_u256_array(sf, 0)
                .map(|v| v.len() == tokens.len())
                .unwrap_or(false) =>
        {
            decode_u256_array(sf, 0)?
        }
        _ => {
            let mut v = Vec::with_capacity(tokens.len());
            for t in &tokens {
                // [Codex] Fail-closed: a token whose decimals() can't be read SKIPS the pool (via the
                // `?` below) rather than fabricating an 18-dec default that would mis-scale a non-18
                // token. Legacy StablePools are stablecoins (decimals() effectively always answers),
                // so this only triggers on a genuinely broken/unreadable token.
                let d = crate::erc20::fetch_erc20_decimals(client, t, block).ok();
                v.push(legacy_scaling_factor(d)?);
            }
            v
        }
    };

    // 4) amplification parameter (value = A * 1000). Still REQUIRED — a revert here means a
    // non-stable (weighted) pool, which we don't model → skip.
    let amp_resp = client.eth_call(pool, &selector(b"getAmplificationParameter()"), block)?;

    // 5) swap fee (1e18 fraction)
    let fee_resp = client.eth_call(pool, &selector(b"getSwapFeePercentage()"), block)?;

    // 6) BPT index (ComposableStable only; legacy stable reverts → None)
    let bpt_resp = client
        .eth_call(pool, &selector(b"getBptIndex()"), block)
        .ok();

    decode_balancer_stable(
        pool_id,
        &pt_resp,
        scaling_factors,
        &amp_resp,
        &fee_resp,
        bpt_resp.as_deref(),
    )
}

/// Batch the Balancer stable-pool reads for many pools through Multicall3 `aggregate3`.
/// Returns one `Option` per input pool (`None` if the pool is non-stable / a required
/// sub-call reverted), in input order.
///
/// `getPoolTokens` is a Vault call keyed by the pool's `poolId`, which itself must be
/// read from the pool — so this issues TWO `aggregate3` round-trips per chunk:
///   1. `getPoolId()` for every pool (one call each).
///   2. `getPoolTokens(poolId)` against the Vault + the pool's own
///      `getScalingFactors()` / `getAmplificationParameter()` / `getSwapFeePercentage()`
///      / `getBptIndex()` reads.
/// That is 2 RPC per chunk instead of ~6 per pool. The transport is batched; each pool's
/// sub-returns are decoded with the SAME [`decode_balancer_stable`] the per-pool
/// [`fetch_balancer_stable`] uses, so a batched view is byte-for-byte identical.
///
/// Non-stable pools (weighted, etc.) revert `getAmplificationParameter()` /
/// `getScalingFactors()`; that sub-call's `success=false` flag flows into a `None`
/// for that pool WITHOUT failing the chunk — they are skipped exactly as the per-pool
/// path skips them today.
pub fn fetch_balancer_states_batched(
    client: &ArbitrumRpcClient,
    multicall3: &Address,
    vault: &Address,
    pools: &[Address],
    block: BlockTag,
) -> Result<Vec<Option<BalancerStableView>>, RpcError> {
    use crate::multicall::{aggregate3, Call3};
    let pool_id_sel = selector(b"getPoolId()");
    let get_pool_tokens_sel = selector(b"getPoolTokens(bytes32)");
    let scaling_sel = selector(b"getScalingFactors()");
    let amp_sel = selector(b"getAmplificationParameter()");
    let fee_sel = selector(b"getSwapFeePercentage()");
    let bpt_sel = selector(b"getBptIndex()");
    let dec_sel = selector(b"decimals()");

    // ── Round 1: getPoolId() for every pool ──────────────────────────────────────
    let id_calls: Vec<Call3> = pools
        .iter()
        .map(|p| Call3 {
            target: *p,
            allow_failure: true,
            call_data: pool_id_sel.to_vec(),
        })
        .collect();
    let id_results = aggregate3(client, multicall3, &id_calls, block)?;
    // Decode each poolId (None if the call reverted / decoded short — that pool can't be
    // snapshotted and stays None in the output).
    let pool_ids: Vec<Option<[u8; 32]>> = id_results
        .iter()
        .map(|r| {
            if r.success && r.return_data.len() >= 32 {
                let mut pid = [0u8; 32];
                pid.copy_from_slice(&r.return_data[0..32]);
                Some(pid)
            } else {
                None
            }
        })
        .collect();

    // ── Round 2: per-pool state reads (5 calls each) ─────────────────────────────
    // We only emit calls for pools that produced a poolId. The per-pool 5-call block is
    // laid out in a fixed order: getPoolTokens, scaling, amp, fee, bptIndex.
    let mut calls: Vec<Call3> = Vec::with_capacity(pools.len() * 5);
    // Map each pool index to its call-block start, or None if it had no poolId.
    let mut block_start: Vec<Option<usize>> = Vec::with_capacity(pools.len());
    for (i, pid_opt) in pool_ids.iter().enumerate() {
        match pid_opt {
            Some(pid) => {
                block_start.push(Some(calls.len()));
                let pool = pools[i];
                let mut gpt = Vec::with_capacity(36);
                gpt.extend_from_slice(&get_pool_tokens_sel);
                gpt.extend_from_slice(pid);
                calls.push(Call3 {
                    target: *vault,
                    allow_failure: true,
                    call_data: gpt,
                });
                calls.push(Call3 {
                    target: pool,
                    allow_failure: true,
                    call_data: scaling_sel.to_vec(),
                });
                calls.push(Call3 {
                    target: pool,
                    allow_failure: true,
                    call_data: amp_sel.to_vec(),
                });
                calls.push(Call3 {
                    target: pool,
                    allow_failure: true,
                    call_data: fee_sel.to_vec(),
                });
                calls.push(Call3 {
                    target: pool,
                    allow_failure: true,
                    call_data: bpt_sel.to_vec(),
                });
            }
            None => block_start.push(None),
        }
    }
    let state_results = aggregate3(client, multicall3, &calls, block)?;

    // Phase 1: for each pool require getPoolTokens/amp/fee to succeed (amp-revert = weighted →
    // skip). scaling_factors come from getScalingFactors() when present; legacy StablePools
    // revert it → collect their tokens for a round-3 decimals() fetch (derive 10^(36-d)).
    struct Raw {
        pid: [u8; 32],
        pt: Vec<u8>,
        amp: Vec<u8>,
        fee: Vec<u8>,
        bpt: Option<Vec<u8>>,
        sf: Option<Vec<U256>>,
    }
    let mut raws: Vec<Option<Raw>> = (0..pools.len()).map(|_| None).collect();
    let mut needs_dec: Vec<(usize, Vec<Address>)> = Vec::new();
    for (i, pid_opt) in pool_ids.iter().enumerate() {
        let (Some(pid), Some(start)) = (pid_opt, block_start[i]) else {
            continue;
        };
        let (pt, sf, amp, fee, bpt) = (
            state_results.get(start),
            state_results.get(start + 1),
            state_results.get(start + 2),
            state_results.get(start + 3),
            state_results.get(start + 4),
        );
        let (Some(pt), Some(amp), Some(fee)) = (pt, amp, fee) else {
            continue;
        };
        if !(pt.success && amp.success && fee.success) {
            continue;
        }
        let Ok(tokens) = decode_address_array(&pt.return_data, 0) else {
            continue;
        };
        let sf_dec: Option<Vec<U256>> = match sf {
            Some(s)
                if s.success
                    && decode_u256_array(&s.return_data, 0)
                        .map(|v| v.len() == tokens.len())
                        .unwrap_or(false) =>
            {
                decode_u256_array(&s.return_data, 0).ok()
            }
            _ => None,
        };
        if sf_dec.is_none() {
            needs_dec.push((i, tokens));
        }
        let bpt_b = bpt.and_then(|b| {
            if b.success {
                Some(b.return_data.clone())
            } else {
                None
            }
        });
        raws[i] = Some(Raw {
            pid: *pid,
            pt: pt.return_data.clone(),
            amp: amp.return_data.clone(),
            fee: fee.return_data.clone(),
            bpt: bpt_b,
            sf: sf_dec,
        });
    }
    // Round 3 (only legacy StablePools): decimals() for their tokens → derive scaling.
    if !needs_dec.is_empty() {
        let mut dcalls: Vec<Call3> = Vec::new();
        let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
        for (idx, toks) in &needs_dec {
            let s = dcalls.len();
            for t in toks {
                dcalls.push(Call3 {
                    target: *t,
                    allow_failure: true,
                    call_data: dec_sel.to_vec(),
                });
            }
            ranges.push((*idx, s, toks.len()));
        }
        let dres = aggregate3(client, multicall3, &dcalls, block)?;
        for (idx, s, n) in ranges {
            let decimals = (0..n).map(|k| {
                dres.get(s + k)
                    .filter(|r| r.success)
                    .and_then(|r| r.return_data.get(31).copied())
            });
            let Ok(v) = legacy_scaling_factors(decimals) else {
                continue;
            };
            if let Some(raw) = raws[idx].as_mut() {
                raw.sf = Some(v);
            }
        }
    }
    let mut out: Vec<Option<BalancerStableView>> = Vec::with_capacity(pools.len());
    for raw in raws {
        out.push(raw.and_then(|r| {
            r.sf.and_then(|sf| {
                decode_balancer_stable(r.pid, &r.pt, sf, &r.amp, &r.fee, r.bpt.as_deref()).ok()
            })
        }));
    }
    Ok(out)
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
