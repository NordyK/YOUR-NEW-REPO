//! Canonical 4-byte function selectors (from §3.2 `reuse/decoding/swap_decoder.rs`).
//!
//! Used to classify a tx's calldata into a `TxClass`. The hot path uses a flat jump table
//! `(to_id, selector) -> action`; this module exposes the raw selector constants and a
//! `SelectorRegistry` for lookups during startup.

/// V3 SwapRouter selectors and SwapRouter02 selectors.
pub const KNOWN_V3_ROUTER_SELECTORS: &[[u8; 4]] = &[
    [0xc0, 0x4b, 0x8d, 0x59], // exactInput (V3 SwapRouter)
    [0x41, 0x4b, 0xf3, 0x89], // exactInputSingle
    [0xf2, 0x8c, 0x04, 0x98], // exactOutput
    [0xdb, 0x3e, 0x21, 0x98], // exactOutputSingle
    [0xb8, 0x58, 0x18, 0x3f], // SwapRouter02 exactInput
    [0x04, 0xe4, 0x5a, 0xaf], // SwapRouter02 exactInputSingle
    [0x09, 0xb8, 0x13, 0x46], // SwapRouter02 exactOutput
    [0x50, 0x23, 0xb4, 0xdf], // SwapRouter02 exactOutputSingle
    [0xac, 0x96, 0x50, 0xd8], // multicall
    [0x5a, 0xe4, 0x01, 0xdc], // multicall with deadline
];

/// V2 router selectors (Sushi/Uniswap V2 router 02, and forks like Camelot that
/// reuse the same calldata layout). Includes the fee-on-transfer exact-input
/// variants (identical layout up to `path`) and the exact-output variants.
pub const KNOWN_V2_ROUTER_SELECTORS: &[[u8; 4]] = &[
    [0x38, 0xed, 0x17, 0x39], // swapExactTokensForTokens
    [0x7f, 0xf3, 0x6a, 0xb5], // swapExactETHForTokens
    [0x18, 0xcb, 0xaf, 0xe5], // swapExactTokensForETH
    [0x5c, 0x11, 0xd7, 0x95], // swapExactTokensForTokensSupportingFeeOnTransferTokens
    [0xb6, 0xf9, 0xde, 0x95], // swapExactETHForTokensSupportingFeeOnTransferTokens
    [0x79, 0x1a, 0xc9, 0x47], // swapExactTokensForETHSupportingFeeOnTransferTokens
    [0x88, 0x03, 0xdb, 0xee], // swapTokensForExactTokens (exact-output)
    [0x4a, 0x25, 0xd9, 0x4a], // swapTokensForExactETH (exact-output)
];

