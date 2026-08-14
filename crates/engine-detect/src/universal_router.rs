//! Uniswap Universal Router `execute` decoder.
//!
//! UR's two entry points (selectors `0x24856bc3` and `0x3593564c`) take a packed
//! `commands` byte array plus a `bytes[] inputs` array. Each command byte selects a
//! command kind; the corresponding `inputs[i]` carries the ABI-encoded arguments.
//!
//! Commands we decode for the engine's purposes:
//!
//! | Opcode | Name | Args |
//! |---|---|---|
//! | 0x00 | V3_SWAP_EXACT_IN | (recipient, amountIn, amountOutMin, path, fromCaller) |
//! | 0x01 | V3_SWAP_EXACT_OUT | (recipient, amountOut, amountInMax, path, fromCaller) |
//! | 0x08 | V2_SWAP_EXACT_IN | (recipient, amountIn, amountOutMin, path, fromCaller) |
//! | 0x09 | V2_SWAP_EXACT_OUT | (recipient, amountOut, amountInMax, path, fromCaller) |
//!
//! Anything else is ignored (return an empty intent list); the engine will pick up the
//! state change via REVM execution if the pool is in the warm DB.

use alloy_primitives::U256;
use arrayvec::ArrayVec;

use crate::router_decoder::{
    decode_v3_path, DecodeError, DecodedHop, Direction, RouteIntent, MAX_DECODED_HOPS,
};
use crate::selectors::SelectorClass;

const CMD_V3_SWAP_EXACT_IN: u8 = 0x00;
const CMD_V3_SWAP_EXACT_OUT: u8 = 0x01;
const CMD_V2_SWAP_EXACT_IN: u8 = 0x08;
const CMD_V2_SWAP_EXACT_OUT: u8 = 0x09;

/// Decode Universal Router `execute` calldata into one or more `RouteIntent`s.
///
/// The selector class must be `SelectorClass::Aggregator`. The selector tells us whether
/// the body starts with `(bytes commands, bytes[] inputs)` (0x24856bc3) or
/// `(bytes commands, bytes[] inputs, uint256 deadline)` (0x3593564c).
pub fn decode_universal_router(
    selector: [u8; 4],
    calldata: &[u8],
) -> Result<ArrayVec<RouteIntent, MAX_DECODED_HOPS>, DecodeError> {
    if calldata.len() < 4 {
        return Err(DecodeError::TooShort);
    }
    let body = &calldata[4..];
    let (commands_offset_field, inputs_offset_field) = match selector {
        [0x24, 0x85, 0x6b, 0xc3] => (0usize, 32usize),
        [0x35, 0x93, 0x56, 0x4c] => (0usize, 32usize),
        _ => return Err(DecodeError::UnknownSelector),
    };
    if body.len() < 64 {
        return Err(DecodeError::TooShort);
    }
    let commands_offset = read_offset(&body[commands_offset_field..commands_offset_field + 32])?;
    let inputs_offset = read_offset(&body[inputs_offset_field..inputs_offset_field + 32])?;
    if body.len() < commands_offset + 32 || body.len() < inputs_offset + 32 {
        return Err(DecodeError::Malformed);
    }
    let commands_len = read_offset(&body[commands_offset..commands_offset + 32])?;
    let commands_start = commands_offset + 32;
    if body.len() < commands_start + commands_len {
        return Err(DecodeError::Malformed);
    }
    let commands = &body[commands_start..commands_start + commands_len];

    let inputs_len = read_offset(&body[inputs_offset..inputs_offset + 32])?;
    if commands.len() != inputs_len {
        // UR requires one input per command. If they don't match, the calldata is
        // malformed and we drop the whole frame.
        return Err(DecodeError::Malformed);
    }
    let mut input_offsets: Vec<usize> = Vec::with_capacity(inputs_len);
    for i in 0..inputs_len {
        let pos = inputs_offset + 32 + i * 32;
        if body.len() < pos + 32 {
            return Err(DecodeError::Malformed);
        }
        let rel = read_offset(&body[pos..pos + 32])?;
        // The inner offsets are relative to the start of the `inputs` array's tail data,
        // which begins at `inputs_offset + 32 + inputs_len * 32`. To resolve to an
        // absolute body offset we add the tail start.
        input_offsets.push(inputs_offset + 32 + rel);
    }

    let mut intents: ArrayVec<RouteIntent, MAX_DECODED_HOPS> = ArrayVec::new();
    for (i, &cmd) in commands.iter().enumerate() {
        // Official Uniswap `Commands.sol`: `COMMAND_TYPE_MASK = 0x3f`… no — the type mask is
        // `0x7f` (low 7 bits); bit 0x80 is `FLAG_ALLOW_REVERT`. Mask the low 7 bits so a command
        // whose 0x40 bit is set (e.g. PERMIT2_PERMIT_BATCH = 0x03 vs the 0x40-range opcodes)
        // isn't silently folded onto a different opcode.
        let masked = cmd & 0x7f; // low 7 bits = opcode; 0x80 = FLAG_ALLOW_REVERT.
        let input_off = input_offsets[i];
        // Each input is itself an ABI `bytes` field: length(32) || data(padded).
        if body.len() < input_off + 32 {
            return Err(DecodeError::Malformed);
        }
        let inner_len = read_offset(&body[input_off..input_off + 32])?;
        let inner_start = input_off + 32;
        if body.len() < inner_start + inner_len {
            return Err(DecodeError::Malformed);
        }
        let input_body = &body[inner_start..inner_start + inner_len];
        match masked {
            CMD_V3_SWAP_EXACT_IN => {
                if let Some(intent) = decode_v3_swap_in(input_body)? {
                    let _ = intents.try_push(intent);
                }
            }
            CMD_V3_SWAP_EXACT_OUT => {
                if let Some(intent) = decode_v3_swap_out(input_body)? {
                    let _ = intents.try_push(intent);
                }
            }
            CMD_V2_SWAP_EXACT_IN => {
                if let Some(intent) = decode_v2_swap_in(input_body)? {
                    let _ = intents.try_push(intent);
                }
            }
            CMD_V2_SWAP_EXACT_OUT => {
                if let Some(intent) = decode_v2_swap_out(input_body)? {
                    let _ = intents.try_push(intent);
                }
            }
            _ => {}
        }
    }
    Ok(intents)
}

