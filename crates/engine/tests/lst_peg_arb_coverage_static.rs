//! LST PEG-ARB COVERAGE — pins that the liquid-staking-token niche stays fully wired so a config
//! edit can't silently drop the "one clean, high-margin, lightly-contested arb inside our coverage"
//! (wstETH/WETH and the weETH/rETH/ezETH peg pairs).
//!
//! Grounded in Dune (`dex.trades`, Arbitrum, trailing 90d, queried 2026-06-20). LST↔WETH peg-arb
//! liquidity and the engine family each venue maps to:
//!   * Uniswap V3   wstETH ~$75.6M · rETH ~$5.3M · weETH ~$3.0M · ezETH ~$1.8M   BYTE-EXACT (UniswapV3)   → fast-lane
//!   * Camelot V3   weETH  ~$21.5M · wstETH ~$6.8M · ezETH ~$3.5M                BYTE-EXACT (AlgebraV19)  → fast-lane
//!   * Sushi V2     wstETH ~$0.45M                                              BYTE-EXACT (SushiV2)     → fast-lane
//!   * Fluid v1     wstETH ~$62.2M · weETH ~$50.3M                              Tier-2 (FluidDex)        → REVM-priced
//!   * Balancer V3  wstETH ~$5.4M · rETH ~$5.4M                                 NOT COVERED (we wire balancer_v2 = a different Vault)
//!   * Integral SIZE wstETH ~$8.2M                                             OUT OF SCOPE (RFQ/TWAP, not a constant-function AMM)
//! sfrxETH: ~0 Arbitrum WETH liquidity in 90d — listed but effectively dead (harmless dead weight).
//!
//! Why "listed + carrying venue configured" == tradable: every `factory_*.rs` registers a pool
//! ONLY if `token_allowlist.contains(token0) && token_allowlist.contains(token1)`, so any pool whose
//! both tokens are in the 20-token universe is auto-discovered. Thus the LSTs being in the universe
//! AND the carrying venues being configured is sufficient for the peg pairs to be tradable.
//!
//! Fast-lane consequence (answers "can other families be promoted when the AWS harness proves them"):
//! the peg arb's two deepest venues — Uniswap V3 and Camelot V3 — are BOTH byte-exact, so a
//! cross-venue wstETH/weETH/ezETH peg cycle is fully fast-lane-eligible the moment the AWS shadow
//! monitor shows `disagree==0` (`family_promotion_ready`). The Fluid v1 leg (largest single venue)
//! is Tier-2 and can NEVER be promoted (not byte-exact) — it rides the REVM-backed path.

use std::fs;

fn cfg() -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../engine.example.json");
    let s = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&s).expect("parse engine.example.json")
}

/// (symbol, canonical Arbitrum address lowercased). These are the addresses that returned real
/// LST↔WETH volume on Dune, so pinning them guards against a typo'd/placeholder address too.
const LSTS: &[(&str, &str)] = &[
    ("WETH", "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"),
    ("wstETH", "0x5979d7b546e38e414f7e9822514be443a4800529"),
    ("weETH", "0x35751007a407ca6feffe80b3cb397736d2cf4dbe"),
    ("rETH", "0xec70dcb4a1efa46b8f2d97c310c9c4790ba5ffa8"),
    ("ezETH", "0x2416092f143378750bb29b79ed961ab195cceea5"),
    ("sfrxETH", "0x95ab45875cffdba1e5f451b950bc2e42c0053f39"),
];

#[test]
fn lst_tokens_present_with_canonical_addresses() {
    let d = cfg();
    let tokens = d["tokens"].as_array().expect("tokens array");
    for (sym, addr) in LSTS {
        let entry = tokens
            .iter()
            .find(|t| t["symbol"].as_str() == Some(sym))
            .unwrap_or_else(|| panic!("LST {sym} missing from the token universe"));
        assert_eq!(
            entry["address"].as_str().unwrap().to_ascii_lowercase(),
            *addr,
            "{sym} address must be the canonical Arbitrum LST contract"
        );
        assert_eq!(entry["decimals"].as_u64(), Some(18), "{sym} decimals");
        assert_eq!(
            entry["risk_class"].as_str(),
            Some("permissionless"),
            "{sym} must be tradable (permissionless), not Excluded"
        );
    }
}

#[test]
fn lst_carrying_venues_are_configured() {
    let d = cfg();
    let families: Vec<&str> = d["venues"]
        .as_array()
        .expect("venues")
        .iter()
        .filter_map(|v| v["family"].as_str())
        .collect();
    // Byte-exact venues that carry the LST peg pairs (fast-lane-promotable).
    for fam in ["uniswap_v3", "camelot_v3", "sushi_v2", "uniswap_v4"] {
        assert!(
            families.contains(&fam),
            "byte-exact LST venue {fam} must be configured"
        );
    }
    // Tier-2 venue that also carries LST volume (REVM-priced, never promoted).
    assert!(
        families.contains(&"balancer_v2"),
        "balancer_v2 must be configured"
    );
    // Fluid v1 — the single largest LST venue (Tier-2) — wired via its resolver.
    let fluid = d["discovery"]["fluid_v1_resolver"].as_str().unwrap_or("");
    assert!(
        !fluid.is_empty(),
        "fluid_v1_resolver must be set (largest LST venue, Tier-2)"
    );
}

#[test]
fn lst_peg_arb_has_two_byte_exact_venues_for_fastlane() {
    // The clean fast-lane peg cycle (wstETH/weETH/ezETH) needs >=2 byte-exact venues so BOTH legs
    // can ride the zero-RPC lane once the AWS shadow monitor proves `disagree==0`. Uniswap V3 +
    // Camelot V3 (AlgebraV19) are both byte-exact and both carry these pairs (Dune-confirmed).
    let d = cfg();
    let families: Vec<&str> = d["venues"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["family"].as_str())
        .collect();
    let byte_exact_lst_venues = ["uniswap_v3", "camelot_v3"]
        .iter()
        .filter(|f| families.contains(f))
        .count();
    assert!(
        byte_exact_lst_venues >= 2,
        "need >=2 byte-exact venues carrying the LST peg pairs for a fast-lane cross-venue cycle"
    );
}