/// Aggregator selectors (1inch, Paraswap, Odos, KyberSwap, Universal Router, etc.). Listed
/// for **state-update** classification only — the engine never builds aggregator-targeted
/// backrun candidates (§0.2).
// Selectors verified against openchain.xyz signature database (2026-05-28). Labels here are
// the EXACT 4byte→signature mapping, not heuristics — earlier comments were inconsistent
// (eg. 0xe2c95c82 was labeled `unoswap2` but actually resolves to `unoswapTo`).
pub const KNOWN_AGGREGATOR_SELECTORS: &[[u8; 4]] = &[
    [0x24, 0x85, 0x6b, 0xc3], // Uniswap UniversalRouter execute(bytes,bytes[])
    [0x35, 0x93, 0x56, 0x4c], // Uniswap UniversalRouter execute(bytes,bytes[],uint256)
    [0x07, 0xed, 0x23, 0x79], // 1inch v6 swap(address,(...7-static fields...),bytes) — DECODED
    [0x83, 0x80, 0x0a, 0x8e], // 1inch v6 unoswap(uint256,uint256,uint256,uint256) — DECODED
    [0xe2, 0xc9, 0x5c, 0x82], // 1inch v6 unoswapTo(uint256,uint256,uint256,uint256,uint256)
    [0x87, 0x70, 0xba, 0x91], // 1inch v6 unoswap2(uint256,uint256,uint256,uint256,uint256)
    [0x19, 0x36, 0x74, 0x72], // 1inch v6 unoswap3(uint256,uint256,uint256,uint256,uint256,uint256)
    // B3: the previous nine `address`-typed `*To`/3-arg `ethUnoswap` entries were FICTION —
    // their listed bytes were keccak of `address`-typed signatures that do NOT exist on real
    // v6 (v6 packs the recipient into a leading `uint256`). Replaced with the genuine
    // all-`uint256` selectors, each verified by `cast sig`:
    [0xf7, 0xf6, 0x3e, 0x5e], // 1inch v6 unoswap2To(uint256,uint256,uint256,uint256,uint256,uint256)
    [0x7e, 0x2c, 0x67, 0xf9], // 1inch v6 unoswap3To(uint256,uint256,uint256,uint256,uint256,uint256,uint256)
    [0xa7, 0x6d, 0xfc, 0x3b], // 1inch v6 ethUnoswap(uint256,uint256)
    [0x89, 0xaf, 0x92, 0x6a], // 1inch v6 ethUnoswap2(uint256,uint256,uint256)
    [0x18, 0x8a, 0xc3, 0x5d], // 1inch v6 ethUnoswap3(uint256,uint256,uint256,uint256)
    [0x17, 0x5a, 0xcc, 0xdc], // 1inch v6 ethUnoswapTo(uint256,uint256,uint256)
    [0xc7, 0x04, 0x10, 0xd6], // 1inch v6 ethUnoswap2To(uint256,uint256,uint256,uint256)
    [0xe0, 0x60, 0x07, 0x11], // 1inch v6 ethUnoswap3To(uint256,uint256,uint256,uint256,uint256)
    [0xe3, 0xea, 0xd5, 0x9e], // Paraswap v6 swapExactAmountIn(address,(7-static fields),uint256,bytes,bytes) — DECODED
    [0xf1, 0x7a, 0x45, 0x46], // Odos v3 swap(inputs[],outputs[],uint256,uint256,address,bytes)
    [0x41, 0x55, 0x65, 0xb0], // 0x ExchangeProxy transformERC20(address,address,uint256,uint256,(uint32,bytes)[])
    [0xa8, 0xc9, 0xed, 0x67], // KyberSwap Elastic swapExactInputSingle((tokenIn,tokenOut,fee,recipient,amount,minOut,deadline,sqrtPriceLimit)) — DECODED
    [0x52, 0xbb, 0xbe, 0x29], // Balancer V2 Vault swap(SingleSwap,FundManagement,uint256,uint256) — DECODED (concrete pool)
    [0x94, 0x5b, 0xce, 0xc9], // Balancer V2 Vault batchSwap(uint8,steps[],assets[],funds,limits[],uint256) — DECODED (first step)
];

use ahash::AHashMap;

/// Reverse lookup `selector -> selector class`. Used at startup to assemble the
/// classifier table.
pub struct SelectorRegistry {
    map: AHashMap<[u8; 4], SelectorClass>,
}

/// Selector class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorClass {
    /// Known V3 router method.
    V3Router,
    /// Known V2 router method.
    V2Router,
    /// Known aggregator method.
    Aggregator,
}

impl SelectorRegistry {
    /// Build a registry covering the constants in this module.
    pub fn canonical() -> Self {
        let mut map = AHashMap::default();
        for s in KNOWN_V3_ROUTER_SELECTORS {
            map.insert(*s, SelectorClass::V3Router);
        }
        for s in KNOWN_V2_ROUTER_SELECTORS {
            map.insert(*s, SelectorClass::V2Router);
        }
        for s in KNOWN_AGGREGATOR_SELECTORS {
            map.insert(*s, SelectorClass::Aggregator);
        }
        Self { map }
    }

