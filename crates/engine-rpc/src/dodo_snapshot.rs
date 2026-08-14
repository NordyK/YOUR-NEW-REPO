//! DODO V2 pool state snapshot reader.
//!
//! Reads three views per pool:
//! * `_BASE_TOKEN_()` → `address`
//! * `_QUOTE_TOKEN_()` → `address`
//! * `getPMMStateForCall()` → `(uint i, uint K, uint B, uint Q, uint B0, uint Q0, uint R)`
//! * `getUserFeeRate(address)` → `(uint lpFeeRate, uint mtFeeRate)`
//!
//! The math kernel (`engine_amm::dodo_v2`) consumes the resulting `DodoState`.

use alloy_primitives::{Address as AmmAddress, U256};
use engine_amm::dodo_v2::{dodo_state_from_view, DodoPoolClass, DodoState};
use engine_feed::Address;

#[inline]
fn to_amm_addr(a: [u8; 20]) -> AmmAddress {
    AmmAddress::from(a)
}
use tiny_keccak::{Hasher, Keccak};

use crate::client::{ArbitrumRpcClient, BlockTag, RpcError};

fn selector(sig: &[u8]) -> [u8; 4] {
    let mut k = Keccak::v256();
    k.update(sig);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    [out[0], out[1], out[2], out[3]]
}

fn base_token_selector() -> [u8; 4] {
    selector(b"_BASE_TOKEN_()")
}

fn quote_token_selector() -> [u8; 4] {
    selector(b"_QUOTE_TOKEN_()")
}

fn pmm_state_selector() -> [u8; 4] {
    selector(b"getPMMStateForCall()")
}

fn user_fee_rate_selector() -> [u8; 4] {
    selector(b"getUserFeeRate(address)")
}

fn version_selector() -> [u8; 4] {
    selector(b"version()")
}

/// Map a DODO pool's on-chain `version()` string to its [`DodoPoolClass`].
///
/// DODO V2 pools self-identify: `"DVM 1.0.2"`, `"DPP 1.0.0"`, `"DPP Advanced 1.1.0"`,
/// `"DSP 1.0.0"`, etc. The PMM `getPMMStateForCall()` already returns resolved targets so
/// the quote math is class-agnostic, but the class is still recorded truthfully (DPP/DSP
/// pools were historically mislabelled `Dvm`, which is what produced the phantom). Returns
/// `None` if the response isn't a decodable ABI string (caller keeps its fallback class).
fn class_from_version(resp: &[u8]) -> Option<DodoPoolClass> {
    // ABI string: [offset:32][len:32][bytes...]; tolerate a bare/empty return.
    if resp.len() < 64 {
        return None;
    }
    let len = U256::from_be_bytes::<32>(resp[32..64].try_into().ok()?);
    let len: usize = len.try_into().ok()?;
    let end = 64usize.checked_add(len)?;
    if len == 0 || end > resp.len() {
        return None;
    }
    let s = core::str::from_utf8(&resp[64..end]).ok()?;
    // Order matters: check the more specific tokens first.
    if s.contains("DSP") {
        Some(DodoPoolClass::Dsp)
    } else if s.contains("DPP") {
        Some(DodoPoolClass::Dpp)
    } else if s.contains("DVM") {
        Some(DodoPoolClass::Dvm)
    } else {
        None
    }
}

