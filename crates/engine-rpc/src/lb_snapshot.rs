//! Trader Joe Liquidity Book (LB v2.1/v2.2) pool state snapshot reader.
//!
//! Mirrors the reference `fetch_single_lb_state_at_block` loader. Reads, via the
//! `ILBPair` interface:
//! * `getActiveId() -> uint24` — current active bin id.
//! * `getBinStep() -> uint16` — bin step in basis points.
//! * `getStaticFeeParameters() -> (uint16 baseFactor, uint16 filterPeriod,
//!    uint16 decayPeriod, uint16 reductionFactor, uint24 variableFeeControl,
//!    uint16 protocolShare, uint24 maxVolatilityAccumulator)`.
//! * `getVariableFeeParameters() -> (uint24 volatilityAccumulator,
//!    uint24 volatilityReference, uint24 idReference, uint40 timeOfLastUpdate)`.
//! * `getBin(uint24 id) -> (uint128 binReserveX, uint128 binReserveY)` for the
//!   active bin plus a window of ±`MAX_TRAVERSAL_BINS` neighbouring bins.
//!
//! The bin-window `getBin` fan-out is collapsed into a single Multicall3
//! `aggregate3` round-trip (same model as `fetch_v3_tick_window_multicall`) with
//! a per-bin sequential `eth_call` fallback.
//!
//! The math kernel (`engine_amm::trader_joe_lb`) consumes the resulting `LBState`.

use std::collections::BTreeMap;
use std::sync::Arc;

use engine_amm::trader_joe_lb::{BinData, LBState, MAX_TRAVERSAL_BINS};
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

fn get_active_id_selector() -> [u8; 4] {
    selector(b"getActiveId()")
}

fn get_bin_step_selector() -> [u8; 4] {
    selector(b"getBinStep()")
}

fn get_static_fee_selector() -> [u8; 4] {
    selector(b"getStaticFeeParameters()")
}

fn get_variable_fee_selector() -> [u8; 4] {
    selector(b"getVariableFeeParameters()")
}

fn get_bin_selector() -> [u8; 4] {
    selector(b"getBin(uint24)")
}

/// Build the `getBin(uint24 id)` calldata: selector + the bin id right-aligned in
/// a 32-byte word (uint24 is unsigned — no sign extension).
fn get_bin_calldata(id: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 32);
    data.extend_from_slice(&get_bin_selector());
    let mut word = [0u8; 32];
    word[29] = (id >> 16) as u8;
    word[30] = (id >> 8) as u8;
    word[31] = id as u8;
    data.extend_from_slice(&word);
    data
}

/// Decode a `getBin` response `(uint128 binReserveX, uint128 binReserveY)`.
fn decode_bin(resp: &[u8]) -> Option<(u128, u128)> {
    if resp.len() < 64 {
        return None;
    }
    let mut rx = [0u8; 16];
    rx.copy_from_slice(&resp[16..32]);
    let mut ry = [0u8; 16];
    ry.copy_from_slice(&resp[48..64]);
    Some((u128::from_be_bytes(rx), u128::from_be_bytes(ry)))
}

/// Read a uint at word `idx` (each word is 32 bytes), interpreting the low 8
/// bytes as a `u64`. LB fee params all fit far below 2^64.
fn word_u64(resp: &[u8], idx: usize) -> Result<u64, RpcError> {
    let off = idx * 32;
    if resp.len() < off + 32 {
        return Err(RpcError::Decode("lb fee param word short".into()));
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&resp[off + 24..off + 32]);
    Ok(u64::from_be_bytes(buf))
}

/// Fetch a complete LB pool snapshot at the given block.
///
/// `block_number` is recorded into `LBState.block_number` for observability and
/// the cache-coherency book-keeping the snapshot/resync paths rely on; pass the
/// numeric head matching `block`.
pub fn fetch_lb_state(
    client: &ArbitrumRpcClient,
    pool: &Address,
    block: BlockTag,
    block_number: u64,
) -> Result<LBState, RpcError> {
    // 1. Scalar views.
    let active_resp = client.eth_call(pool, &get_active_id_selector(), block)?;
    let active_id = word_u64(&active_resp, 0)? as u32;

    let bin_step_resp = client.eth_call(pool, &get_bin_step_selector(), block)?;
    let bin_step = word_u64(&bin_step_resp, 0)? as u16;

    let static_resp = client.eth_call(pool, &get_static_fee_selector(), block)?;
    if static_resp.len() < 7 * 32 {
        return Err(RpcError::Decode("lb static fee params too short".into()));
    }
    let base_factor = word_u64(&static_resp, 0)? as u16;
    let filter_period = word_u64(&static_resp, 1)? as u16;
    let decay_period = word_u64(&static_resp, 2)? as u16;
    let reduction_factor = word_u64(&static_resp, 3)? as u16;
    let variable_fee_control = word_u64(&static_resp, 4)? as u32;
    let protocol_share = word_u64(&static_resp, 5)? as u16;
    let max_volatility_accumulator = word_u64(&static_resp, 6)? as u32;

    let var_resp = client.eth_call(pool, &get_variable_fee_selector(), block)?;
    if var_resp.len() < 4 * 32 {
        return Err(RpcError::Decode("lb variable fee params too short".into()));
    }
    let volatility_accumulator = word_u64(&var_resp, 0)? as u32;
    let volatility_reference = word_u64(&var_resp, 1)? as u32;
    let id_reference = word_u64(&var_resp, 2)? as u32;
    let time_of_last_update = word_u64(&var_resp, 3)?;

    // 2. Active bin reserves.
    let active_bin_resp = client.eth_call(pool, &get_bin_calldata(active_id), block)?;
    let (reserve_x, reserve_y) = decode_bin(&active_bin_resp)
        .ok_or_else(|| RpcError::Decode("lb active bin short".into()))?;

    // 3. Bin window — ±MAX_TRAVERSAL_BINS around active id. Try one Multicall3
    //    aggregate3 first; fall back to sequential per-bin eth_calls on error.
    let window = MAX_TRAVERSAL_BINS;
    let start = active_id.saturating_sub(window);
    let end = active_id.saturating_add(window);
    let bin_ids: Vec<u32> = (start..=end).filter(|&id| id != active_id).collect();

    let bin_map = fetch_bin_window(client, pool, &bin_ids, block);

    Ok(LBState {
        active_id,
        bin_step,
        reserve_x,
        reserve_y,
        base_factor,
        filter_period,
        decay_period,
        reduction_factor,
        variable_fee_control,
        protocol_share,
        max_volatility_accumulator,
        volatility_accumulator,
        volatility_reference,
        id_reference,
        time_of_last_update,
        block_number,
        log_index: 0,
        stale: false,
        bin_map: Arc::new(bin_map),
    })
}