fn decode_v3_swap_in(body: &[u8]) -> Result<Option<RouteIntent>, DecodeError> {
    // (address recipient, uint256 amountIn, uint256 amountOutMin, bytes path, bool fromCaller)
    if body.len() < 32 * 5 {
        return Err(DecodeError::TooShort);
    }
    let amount_in = read_u256(&body[32..64])?;
    let amount_out_min = read_u256(&body[64..96])?;
    let path_offset = read_offset(&body[96..128])?;
    let hops = decode_v3_path(body, path_offset)?;
    Ok(Some(RouteIntent {
        hops,
        amount_in,
        amount_out_min,
        exact_input: true,
    }))
}

fn decode_v3_swap_out(body: &[u8]) -> Result<Option<RouteIntent>, DecodeError> {
    // [Codex#1] V3_SWAP_EXACT_OUT is exact-OUTPUT: (recipient, amountOut, amountInMax, path, payer).
    // The INPUT amount is NOT in the calldata. We still DECODE it (so the route-intent inventory
    // characterizes the swap), tagged exact_input = false with amount_in = 0 as the "input unknown"
    // sentinel. predict_route NEVER forward-applies an exact-output intent (its `!exact_input` guard
    // returns early) — doing so would commit the pool's STALE pre-swap state and fire a DirtyEvent
    // against stale prices. The touched pools are refreshed by the resync / shadow-REVM path. NB: the
    // V3 exact-output `path` is reverse-encoded (tokenOut -> tokenIn), so these hops are output-first;
    // that is fine because the intent is never forward-applied (a feed-latency back-solve would need a
    // reverse-tick kernel — tracked future work).
    if body.len() < 32 * 5 {
        return Err(DecodeError::TooShort);
    }
    let amount_out = read_u256(&body[32..64])?;
    let path_offset = read_offset(&body[96..128])?;
    let hops = decode_v3_path(body, path_offset)?;
    Ok(Some(RouteIntent {
        hops,
        amount_in: U256::ZERO,
        amount_out_min: amount_out,
        exact_input: false,
    }))
}

