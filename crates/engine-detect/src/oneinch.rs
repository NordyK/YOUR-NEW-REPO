//! 1inch v6 AggregationRouter decoders.
//!
//! Plan §0.2 forbids *targeting* aggregator transactions for backrun. We decode
//! these for the **state-update** path only — i.e. so the engine can mark the
//! touched pool dirty and re-evaluate our own arbitrage routes against the new
//! state. The engine never builds a candidate that mimics a 1inch trade.
//!
//! Decoded selectors (all verified via `cast sig "<signature>"` against the live 1inch
//! AggregationRouterV6 ABI; v6 packs the recipient/flags into `uint256`, never `address`):
//!
//! | Selector   | Function    | Signature                                                | Pools |
//! |------------|-------------|----------------------------------------------------------|-------|
//! | 0x83800a8e | unoswap     | unoswap(uint256,uint256,uint256,uint256)                 | 1     |
//! | 0xe2c95c82 | unoswapTo   | unoswapTo(uint256,uint256,uint256,uint256,uint256)       | 1     |
//! | 0x8770ba91 | unoswap2    | unoswap2(uint256,uint256,uint256,uint256,uint256)        | 2     |
//! | 0x19367472 | unoswap3    | unoswap3(uint256,uint256,uint256,uint256,uint256,uint256)| 3     |
//! | 0xa76dfc3b | ethUnoswap  | ethUnoswap(uint256,uint256)                              | 1 (ETH) |
//! | 0x89af926a | ethUnoswap2 | ethUnoswap2(uint256,uint256,uint256)                     | 2 (ETH) |
//! | 0x188ac35d | ethUnoswap3 | ethUnoswap3(uint256,uint256,uint256,uint256)             | 3 (ETH) |
//!
//! Each `dex` word packs the pool address (low 160 bits) plus flags:
//! * bit 247 — `zeroForOne` direction (0 = token0→token1 within the pool).
//! * bits 248+ — protocol flags (V2, V3, Curve, etc.) the executor uses to
//!   pick the right swap path. We don't need them for state-update.
//!
//! The token0/token1 of each hop are not in the calldata — the executor reads
//! them from the pool. For our purposes the `predict_route` path resolves
//! direction from the pool's `token0`/`token1` plus the `zeroForOne` flag we
//! decode here, so we pass `Direction::ZeroForOne` / `OneForZero` directly
//! instead of `Direction::Unknown`.

use alloy_primitives::U256;
use arrayvec::ArrayVec;

use crate::router_decoder::{DecodeError, DecodedHop, Direction, RouteIntent, MAX_DECODED_HOPS};

/// Bit position of the `zeroForOne` flag inside a `dex` word.
const ZERO_FOR_ONE_BIT: usize = 247;

fn read_u256(w: &[u8]) -> Result<U256, DecodeError> {
    if w.len() < 32 {
        return Err(DecodeError::TooShort);
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&w[..32]);
    Ok(U256::from_be_bytes(buf))
}

/// Extract the pool address (low 20 bytes) and `zeroForOne` flag from a packed
/// `dex` word.
fn unpack_dex(word: &[u8]) -> Result<([u8; 20], Direction), DecodeError> {
    if word.len() < 32 {
        return Err(DecodeError::TooShort);
    }
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&word[12..32]);
    // The `zeroForOne` bit lives in the upper limb of the big-endian 256-bit word
    // (`word[0]` is most-significant). Derive the byte index and mask from
    // `ZERO_FOR_ONE_BIT` rather than hard-coding them: for bit 247 this is byte
    // (255 - 247) / 8 = 1 and mask 1 << (247 % 8) = 0x80.
    let byte_idx = (255 - ZERO_FOR_ONE_BIT) / 8;
    let mask = 1u8 << (ZERO_FOR_ONE_BIT % 8);
    let z = (word[byte_idx] & mask) != 0;
    Ok((
        addr,
        if z {
            Direction::ZeroForOne
        } else {
            Direction::OneForZero
        },
    ))
}