/// Fetch reserves for a set of bin ids, batched via Multicall3 with a sequential
/// fallback. Empty bins (both reserves zero) are omitted, matching the reference
/// loader and the kernel's gap-skip traversal.
fn fetch_bin_window(
    client: &ArbitrumRpcClient,
    pool: &Address,
    bin_ids: &[u32],
    block: BlockTag,
) -> BTreeMap<u32, BinData> {
    let mut bin_map = BTreeMap::new();
    if bin_ids.is_empty() {
        return bin_map;
    }

    let calls: Vec<crate::multicall::Call3> = bin_ids
        .iter()
        .map(|&id| crate::multicall::Call3 {
            target: *pool,
            allow_failure: true,
            call_data: get_bin_calldata(id),
        })
        .collect();

    match crate::multicall::aggregate3(
        client,
        &crate::multicall::MULTICALL3_ARBITRUM,
        &calls,
        block,
    ) {
        Ok(results) => {
            for (&id, result) in bin_ids.iter().zip(results.iter()) {
                if !result.success {
                    continue;
                }
                if let Some((rx, ry)) = decode_bin(&result.return_data) {
                    if rx > 0 || ry > 0 {
                        bin_map.insert(
                            id,
                            BinData {
                                reserve_x: rx,
                                reserve_y: ry,
                            },
                        );
                    }
                }
            }
        }
        Err(_) => {
            // Sequential fallback — one eth_call per bin.
            for &id in bin_ids {
                if let Ok(resp) = client.eth_call(pool, &get_bin_calldata(id), block) {
                    if let Some((rx, ry)) = decode_bin(&resp) {
                        if rx > 0 || ry > 0 {
                            bin_map.insert(
                                id,
                                BinData {
                                    reserve_x: rx,
                                    reserve_y: ry,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    bin_map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lb_selectors_match_keccak() {
        // Verified against keccak256(sig)[..4].
        assert_eq!(get_active_id_selector(), [0xdb, 0xe6, 0x5e, 0xdc]);
        assert_eq!(get_bin_step_selector(), [0x17, 0xf1, 0x1e, 0xcc]);
        assert_eq!(get_bin_selector(), [0x0a, 0xbe, 0x96, 0x88]);
        assert_eq!(get_static_fee_selector(), [0x7c, 0xa0, 0xde, 0x30]);
        assert_eq!(get_variable_fee_selector(), [0x8d, 0x70, 0x24, 0xe5]);
    }

    #[test]
    fn get_bin_calldata_encodes_uint24() {
        // activeId 0x7ffff8 = 8388600 (the live 0xfc43aaf8 USDT0/USDC pair).
        let data = get_bin_calldata(0x7ffff8);
        assert_eq!(&data[0..4], &[0x0a, 0xbe, 0x96, 0x88]);
        assert_eq!(data[4 + 29], 0x7f);
        assert_eq!(data[4 + 30], 0xff);
        assert_eq!(data[4 + 31], 0xf8);
        // The high 29 bytes of the arg word are zero (uint24, no sign extension).
        assert!(data[4..4 + 29].iter().all(|&b| b == 0));
    }

    #[test]
    fn decode_bin_reads_two_uint128() {
        let mut resp = [0u8; 64];
        // reserveX = 10877514 in low 16 bytes of word 0.
        resp[16..32].copy_from_slice(&10_877_514u128.to_be_bytes());
        // reserveY = 5753683 in low 16 bytes of word 1.
        resp[48..64].copy_from_slice(&5_753_683u128.to_be_bytes());
        assert_eq!(decode_bin(&resp), Some((10_877_514, 5_753_683)));
    }
}