fn decode_v2_swap_in(body: &[u8]) -> Result<Option<RouteIntent>, DecodeError> {
    // (address recipient, uint256 amountIn, uint256 amountOutMin, address[] path, bool fromCaller)
    if body.len() < 32 * 5 {
        return Err(DecodeError::TooShort);
    }
    let amount_in = read_u256(&body[32..64])?;
    let amount_out_min = read_u256(&body[64..96])?;
    let path_offset = read_offset(&body[96..128])?;
    let path = decode_address_array(body, path_offset)?;
    if path.len() < 2 {
        return Ok(None);
    }
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    for w in path.windows(2) {
        hops.push(DecodedHop {
            pool: None,
            token_in: w[0],
            token_out: w[1],
            fee_pips: 0,
            direction: Direction::Unknown,
        });
    }
    Ok(Some(RouteIntent {
        hops,
        amount_in,
        amount_out_min,
        exact_input: true,
    }))
}

fn decode_v2_swap_out(body: &[u8]) -> Result<Option<RouteIntent>, DecodeError> {
    // [Codex#1] V2_SWAP_EXACT_OUT is exact-OUTPUT: (recipient, amountOut, amountInMax, path, payer).
    // The INPUT amount is NOT in the calldata. We still DECODE it (so the route-intent inventory
    // characterizes the swap), tagged exact_input = false with amount_in = 0 as the "input unknown"
    // sentinel. predict_route NEVER forward-applies an exact-output intent (its `!exact_input` guard) —
    // doing so would commit the pool's STALE pre-swap state. The touched pools are refreshed by the
    // resync / shadow-REVM path. (The V2 kernel has getAmountIn, so a feed-latency back-solve from the
    // known output is feasible future work.)
    if body.len() < 32 * 5 {
        return Err(DecodeError::TooShort);
    }
    let amount_in_max = read_u256(&body[64..96])?;
    let path_offset = read_offset(&body[96..128])?;
    let path = decode_address_array(body, path_offset)?;
    if path.len() < 2 {
        return Ok(None);
    }
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    for w in path.windows(2) {
        hops.push(DecodedHop {
            pool: None,
            token_in: w[0],
            token_out: w[1],
            fee_pips: 0,
            direction: Direction::Unknown,
        });
    }
    Ok(Some(RouteIntent {
        hops,
        amount_in: U256::ZERO,
        amount_out_min: amount_in_max,
        exact_input: false,
    }))
}

// ABI helpers (mirror router_decoder.rs).

fn read_u256(bytes: &[u8]) -> Result<U256, DecodeError> {
    if bytes.len() < 32 {
        return Err(DecodeError::TooShort);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes[..32]);
    Ok(U256::from_be_bytes(arr))
}

fn read_offset(bytes: &[u8]) -> Result<usize, DecodeError> {
    let v = read_u256(bytes)?;
    if v.bit_len() > 32 {
        return Err(DecodeError::Malformed);
    }
    Ok(v.as_limbs()[0] as usize)
}

fn read_address(bytes: &[u8]) -> Result<[u8; 20], DecodeError> {
    if bytes.len() < 32 {
        return Err(DecodeError::TooShort);
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&bytes[12..32]);
    Ok(a)
}

fn decode_address_array(body: &[u8], offset: usize) -> Result<Vec<[u8; 20]>, DecodeError> {
    if body.len() < offset + 32 {
        return Err(DecodeError::Malformed);
    }
    let len = read_offset(&body[offset..offset + 32])?;
    if len > MAX_DECODED_HOPS + 1 {
        return Err(DecodeError::TooManyHops);
    }
    let data_start = offset + 32;
    if body.len() < data_start + len * 32 {
        return Err(DecodeError::Malformed);
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let a = read_address(&body[data_start + i * 32..data_start + (i + 1) * 32])?;
        out.push(a);
    }
    Ok(out)
}