fn decode_address(resp: &[u8]) -> Result<Address, RpcError> {
    if resp.len() < 32 {
        return Err(RpcError::Decode("dodo address word short".into()));
    }
    if resp[0..12].iter().any(|&b| b != 0) {
        return Err(RpcError::Decode("dodo address padding".into()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&resp[12..32]);
    Ok(a)
}

fn decode_u256(resp: &[u8], offset: usize) -> Result<U256, RpcError> {
    if resp.len() < offset + 32 {
        return Err(RpcError::Decode("dodo u256 word short".into()));
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&resp[offset..offset + 32]);
    Ok(U256::from_be_bytes(buf))
}

/// Decode a complete DODO V2 pool state from the four raw sub-returns the snapshot
/// reads: `_BASE_TOKEN_()`, `_QUOTE_TOKEN_()`, `getPMMStateForCall()`, and the
/// `getUserFeeRate(address(0))` view.
///
/// `fee_resp` is `None` when the fee call reverted (some pool variants don't expose
/// `getUserFeeRate`) — in that case the same conservative 30 bps fallback the
/// per-pool path used is applied. This is the SINGLE source of decode truth: both
/// the per-pool [`fetch_dodo_state`] and the batched [`fetch_dodo_states_batched`]
/// route every sub-return through here, so a batched `DodoState` is byte-for-byte
/// identical to the per-pool one (a DODO mis-decode historically produced phantom
/// profit, so the decode is deliberately shared, not re-implemented).
#[allow(clippy::too_many_arguments)]
/// [row-7] The getter calldatas the shadow worker runs against a DODO pool's REVM post-state —
/// identical to `fetch_dodo_state`'s reads (`_BASE_TOKEN_`, `_QUOTE_TOKEN_`, `getPMMStateForCall`,
/// `getUserFeeRate(address(0))`, `version()`). Feed the raw returns (in this order) to
/// [`decode_dodo_state`] with the pool's known [`DodoPoolClass`].
pub fn dodo_shadow_getters() -> [Vec<u8>; 5] {
    let mut user_fee = Vec::with_capacity(36);
    user_fee.extend_from_slice(&user_fee_rate_selector());
    user_fee.extend_from_slice(&[0u8; 32]); // getUserFeeRate(address(0))
    [
        base_token_selector().to_vec(),
        quote_token_selector().to_vec(),
        pmm_state_selector().to_vec(),
        user_fee,
        version_selector().to_vec(),
    ]
}

/// Decode a DODO pool's `DodoState` from the raw returns of its five getters (see
/// [`dodo_shadow_getters`] / `fetch_dodo_state`). Shared by the RPC resync and the [row-7] shadow
/// post-state path so both produce a byte-identical `DodoState`.
pub fn decode_dodo_state(
    pool: &Address,
    pool_class: DodoPoolClass,
    base_resp: &[u8],
    quote_resp: &[u8],
    pmm_resp: &[u8],
    fee_resp: Option<&[u8]>,
    version_resp: Option<&[u8]>,
    block_number: u64,
) -> Result<DodoState, RpcError> {
    let base_token = decode_address(base_resp)?;
    let quote_token = decode_address(quote_resp)?;
    // On-chain `version()` is authoritative for the class; the passed `pool_class` is only a
    // fallback for pools that don't expose a decodable version string.
    let pool_class = version_resp
        .and_then(class_from_version)
        .unwrap_or(pool_class);

    if pmm_resp.len() < 7 * 32 {
        return Err(RpcError::Decode("dodo pmm state too short".into()));
    }
    let i = decode_u256(pmm_resp, 0)?;
    let k = decode_u256(pmm_resp, 32)?;
    let b = decode_u256(pmm_resp, 64)?;
    let q = decode_u256(pmm_resp, 96)?;
    let b0 = decode_u256(pmm_resp, 128)?;
    let q0 = decode_u256(pmm_resp, 160)?;
    let r = decode_u256(pmm_resp, 192)?;

    let (lp_fee, mt_fee) = match fee_resp {
        Some(fee_resp) => {
            if fee_resp.len() < 64 {
                return Err(RpcError::Decode("dodo user fee rate short".into()));
            }
            (decode_u256(fee_resp, 0)?, decode_u256(fee_resp, 32)?)
        }
        None => {
            // Some pool variants don't expose getUserFeeRate — fall back to
            // a conservative 30 bps (3e15 / 1e18) total fee with 75/25 LP/mt split.
            (
                U256::from(2_250_000_000_000_000u64), // 22.5bps LP
                U256::from(750_000_000_000_000u64),   // 7.5bps mt
            )
        }
    };

    dodo_state_from_view(
        to_amm_addr(*pool),
        pool_class,
        to_amm_addr(base_token),
        to_amm_addr(quote_token),
        i,
        k,
        b,
        q,
        b0,
        q0,
        r,
        lp_fee,
        mt_fee,
        block_number,
        0,
    )
    .map_err(|e| RpcError::Decode(format!("dodo state assembly: {:?}", e)))
}

/// Fetch a complete DODO V2 pool snapshot at the given block.
///
/// `pool_class` must be known from discovery (which factory emitted the New*
/// event). We can't reliably infer it from on-chain state alone.
pub fn fetch_dodo_state(
    client: &ArbitrumRpcClient,
    pool: &Address,
    pool_class: DodoPoolClass,
    block: BlockTag,
    block_number: u64,
) -> Result<DodoState, RpcError> {
    let base_resp = client.eth_call(pool, &base_token_selector(), block)?;
    let quote_resp = client.eth_call(pool, &quote_token_selector(), block)?;
    let pmm_resp = client.eth_call(pool, &pmm_state_selector(), block)?;

    // Read user fee rate with zero address as the "trader" — this gives the
    // global fee schedule. A revert here is tolerated (fallback fee schedule).
    let mut user_fee_calldata = Vec::with_capacity(4 + 32);
    user_fee_calldata.extend_from_slice(&user_fee_rate_selector());
    user_fee_calldata.extend_from_slice(&[0u8; 32]); // address(0) padded
    let fee_resp = client.eth_call(pool, &user_fee_calldata, block).ok();

    // version() decides the pool class on-chain (DVM/DPP/DSP); a revert is tolerated.
    let version_resp = client.eth_call(pool, &version_selector(), block).ok();

    decode_dodo_state(
        pool,
        pool_class,
        &base_resp,
        &quote_resp,
        &pmm_resp,
        fee_resp.as_deref(),
        version_resp.as_deref(),
        block_number,
    )
}

/// Batch the four DODO reads (`_BASE_TOKEN_`, `_QUOTE_TOKEN_`, `getPMMStateForCall`,
/// `getUserFeeRate(address(0))`) for many pools through ONE Multicall3 `aggregate3`.
/// Returns one `Option` per input pool (`None` if any required sub-call failed or
/// decoded short), in input order.
///
/// The transport is batched; each pool's four sub-returns are decoded with the SAME
/// [`decode_dodo_state`] the per-pool [`fetch_dodo_state`] uses — so a batched
/// `DodoState` is byte-for-byte identical to the per-pool one. The `getUserFeeRate`
/// sub-call is allowed to revert (per-call success flag), mirroring the per-pool
/// fee-fallback path exactly. `pools` carries each pool's discovery-derived
/// [`DodoPoolClass`] (we can't infer it from on-chain state).
pub fn fetch_dodo_states_batched(
    client: &ArbitrumRpcClient,
    multicall3: &Address,
    pools: &[(Address, DodoPoolClass)],
    block: BlockTag,
    block_number: u64,
) -> Result<Vec<Option<DodoState>>, RpcError> {
    use crate::multicall::{aggregate3, Call3};
    // 5 sub-calls per pool, in a fixed order: base, quote, pmm, userFeeRate, version.
    let base_sel = base_token_selector();
    let quote_sel = quote_token_selector();
    let pmm_sel = pmm_state_selector();
    let fee_sel = user_fee_rate_selector();
    let ver_sel = version_selector();
    let mut calls: Vec<Call3> = Vec::with_capacity(pools.len() * 5);
    for (addr, _class) in pools {
        calls.push(Call3 {
            target: *addr,
            allow_failure: true,
            call_data: base_sel.to_vec(),
        });
        calls.push(Call3 {
            target: *addr,
            allow_failure: true,
            call_data: quote_sel.to_vec(),
        });
        calls.push(Call3 {
            target: *addr,
            allow_failure: true,
            call_data: pmm_sel.to_vec(),
        });
        let mut fee_data = Vec::with_capacity(4 + 32);
        fee_data.extend_from_slice(&fee_sel);
        fee_data.extend_from_slice(&[0u8; 32]); // address(0) padded
        calls.push(Call3 {
            target: *addr,
            allow_failure: true,
            call_data: fee_data,
        });
        calls.push(Call3 {
            target: *addr,
            allow_failure: true,
            call_data: ver_sel.to_vec(),
        });
    }
    let results = aggregate3(client, multicall3, &calls, block)?;
    let mut out: Vec<Option<DodoState>> = Vec::with_capacity(pools.len());
    for (i, (addr, class)) in pools.iter().enumerate() {
        out.push(decode_dodo_block(
            addr,
            *class,
            &results,
            i * 5,
            block_number,
        ));
    }
    Ok(out)
}

/// Extract+decode ONE pool's `DodoState` from its 4-call block inside an `aggregate3`
/// result slice (`base` = the block's first call index: base, quote, pmm, fee).
/// Returns `None` if any of the required base/quote/pmm sub-calls reverted (the per-pool
/// path errors in that case too); the fee sub-call is allowed to revert and triggers the
/// shared fallback schedule inside [`decode_dodo_state`]. Factored out so the batched-vs-
/// per-pool decode-equivalence test can exercise the exact extraction path.
fn decode_dodo_block(
    addr: &Address,
    class: DodoPoolClass,
    results: &[crate::multicall::CallResult],
    base: usize,
    block_number: u64,
) -> Option<DodoState> {
    let base_r = results.get(base);
    let quote_r = results.get(base + 1);
    let pmm_r = results.get(base + 2);
    let fee_r = results.get(base + 3);
    let ver_r = results.get(base + 4);
    match (base_r, quote_r, pmm_r) {
        (Some(b), Some(q), Some(p)) if b.success && q.success && p.success => {
            let fee_bytes: Option<&[u8]> = match fee_r {
                Some(f) if f.success => Some(&f.return_data),
                _ => None,
            };
            let ver_bytes: Option<&[u8]> = match ver_r {
                Some(v) if v.success => Some(&v.return_data),
                _ => None,
            };
            decode_dodo_state(
                addr,
                class,
                &b.return_data,
                &q.return_data,
                &p.return_data,
                fee_bytes,
                ver_bytes,
                block_number,
            )
            .ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a 32-byte ABI word holding a right-aligned address.
    fn addr_word(a: [u8; 20]) -> Vec<u8> {
        let mut w = vec![0u8; 32];
        w[12..32].copy_from_slice(&a);
        w
    }

    // Build a 32-byte ABI word holding a u128 value right-aligned.
    fn u_word(v: u128) -> Vec<u8> {
        let mut w = vec![0u8; 32];
        w[16..32].copy_from_slice(&v.to_be_bytes());
        w
    }

    // A realistic getPMMStateForCall() return: 7 packed u256 words
    // (i, K, B, Q, B0, Q0, R).
    fn pmm_blob() -> Vec<u8> {
        let mut out = Vec::with_capacity(7 * 32);
        // i (price index, 1e18 scale), K (0.1e18), B/Q reserves, B0/Q0 targets, R (status).
        for v in [
            1_000_000_000_000_000_000u128,   // i = 1.0
            100_000_000_000_000_000u128,     // K = 0.1
            500_000_000_000_000_000_000u128, // B reserve
            480_000_000_000_000_000_000u128, // Q reserve
            500_000_000_000_000_000_000u128, // B0 target
            500_000_000_000_000_000_000u128, // Q0 target
            1u128,                           // R = 1 (ABOVE_ONE etc.)
        ] {
            out.extend_from_slice(&u_word(v));
        }
        out
    }

    fn fee_blob() -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(&u_word(2_250_000_000_000_000)); // lp fee 22.5bps
        out.extend_from_slice(&u_word(750_000_000_000_000)); // mt fee 7.5bps
        out
    }

    // ABI-encode a `version()` string return: [offset=0x20][len][bytes padded].
    fn version_blob(s: &str) -> Vec<u8> {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        out.extend_from_slice(&u_word(0x20)); // offset
        out.extend_from_slice(&u_word(bytes.len() as u128)); // length
        let mut padded = bytes.to_vec();
        padded.resize(bytes.len().div_ceil(32) * 32, 0);
        out.extend_from_slice(&padded);
        out
    }

    use crate::multicall::CallResult;

    fn ok(data: Vec<u8>) -> CallResult {
        CallResult {
            success: true,
            return_data: data,
        }
    }
    fn reverted() -> CallResult {
        CallResult {
            success: false,
            return_data: Vec::new(),
        }
    }

    /// The batched path MUST decode to a `DodoState` byte-for-byte identical to the
    /// per-pool path on the SAME captured sub-returns. A DODO mis-decode historically
    /// produced phantom profit, so this guards that the batch only changes transport,
    /// never the decode. We build a synthetic `aggregate3` result block, run it through
    /// the EXACT batched extraction (`decode_dodo_block`), and assert it equals the
    /// per-pool `decode_dodo_state` fed the identical raw blobs.
    #[test]
    fn batched_decode_equals_per_pool_decode() {
        let pool: Address = [0x42; 20];
        let base_resp = addr_word([0xAA; 20]);
        let quote_resp = addr_word([0xBB; 20]);
        let pmm_resp = pmm_blob();
        let fee_resp = fee_blob();
        let ver_resp = version_blob("DPP 1.0.0");
        let block_number = 123_456_789u64;
        // Pass DVM as the fallback; version() should override it to DPP in BOTH paths.
        let class = DodoPoolClass::Dvm;

        // Per-pool path: exactly what fetch_dodo_state calls.
        let per_pool = decode_dodo_state(
            &pool,
            class,
            &base_resp,
            &quote_resp,
            &pmm_resp,
            Some(&fee_resp),
            Some(&ver_resp),
            block_number,
        )
        .expect("per-pool decode");
        assert_eq!(
            per_pool.pool_class,
            DodoPoolClass::Dpp,
            "version() must override class"
        );

        // Batched path: the real extraction over a synthetic aggregate3 result block.
        let results = vec![
            ok(base_resp.clone()),
            ok(quote_resp.clone()),
            ok(pmm_resp.clone()),
            ok(fee_resp.clone()),
            ok(ver_resp.clone()),
        ];
        let batched =
            decode_dodo_block(&pool, class, &results, 0, block_number).expect("batched decode");

        assert_eq!(
            per_pool, batched,
            "batched DODO decode diverged from per-pool"
        );
    }

    /// When the fee sub-call reverts, the batched path drops it to the fallback schedule
    /// via the per-call success flag — identical to the per-pool path (which passes
    /// `None` on a reverted fee read).
    #[test]
    fn fee_revert_fallback_is_identical() {
        let pool: Address = [0x42; 20];
        let base_resp = addr_word([0xAA; 20]);
        let quote_resp = addr_word([0xBB; 20]);
        let pmm_resp = pmm_blob();
        let block_number = 42u64;
        let class = DodoPoolClass::Dpp;

        // No version() here → both paths must fall back to the passed class (DPP).
        let per_pool = decode_dodo_state(
            &pool,
            class,
            &base_resp,
            &quote_resp,
            &pmm_resp,
            None,
            None,
            block_number,
        )
        .expect("per-pool decode (fee revert)");

        // Batched: fee sub-call reverted (success=false) → batched extraction treats it
        // as None → same fallback.
        let results = vec![
            ok(base_resp.clone()),
            ok(quote_resp.clone()),
            ok(pmm_resp.clone()),
            reverted(),
        ];
        let batched = decode_dodo_block(&pool, class, &results, 0, block_number)
            .expect("batched decode (fee revert)");
        assert_eq!(per_pool, batched);
    }

    /// A reverted base/quote/pmm sub-call must yield `None` in the batched path (the
    /// per-pool path errors), so the pool is skipped — never silently mis-decoded.
    #[test]
    fn required_revert_yields_none() {
        let pool: Address = [0x42; 20];
        let results = vec![
            reverted(), // base reverted
            ok(addr_word([0xBB; 20])),
            ok(pmm_blob()),
            ok(fee_blob()),
        ];
        assert!(decode_dodo_block(&pool, DodoPoolClass::Dvm, &results, 0, 1).is_none());
    }
}