/// Decode `unoswap(uint256 token, uint256 amount, uint256 minReturn, uint256 dex)`.
pub fn decode_unoswap(calldata: &[u8]) -> Result<RouteIntent, DecodeError> {
    if calldata.len() < 4 + 32 * 4 {
        return Err(DecodeError::TooShort);
    }
    let body = &calldata[4..];
    // body[0..32] = token (input token — uint256 with address in low 160 bits)
    let mut token_in = [0u8; 20];
    token_in.copy_from_slice(&body[12..32]);
    let amount_in = read_u256(&body[32..64])?;
    let amount_out_min = read_u256(&body[64..96])?;
    let (pool, direction) = unpack_dex(&body[96..128])?;
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    hops.push(DecodedHop {
        pool: Some(pool),
        token_in,
        token_out: [0u8; 20],
        fee_pips: 0,
        direction,
    });
    Ok(RouteIntent {
        hops,
        amount_in,
        amount_out_min,
        exact_input: true,
    })
}

/// Decode `unoswapTo(uint256 to, uint256 token, uint256 amount, uint256 minReturn, uint256 dex)`
/// — selector `0xe2c95c82`. Same as `unoswap` but recipient-prefixed (the leading `to` word is
/// not needed for state-update), so this is still a **single-pool** swap. We skip the `to` word
/// and read `(token, amount, minReturn, dex)` from the shifted offsets.
pub fn decode_unoswap_to(calldata: &[u8]) -> Result<RouteIntent, DecodeError> {
    if calldata.len() < 4 + 32 * 5 {
        return Err(DecodeError::TooShort);
    }
    let body = &calldata[4..];
    // body[0..32] = to (recipient — skipped)
    let mut token_in = [0u8; 20];
    token_in.copy_from_slice(&body[44..64]); // low 20 bytes of body[32..64] = token
    let amount_in = read_u256(&body[64..96])?;
    let amount_out_min = read_u256(&body[96..128])?;
    let (pool, direction) = unpack_dex(&body[128..160])?;
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    hops.push(DecodedHop {
        pool: Some(pool),
        token_in,
        token_out: [0u8; 20],
        fee_pips: 0,
        direction,
    });
    Ok(RouteIntent {
        hops,
        amount_in,
        amount_out_min,
        exact_input: true,
    })
}

/// Decode `unoswap2(uint256 token, uint256 amount, uint256 minReturn, uint256 dex, uint256 dex2)`.
pub fn decode_unoswap2(calldata: &[u8]) -> Result<RouteIntent, DecodeError> {
    if calldata.len() < 4 + 32 * 5 {
        return Err(DecodeError::TooShort);
    }
    let body = &calldata[4..];
    let mut token_in = [0u8; 20];
    token_in.copy_from_slice(&body[12..32]);
    let amount_in = read_u256(&body[32..64])?;
    let amount_out_min = read_u256(&body[64..96])?;
    let (pool0, dir0) = unpack_dex(&body[96..128])?;
    let (pool1, dir1) = unpack_dex(&body[128..160])?;
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    hops.push(DecodedHop {
        pool: Some(pool0),
        token_in,
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir0,
    });
    hops.push(DecodedHop {
        pool: Some(pool1),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir1,
    });
    Ok(RouteIntent {
        hops,
        amount_in,
        amount_out_min,
        exact_input: true,
    })
}