/// Silence unused-import warning when feature configurations strip the SelectorClass alias.
#[allow(dead_code)]
fn _link_selector_class(class: SelectorClass) -> SelectorClass {
    class
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad_u256(value: u128) -> [u8; 32] {
        U256::from(value).to_be_bytes()
    }

    fn pad_address(addr: &[u8; 20]) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[12..].copy_from_slice(addr);
        out
    }

    /// Build a minimal UR `execute(bytes commands, bytes[] inputs)` with a single
    /// V2_SWAP_EXACT_IN command. Verify the decoder extracts the path.
    #[test]
    fn decode_universal_router_v2_swap_exact_in() {
        // Build the V2_SWAP_EXACT_IN input: (recipient, amountIn, amountOutMin, path, fromCaller)
        let mut inner = Vec::new();
        inner.extend_from_slice(&pad_address(&[0xee; 20])); // recipient
        inner.extend_from_slice(&pad_u256(1_000_000)); // amountIn
        inner.extend_from_slice(&pad_u256(0)); // amountOutMin
        inner.extend_from_slice(&pad_u256(160)); // path offset (5 head fields × 32 = 160)
        inner.extend_from_slice(&pad_u256(1)); // fromCaller = true
        inner.extend_from_slice(&pad_u256(2)); // path length
        inner.extend_from_slice(&pad_address(&[0xaa; 20]));
        inner.extend_from_slice(&pad_address(&[0xbb; 20]));
        // Build outer execute body: (bytes commands, bytes[] inputs)
        // commands_offset @ 0, inputs_offset @ 32
        // commands_tail starts at 64
        let commands_offset = 64u128;
        // Inputs tail starts after the commands tail (32-byte len + 32-byte padded commands).
        let inputs_offset = commands_offset + 32 + 32;
        let mut body = Vec::new();
        body.extend_from_slice(&pad_u256(commands_offset)); // commands offset
        body.extend_from_slice(&pad_u256(inputs_offset)); // inputs offset
        body.extend_from_slice(&pad_u256(1)); // commands length
        let mut command_bytes = [0u8; 32];
        command_bytes[0] = CMD_V2_SWAP_EXACT_IN;
        body.extend_from_slice(&command_bytes); // packed command byte
        body.extend_from_slice(&pad_u256(1)); // inputs length
        body.extend_from_slice(&pad_u256(32)); // inner-input offset (relative to inputs tail start)
        body.extend_from_slice(&pad_u256(inner.len() as u128)); // inner-input length
        body.extend_from_slice(&inner);

        let mut calldata = Vec::new();
        calldata.extend_from_slice(&[0x24, 0x85, 0x6b, 0xc3]);
        calldata.extend_from_slice(&body);

        let intents = decode_universal_router([0x24, 0x85, 0x6b, 0xc3], &calldata).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].amount_in, U256::from(1_000_000u64));
        assert_eq!(intents[0].hops.len(), 1);
        assert_eq!(intents[0].hops[0].token_in, [0xaa; 20]);
        assert_eq!(intents[0].hops[0].token_out, [0xbb; 20]);
    }

    /// B4: Uniswap `Commands.sol` `COMMAND_TYPE_MASK = 0x7f` (low 7 bits); 0x80 = FLAG_ALLOW_REVERT.
    /// The opcode mask used in `decode_universal_router` must be `& 0x7f`, not `& 0x3f`, so that
    /// the 0x40 bit survives. A command byte 0x40 must mask to 0x40 (a distinct opcode), and a
    /// command byte 0x81 (0x01 | FLAG_ALLOW_REVERT) must mask to 0x01.
    #[test]
    fn command_type_mask_is_low_7_bits() {
        assert_eq!(
            0x40u8 & 0x7f,
            0x40,
            "0x40 must not be folded to 0x00 by the mask"
        );
        assert_eq!(
            0x81u8 & 0x7f,
            0x01,
            "0x81 (allow-revert flag | V3_SWAP_EXACT_OUT) must mask to 0x01"
        );
        // Guard against a regression back to the 6-bit mask, which would alias 0x40 → 0x00.
        assert_ne!(0x40u8 & 0x7f, 0x40u8 & 0x3f);
    }
}
