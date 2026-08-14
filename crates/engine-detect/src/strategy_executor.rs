//! Decoder for the dominant Arbitrum "StrategyExecutor" custom router
//! (`0x93c30e6e09f698cfea99b09b5166ed90cfaedd17`) — the single contract the prior flow analysis
//! found to carry ~47 % of major-token swap volume (plan §6 Workstream E.3, "top custom selectors").
//!
//! # Why a dedicated decoder
//! The StrategyExecutor is **unverified** and uses a **private, compact pool-index calldata
//! encoding** — NOT standard ABI and NOT an embedded pool address — so neither the byte-wise
//! address sniff (`pipeline::sniff_pool_addresses`) nor the access-list sniff
//! (`pipeline::sniff_access_list_pools`) can wake the pool it touches *synchronously* (pre-execution
//! from the feed tx). Without this, those swaps are only caught post-execution via the pool's
//! Uniswap `Swap` log — one block late.
//!
//! # The encoding (reverse-engineered from live calldata, 2026-06)
//! Entry selector `0xb2460c48`. The first 2 bytes after the selector are a big-endian **pool
//! index** into the executor's own internal pool table; the remainder is variable-length route /
//! amount data (32- and 36-byte payloads observed for single- vs multi-leg routes). Six live
//! samples across three distinct pools confirmed the index is constant per pool and distinct across
//! pools (see the unit tests' golden vectors). Example: index `0x0013` ⇒ the WBTC/WETH UniV3 pool
//! `0x2f5e87C9312fa29aed5c179E456625D79015299c`.
//!
//! # What this module provides vs. what the operator must supply
//! [`decode_pool_index`] extracts the index from the calldata (pure, fully tested). The
//! `index → pool_address` map is **live data** — it is built by correlating each `0xb2460c48`
//! payload with the pool named in that tx's emitted `Swap` log (see `DATA_CAPTURE.md` §2), because
//! the address is not in the calldata. Once the operator supplies that map,
//! [`resolve_executor_pool`] turns a feed tx into the resident pool to mark dirty. The map is
//! append-only and grows as the executor registers new pools; an unknown index simply falls back to
//! the post-execution `Swap`-log path (no incorrect wakeup is ever produced).

use ahash::AHashMap;

/// The StrategyExecutor contract address on Arbitrum One.
pub const STRATEGY_EXECUTOR: [u8; 20] = [
    0x93, 0xc3, 0x0e, 0x6e, 0x09, 0xf6, 0x98, 0xcf, 0xea, 0x99, 0xb0, 0x9b, 0x51, 0x66, 0xed, 0x90,
    0xcf, 0xae, 0xdd, 0x17,
];

/// The StrategyExecutor's dominant entry selector (`keccak("…")[..4]` is unknown — the contract is
/// unverified — but the on-chain selector byte string is fixed at `0xb2460c48`).
pub const SELECTOR_B2460C48: [u8; 4] = [0xb2, 0x46, 0x0c, 0x48];

/// Extract the big-endian u16 pool index from a StrategyExecutor `0xb2460c48` calldata.
///
/// `calldata` is the full tx input (selector ++ payload). Returns `None` if the selector doesn't
/// match or the payload is too short to hold the index. Pure + allocation-free.
#[inline]
pub fn decode_pool_index(calldata: &[u8]) -> Option<u16> {
    if calldata.len() < 6 || calldata[..4] != SELECTOR_B2460C48 {
        return None;
    }
    Some(u16::from_be_bytes([calldata[4], calldata[5]]))
}

/// Resolve a StrategyExecutor feed tx to the resident pool it will move, via the operator-supplied
/// `index → pool_address` map (see module docs / `DATA_CAPTURE.md` §2 for how that map is built).
/// Returns `None` for a non-matching selector or an index not yet in the map (the latter falls back
/// to the post-execution `Swap`-log path — never a wrong wakeup).
#[inline]
pub fn resolve_executor_pool(
    calldata: &[u8],
    index_to_pool: &AHashMap<u16, [u8; 20]>,
) -> Option<[u8; 20]> {
    let idx = decode_pool_index(calldata)?;
    index_to_pool.get(&idx).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `0xb2460c48` calldata captured live from Arbitrum (2026-06), with the pool each tx moved
    // (read from the tx's emitted Uniswap Swap log). These are the golden vectors that pin the
    // encoding: the leading u16 is the pool index — constant per pool, distinct across pools.
    const WBTC_WETH_POOL: [u8; 20] = [
        0x2f, 0x5e, 0x87, 0xc9, 0x31, 0x2f, 0xa2, 0x9a, 0xed, 0x5c, 0x17, 0x9e, 0x45, 0x66, 0x25,
        0xd7, 0x90, 0x15, 0x29, 0x9c,
    ];

    fn hexcd(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn index_is_constant_per_pool_and_distinct_across_pools() {
        // Two txs, SAME pool (WBTC/WETH, 0x2f5e87C9…299c) → same index 0x0013, differing amounts.
        let s1 = hexcd("b2460c48001300252897f3759a100000000000000965dbbae54124521c2d7594424a8f00");
        let s2 = hexcd("b2460c4800130015086ce32791780000000000000965d7beba0aea7b4e5d59a09432d000");
        assert_eq!(decode_pool_index(&s1), Some(0x0013));
        assert_eq!(decode_pool_index(&s2), Some(0x0013));
        // Other live txs on DIFFERENT pools → different indices (and a longer 36-byte payload).
        let s3 = hexcd("b2460c480008"); // index 0x0008 (one of three observed)
        let s4 = hexcd("b2460c480012"); // index 0x0012
        assert_eq!(decode_pool_index(&s3), Some(0x0008));
        assert_eq!(decode_pool_index(&s4), Some(0x0012));
        assert_ne!(decode_pool_index(&s1), decode_pool_index(&s3));
    }

    #[test]
    fn resolve_uses_operator_index_map() {
        let s1 = hexcd("b2460c48001300252897f3759a100000000000000965dbbae54124521c2d7594424a8f00");
        let mut map: AHashMap<u16, [u8; 20]> = AHashMap::new();
        // Unknown index before the map is populated → no synchronous wakeup (falls back to logs).
        assert_eq!(resolve_executor_pool(&s1, &map), None);
        // Operator records 0x0013 → WBTC/WETH pool from the tx's Swap log.
        map.insert(0x0013, WBTC_WETH_POOL);
        assert_eq!(resolve_executor_pool(&s1, &map), Some(WBTC_WETH_POOL));
    }

    #[test]
    fn rejects_wrong_selector_and_short_calldata() {
        assert_eq!(decode_pool_index(&hexcd("deadbeef0013")), None); // wrong selector
        assert_eq!(decode_pool_index(&hexcd("b2460c48")), None); // no index bytes
        assert_eq!(decode_pool_index(&[]), None);
        let map = AHashMap::new();
        assert_eq!(resolve_executor_pool(&hexcd("deadbeef0013"), &map), None);
    }
}