/// Decode `unoswap3(uint256 token, uint256 amount, uint256 minReturn, uint256 dex, uint256 dex2, uint256 dex3)`.
pub fn decode_unoswap3(calldata: &[u8]) -> Result<RouteIntent, DecodeError> {
    if calldata.len() < 4 + 32 * 6 {
        return Err(DecodeError::TooShort);
    }
    let body = &calldata[4..];
    let mut token_in = [0u8; 20];
    token_in.copy_from_slice(&body[12..32]);
    let amount_in = read_u256(&body[32..64])?;
    let amount_out_min = read_u256(&body[64..96])?;
    let (pool0, dir0) = unpack_dex(&body[96..128])?;
    let (pool1, dir1) = unpack_dex(&body[128..160])?;
    let (pool2, dir2) = unpack_dex(&body[160..192])?;
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    hops.push(DecodedHop {
        pool: Some(pool0),
        token_in,
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir0,
    });
    hops.push(DecodedHop {
        pool: Some(pool1),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir1,
    });
    hops.push(DecodedHop {
        pool: Some(pool2),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir2,
    });
    Ok(RouteIntent {
        hops,
        amount_in,
        amount_out_min,
        exact_input: true,
    })
}

/// Decode `ethUnoswap(uint256 minReturn, uint256 dex)`. `amount_in` lives in
/// `msg.value` which isn't on the calldata bus — we set it to zero and let
/// `predict_route` short-circuit to "snapshot unchanged" (the pool gets dirtied
/// via the generation bump regardless, which is what we need for sim trigger).
pub fn decode_eth_unoswap(calldata: &[u8]) -> Result<RouteIntent, DecodeError> {
    if calldata.len() < 4 + 32 * 2 {
        return Err(DecodeError::TooShort);
    }
    let body = &calldata[4..];
    let amount_out_min = read_u256(&body[0..32])?;
    let (pool, direction) = unpack_dex(&body[32..64])?;
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    hops.push(DecodedHop {
        pool: Some(pool),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction,
    });
    Ok(RouteIntent {
        hops,
        amount_in: U256::ZERO,
        amount_out_min,
        exact_input: true,
    })
}

/// Decode `ethUnoswap2(uint256 minReturn, uint256 dex, uint256 dex2)`.
pub fn decode_eth_unoswap2(calldata: &[u8]) -> Result<RouteIntent, DecodeError> {
    if calldata.len() < 4 + 32 * 3 {
        return Err(DecodeError::TooShort);
    }
    let body = &calldata[4..];
    let amount_out_min = read_u256(&body[0..32])?;
    let (pool0, dir0) = unpack_dex(&body[32..64])?;
    let (pool1, dir1) = unpack_dex(&body[64..96])?;
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    hops.push(DecodedHop {
        pool: Some(pool0),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir0,
    });
    hops.push(DecodedHop {
        pool: Some(pool1),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir1,
    });
    Ok(RouteIntent {
        hops,
        amount_in: U256::ZERO,
        amount_out_min,
        exact_input: true,
    })
}

/// Decode `ethUnoswap3(uint256 minReturn, uint256 dex, uint256 dex2, uint256 dex3)`.
pub fn decode_eth_unoswap3(calldata: &[u8]) -> Result<RouteIntent, DecodeError> {
    if calldata.len() < 4 + 32 * 4 {
        return Err(DecodeError::TooShort);
    }
    let body = &calldata[4..];
    let amount_out_min = read_u256(&body[0..32])?;
    let (pool0, dir0) = unpack_dex(&body[32..64])?;
    let (pool1, dir1) = unpack_dex(&body[64..96])?;
    let (pool2, dir2) = unpack_dex(&body[96..128])?;
    let mut hops: ArrayVec<DecodedHop, MAX_DECODED_HOPS> = ArrayVec::new();
    hops.push(DecodedHop {
        pool: Some(pool0),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir0,
    });
    hops.push(DecodedHop {
        pool: Some(pool1),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir1,
    });
    hops.push(DecodedHop {
        pool: Some(pool2),
        token_in: [0u8; 20],
        token_out: [0u8; 20],
        fee_pips: 0,
        direction: dir2,
    });
    Ok(RouteIntent {
        hops,
        amount_in: U256::ZERO,
        amount_out_min,
        exact_input: true,
    })
}