    /// Lookup a selector.
    pub fn lookup(&self, selector: &[u8; 4]) -> Option<SelectorClass> {
        self.map.get(selector).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_v3_selector_lookup() {
        let r = SelectorRegistry::canonical();
        assert_eq!(
            r.lookup(&[0xc0, 0x4b, 0x8d, 0x59]),
            Some(SelectorClass::V3Router)
        );
    }

    #[test]
    fn known_aggregator_lookup() {
        let r = SelectorRegistry::canonical();
        assert_eq!(
            r.lookup(&[0x24, 0x85, 0x6b, 0xc3]),
            Some(SelectorClass::Aggregator)
        );
    }

    #[test]
    fn unknown_selector_returns_none() {
        let r = SelectorRegistry::canonical();
        assert_eq!(r.lookup(&[0x00, 0x00, 0x00, 0x00]), None);
    }

    /// `cast sig` equivalent: the 4-byte selector is `keccak256(signature)[..4]`.
    fn sig4(signature: &str) -> [u8; 4] {
        let h = alloy_primitives::keccak256(signature.as_bytes());
        [h[0], h[1], h[2], h[3]]
    }

    // B1/B3: every wired 1inch v6 selector must equal the offline-keccak of its (corrected)
    // signature string — i.e. exactly what `cast sig "<sig>"` returns. This pins the registry to
    // real v6 ABI signatures and would have caught both the wrong `ethUnoswap2`/`ethUnoswap3` bytes
    // and the nine fictional `address`-typed `*To` entries that v6 does not expose.
    #[test]
    fn oneinch_v6_registry_selectors_match_keccak_signatures() {
        let cases: &[(&str, [u8; 4])] = &[
            (
                "unoswap(uint256,uint256,uint256,uint256)",
                [0x83, 0x80, 0x0a, 0x8e],
            ),
            (
                "unoswapTo(uint256,uint256,uint256,uint256,uint256)",
                [0xe2, 0xc9, 0x5c, 0x82],
            ),
            (
                "unoswap2(uint256,uint256,uint256,uint256,uint256)",
                [0x87, 0x70, 0xba, 0x91],
            ),
            (
                "unoswap3(uint256,uint256,uint256,uint256,uint256,uint256)",
                [0x19, 0x36, 0x74, 0x72],
            ),
            (
                "unoswap2To(uint256,uint256,uint256,uint256,uint256,uint256)",
                [0xf7, 0xf6, 0x3e, 0x5e],
            ),
            (
                "unoswap3To(uint256,uint256,uint256,uint256,uint256,uint256,uint256)",
                [0x7e, 0x2c, 0x67, 0xf9],
            ),
            ("ethUnoswap(uint256,uint256)", [0xa7, 0x6d, 0xfc, 0x3b]),
            (
                "ethUnoswap2(uint256,uint256,uint256)",
                [0x89, 0xaf, 0x92, 0x6a],
            ),
            (
                "ethUnoswap3(uint256,uint256,uint256,uint256)",
                [0x18, 0x8a, 0xc3, 0x5d],
            ),
            (
                "ethUnoswapTo(uint256,uint256,uint256)",
                [0x17, 0x5a, 0xcc, 0xdc],
            ),
            (
                "ethUnoswap2To(uint256,uint256,uint256,uint256)",
                [0xc7, 0x04, 0x10, 0xd6],
            ),
            (
                "ethUnoswap3To(uint256,uint256,uint256,uint256,uint256)",
                [0xe0, 0x60, 0x07, 0x11],
            ),
        ];
        for (sig, expected) in cases {
            assert_eq!(sig4(sig), *expected, "selector mismatch for `{sig}`");
            // …and the selector must actually be present in the aggregator registry.
            assert!(
                KNOWN_AGGREGATOR_SELECTORS.contains(expected),
                "registry is missing the real v6 selector for `{sig}`"
            );
        }

        // The retired fiction must be GONE: none of the nine bogus `address`-typed bytes survive.
        for bogus in [
            [0xc2u8, 0xd5, 0xd6, 0xf4], // unoswapTo(address,…)
            [0xdb, 0x90, 0x27, 0x77],   // unoswap2To(address,…)
            [0x0a, 0x3e, 0xa2, 0x57],   // unoswap3To(address,…)
            [0x88, 0x1d, 0x55, 0x92],   // ethUnoswap(uint256,uint256,uint256) 3-arg
            [0xde, 0x80, 0xf5, 0x5b],   // ethUnoswap2(…wrong…)
            [0x3c, 0xe2, 0xe1, 0x93],   // ethUnoswap3(…wrong…)
            [0x5a, 0x71, 0x2d, 0xcd],   // ethUnoswapTo(address,…)
            [0x66, 0x44, 0x07, 0x2b],   // ethUnoswap2To(address,…)
            [0xbc, 0xcd, 0x4d, 0x16],   // ethUnoswap3To(address,…)
        ] {
            assert!(
                !KNOWN_AGGREGATOR_SELECTORS.contains(&bogus),
                "fictional selector {bogus:02x?} should have been removed"
            );
        }
    }

    #[test]
    fn balancer_vault_selectors_classify_as_aggregator() {
        // Both Balancer V2 Vault entrypoints must be classified so the hot path routes them to
        // `decode_aggregator_intents` (→ `decode_balancer_vault_swap` / `_batch_swap`).
        let r = SelectorRegistry::canonical();
        assert_eq!(
            r.lookup(&[0x52, 0xbb, 0xbe, 0x29]), // swap(SingleSwap,FundManagement,uint256,uint256)
            Some(SelectorClass::Aggregator)
        );
        assert_eq!(
            r.lookup(&[0x94, 0x5b, 0xce, 0xc9]), // batchSwap(uint8,steps[],assets[],funds,limits[],uint256)
            Some(SelectorClass::Aggregator)
        );
    }
}