/// Top-level: dispatch by selector.
///
/// Selectors are the `cast sig`-verified 1inch AggregationRouterV6 values. The earlier wiring had
/// three defects, all fixed here: (1) `ethUnoswap2`/`ethUnoswap3` used non-existent bytes
/// (`0x89af9261`/`0x969c8054`) instead of the real `0x89af926a`/`0x188ac35d`; (2) `0xe2c95c82` is
/// `unoswapTo` (single pool), not `unoswap2`; (3) the real `unoswap2`=`0x8770ba91` was routed to
/// `decode_unoswap3` and the real `unoswap3`=`0x19367472` wasn't dispatched at all.
pub fn decode_oneinch(selector: [u8; 4], calldata: &[u8]) -> Result<RouteIntent, DecodeError> {
    match selector {
        [0x83, 0x80, 0x0a, 0x8e] => decode_unoswap(calldata), // unoswap            → 1 pool
        [0xe2, 0xc9, 0x5c, 0x82] => decode_unoswap_to(calldata), // unoswapTo          → 1 pool
        [0x87, 0x70, 0xba, 0x91] => decode_unoswap2(calldata), // unoswap2           → 2 pools
        [0x19, 0x36, 0x74, 0x72] => decode_unoswap3(calldata), // unoswap3           → 3 pools
        [0xa7, 0x6d, 0xfc, 0x3b] => decode_eth_unoswap(calldata), // ethUnoswap         → 1 pool
        [0x89, 0xaf, 0x92, 0x6a] => decode_eth_unoswap2(calldata), // ethUnoswap2        → 2 pools
        [0x18, 0x8a, 0xc3, 0x5d] => decode_eth_unoswap3(calldata), // ethUnoswap3        → 3 pools
        _ => Err(DecodeError::UnknownSelector),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unoswap_extracts_pool_and_direction() {
        // selector || token(32) || amount(32) || minReturn(32) || dex(32)
        let mut calldata = vec![0x83, 0x80, 0x0a, 0x8e];
        // token = 0x000...00aaaa...aa (20 bytes of 0xaa right-aligned)
        let mut token_word = [0u8; 32];
        for b in &mut token_word[12..32] {
            *b = 0xaa;
        }
        calldata.extend_from_slice(&token_word);
        // amount = 100_000_000
        let mut amt = [0u8; 32];
        amt[28..32].copy_from_slice(&100_000_000u32.to_be_bytes());
        calldata.extend_from_slice(&amt);
        // minReturn = 0
        calldata.extend_from_slice(&[0u8; 32]);
        // dex: pool addr 0xbbb...bbb in low 160 bits, zeroForOne=1 (bit 247)
        let mut dex = [0u8; 32];
        for b in &mut dex[12..32] {
            *b = 0xbb;
        }
        dex[1] = 0x80; // bit 247 set
        calldata.extend_from_slice(&dex);

        let intent = decode_oneinch([0x83, 0x80, 0x0a, 0x8e], &calldata).unwrap();
        assert_eq!(intent.hops.len(), 1);
        assert_eq!(intent.hops[0].token_in, [0xaa; 20]);
        assert_eq!(intent.hops[0].pool, Some([0xbb; 20]));
        assert_eq!(intent.hops[0].direction, Direction::ZeroForOne);
        assert_eq!(intent.amount_in, U256::from(100_000_000u64));
    }

    #[test]
    fn eth_unoswap_zero_amount() {
        let mut calldata = vec![0xa7, 0x6d, 0xfc, 0x3b];
        calldata.extend_from_slice(&[0u8; 32]); // minReturn
        let mut dex = [0u8; 32];
        for b in &mut dex[12..32] {
            *b = 0xcc;
        }
        calldata.extend_from_slice(&dex); // direction = OneForZero (bit clear)
        let intent = decode_oneinch([0xa7, 0x6d, 0xfc, 0x3b], &calldata).unwrap();
        assert_eq!(intent.amount_in, U256::ZERO);
        assert_eq!(intent.hops[0].pool, Some([0xcc; 20]));
        assert_eq!(intent.hops[0].direction, Direction::OneForZero);
    }

    #[test]
    fn rejects_unknown_selector() {
        let calldata = vec![0xff, 0xff, 0xff, 0xff];
        assert!(decode_oneinch([0xff; 4], &calldata).is_err());
    }

    // ---- B1: each wired selector must equal its keccak-derived (cast sig) value. ----
    // The expected bytes below are the offline-keccak results of `cast sig "<sig>"` against the
    // live 1inch AggregationRouterV6 ABI. We assert the dispatch arm actually USES them by feeding
    // a minimal valid body for that arm and requiring it decode (Ok) rather than UnknownSelector.
    #[test]
    fn wired_selectors_match_keccak_derived_values() {
        // (selector bytes, signature) pairs — hard-coded from `cast sig`.
        const UNOSWAP: [u8; 4] = [0x83, 0x80, 0x0a, 0x8e]; // unoswap(uint256,uint256,uint256,uint256)
        const UNOSWAP_TO: [u8; 4] = [0xe2, 0xc9, 0x5c, 0x82]; // unoswapTo(uint256x5)
        const UNOSWAP2: [u8; 4] = [0x87, 0x70, 0xba, 0x91]; // unoswap2(uint256x5)
        const UNOSWAP3: [u8; 4] = [0x19, 0x36, 0x74, 0x72]; // unoswap3(uint256x6)
        const ETH_UNOSWAP: [u8; 4] = [0xa7, 0x6d, 0xfc, 0x3b]; // ethUnoswap(uint256,uint256)
        const ETH_UNOSWAP2: [u8; 4] = [0x89, 0xaf, 0x92, 0x6a]; // ethUnoswap2(uint256,uint256,uint256)
        const ETH_UNOSWAP3: [u8; 4] = [0x18, 0x8a, 0xc3, 0x5d]; // ethUnoswap3(uint256x4)

        // Sanity: the OLD wrong bytes must NOT be the ones we now dispatch.
        assert_ne!(
            ETH_UNOSWAP2,
            [0x89, 0xaf, 0x92, 0x61],
            "ethUnoswap2 must be 0x89af926a not 0x89af9261"
        );
        assert_ne!(
            ETH_UNOSWAP3,
            [0x96, 0x9c, 0x80, 0x54],
            "ethUnoswap3 must be 0x188ac35d not 0x969c8054"
        );

        // Build a body large enough for the widest arm (unoswap3: 6 static words) with a non-zero
        // token word (so the address-vs-zero check passes) and a valid dex word per hop.
        let mk = |n_words: usize, token_first: bool| -> Vec<u8> {
            let mut cd = vec![0u8; 4 + 32 * n_words];
            if token_first {
                for b in &mut cd[4 + 12..4 + 32] {
                    *b = 0xaa;
                } // token word low 20 bytes
            }
            cd
        };
        // Each of these MUST decode (Ok) — proving the dispatch arm exists for that exact selector.
        assert!(decode_oneinch(UNOSWAP, &mk(4, true)).is_ok());
        assert!(decode_oneinch(UNOSWAP_TO, &mk(5, false)).is_ok());
        assert!(decode_oneinch(UNOSWAP2, &mk(5, true)).is_ok());
        assert!(decode_oneinch(UNOSWAP3, &mk(6, true)).is_ok());
        assert!(decode_oneinch(ETH_UNOSWAP, &mk(2, false)).is_ok());
        assert!(decode_oneinch(ETH_UNOSWAP2, &mk(3, false)).is_ok());
        assert!(decode_oneinch(ETH_UNOSWAP3, &mk(4, false)).is_ok());

        // The retired wrong bytes must now be rejected (no stale arm left behind).
        assert!(decode_oneinch([0x89, 0xaf, 0x92, 0x61], &mk(3, false)).is_err());
        assert!(decode_oneinch([0x96, 0x9c, 0x80, 0x54], &mk(4, false)).is_err());
    }

    // ---- B2: crafted calldata must decode to the correct HOP COUNT (1 / 2 / 3). ----
    /// Build a `dex` word: pool addr `fill` in low 20 bytes, zeroForOne flag clear.
    fn dex_word(fill: u8) -> [u8; 32] {
        let mut w = [0u8; 32];
        for b in &mut w[12..32] {
            *b = fill;
        }
        w
    }
    /// 32-byte word with `fill` in the low 20 bytes (used for a `token` word).
    fn addr_word(fill: u8) -> [u8; 32] {
        let mut w = [0u8; 32];
        for b in &mut w[12..32] {
            *b = fill;
        }
        w
    }

    #[test]
    fn unoswap_to_decodes_single_hop() {
        // unoswapTo(to, token, amount, minReturn, dex) — recipient-prefixed single pool.
        let mut cd = vec![0xe2, 0xc9, 0x5c, 0x82];
        cd.extend_from_slice(&addr_word(0xde)); // to (recipient — skipped)
        cd.extend_from_slice(&addr_word(0xaa)); // token
        cd.extend_from_slice(&[0u8; 32]); // amount
        cd.extend_from_slice(&[0u8; 32]); // minReturn
        cd.extend_from_slice(&dex_word(0xbb)); // dex (pool 0xbb..)
        let intent = decode_oneinch([0xe2, 0xc9, 0x5c, 0x82], &cd).unwrap();
        assert_eq!(intent.hops.len(), 1, "unoswapTo is a SINGLE-pool swap");
        assert_eq!(intent.hops[0].token_in, [0xaa; 20]);
        assert_eq!(intent.hops[0].pool, Some([0xbb; 20]));
    }

    #[test]
    fn unoswap2_decodes_two_hops() {
        // unoswap2(token, amount, minReturn, dex, dex2)
        let mut cd = vec![0x87, 0x70, 0xba, 0x91];
        cd.extend_from_slice(&addr_word(0xaa)); // token
        cd.extend_from_slice(&[0u8; 32]); // amount
        cd.extend_from_slice(&[0u8; 32]); // minReturn
        cd.extend_from_slice(&dex_word(0xb1)); // dex
        cd.extend_from_slice(&dex_word(0xb2)); // dex2
        let intent = decode_oneinch([0x87, 0x70, 0xba, 0x91], &cd).unwrap();
        assert_eq!(intent.hops.len(), 2, "unoswap2 is a TWO-pool swap");
        assert_eq!(intent.hops[0].pool, Some([0xb1; 20]));
        assert_eq!(intent.hops[1].pool, Some([0xb2; 20]));
    }

    #[test]
    fn unoswap3_decodes_three_hops() {
        // unoswap3(token, amount, minReturn, dex, dex2, dex3)
        let mut cd = vec![0x19, 0x36, 0x74, 0x72];
        cd.extend_from_slice(&addr_word(0xaa)); // token
        cd.extend_from_slice(&[0u8; 32]); // amount
        cd.extend_from_slice(&[0u8; 32]); // minReturn
        cd.extend_from_slice(&dex_word(0xc1)); // dex
        cd.extend_from_slice(&dex_word(0xc2)); // dex2
        cd.extend_from_slice(&dex_word(0xc3)); // dex3
        let intent = decode_oneinch([0x19, 0x36, 0x74, 0x72], &cd).unwrap();
        assert_eq!(intent.hops.len(), 3, "unoswap3 is a THREE-pool swap");
        assert_eq!(intent.hops[0].pool, Some([0xc1; 20]));
        assert_eq!(intent.hops[1].pool, Some([0xc2; 20]));
        assert_eq!(intent.hops[2].pool, Some([0xc3; 20]));
    }
}
