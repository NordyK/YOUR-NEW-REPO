//! ON-CHAIN QUOTE-EQUIVALENCE PROOF — the engine's Rust integer AMM kernel quote == the REAL
//! on-chain quote, for a REAL Arbitrum pool, at a REAL pinned block (465686728), for the families
//! that the existing `MantisC2Predictor.Fork.t.sol` does NOT cover.
//!
//! This is a real-state diagnostic, not by itself a family-wide promotion proof. Unlike the forge
//! T2 test (which proves the C2 *predictor* by executing real swaps), it compares the *quote kernel*
//! directly with each sampled pool's on-chain pricing view (`get_dy` / `getAmountOut` / family
//! Quoter) at the SAME state. Promotion additionally requires the strict multi-pool/multi-direction
//! coverage report consumed by `fastlane_policy`.
//!
//! For each GAP family it:
//!   1. snapshots the pool via the engine's OWN `engine-rpc` reader → kernel state,
//!   2. computes the kernel quote for several input sizes,
//!   3. `eth_call`s the on-chain GROUND TRUTH (the pool's own pricing fn / family Quoter) for the
//!      same input at the same block,
//!   4. asserts kernel == on-chain for exact-target spot checks, or an explicitly documented bound
//!      for Tier-2 diagnostics. A tolerance result never authorizes the fast lane.
//!   5. prints `(input, kernel_out, onchain_out, delta_abs, delta_bps)` for every size.
//!
//! NO RPC IN THIS PROCESS BY DEFAULT — every test is `#[ignore]` (RPC-gated). The LEADER runs them
//! with `ARBITRUM_RPC_URL` set, against the warm forge cache pinned at block 465686728:
//!
//!   ARBITRUM_RPC_URL=<alchemy> \
//!     cargo test -p engine --test onchain_quote_parity -- --ignored --nocapture
//!
//! Override the block with PARITY_BLOCK=<n> (defaults to 465686728, the warm-cache head). Every
//! state comes from a real on-fork snapshot at that block — there are NO synthetic reserves.
//!
//! GAP families covered here (and why they were gaps):
//!   * Curve StableSwap-NG   — RE-VERIFY: forge T2 covers it, but only the static-fee path on a
//!                             balanced pool; this re-confirms `get_dy` byte/bound vs on-chain.
//!   * Curve CryptoSwap (Tricrypto-NG) — NO existing on-chain test (top priority). `get_amount_out_exact`.
//!   * Curve TwoCrypto-NG    — NO existing on-chain test. `get_amount_out_exact`.
//!   * SolidlyV3 (classic Solidly STABLE, x³y+xy³) — `calculate_stable_swap_output_normalized` vs the
//!                             pool's own `getAmountOut(amountIn, tokenIn)`.
//!   * KyberSwapElastic      — `kyber_elastic::get_amount_out` vs KyberSwap Elastic QuoterV2 (Tier-2:
//!                             fee-reinvestment-per-step not modelled → documented bound).
//!   * UniswapV4             — V3 kernel on V4 state vs the on-chain V4 Quoter (hookless pool).
//!   * Fluid v1              — kernel snapshot vs the deployed DexReservesResolver.

use alloy_primitives::{keccak256, U256};
use engine_amm::{TickInfo, V3Slot0};
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use std::env;

// ───────────────────────────── shared helpers ─────────────────────────────

/// Warm forge-cache head block. Every snapshot + ground-truth `eth_call` is pinned here so the
/// kernel reads EXACTLY the state the on-chain view prices against (no inter-call drift).
const DEFAULT_BLOCK: u64 = 465_686_728;

/// Resolve the pinned block: `PARITY_BLOCK` env override, else the warm-cache head.
fn pinned_block() -> u64 {
    env::var("PARITY_BLOCK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_BLOCK)
}

/// Build the archive client for an explicitly requested ignored parity test. Missing RPC is a hard
/// failure: `cargo test -- --ignored` must never report a vacuous green run with zero comparisons.
fn parity_client() -> (ArbitrumRpcClient, BlockTag, u64) {
    let url = env::var("ARBITRUM_RPC_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .expect("set ARBITRUM_RPC_URL=<archive Arbitrum endpoint>; parity may not SKIP");
    let bn = pinned_block();
    (ArbitrumRpcClient::new(url), BlockTag::Number(bn), bn)
}

/// 20-byte address from a 40-char hex string (no 0x).
fn addr(s: &str) -> [u8; 20] {
    let s = s.trim().trim_start_matches("0x");
    assert_eq!(s.len(), 40, "address must be 20 bytes hex: {s}");
    let mut a = [0u8; 20];
    for (k, b) in a.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * k..2 * k + 2], 16).expect("hex addr");
    }
    a
}

fn sel(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

fn u256_word(x: U256) -> [u8; 32] {
    x.to_be_bytes::<32>()
}

/// Decode word[0..32] of a return blob as U256.
fn decode_u256(resp: &[u8]) -> U256 {
    assert!(
        resp.len() >= 32,
        "return blob too short ({} bytes)",
        resp.len()
    );
    let mut b = [0u8; 32];
    b.copy_from_slice(&resp[0..32]);
    U256::from_be_bytes(b)
}

/// Decode word `n` (0-indexed) of a return blob as U256.
fn decode_word_n(resp: &[u8], n: usize) -> U256 {
    let off = n * 32;
    assert!(resp.len() >= off + 32, "return blob too short for word {n}");
    let mut b = [0u8; 32];
    b.copy_from_slice(&resp[off..off + 32]);
    U256::from_be_bytes(b)
}

fn bps(kernel: U256, onchain: U256) -> f64 {
    if onchain.is_zero() {
        return f64::INFINITY;
    }
    let k = u256_to_f64(kernel);
    let c = u256_to_f64(onchain);
    ((k - c).abs() / c) * 1e4
}

fn u256_to_f64(x: U256) -> f64 {
    // Good enough for delta_bps reporting on realistic quote magnitudes.
    let s = x.to_string();
    s.parse::<f64>().unwrap_or(f64::MAX)
}

/// Tier-1 (byte-exact) assertion: kernel MUST equal on-chain to the wei. Kept as the strict-mode
/// helper a leader can switch a byte-exact family to (V4/Solidly) once a pool is confirmed to
/// reproduce to 0 wei; the live tests route through [`assert_bounded`] with a ≤few-wei epsilon.
#[allow(dead_code)]
fn assert_byte_exact(family: &str, input: U256, kernel: U256, onchain: U256) {
    println!(
        "  {family:<20} in={input:<28} kernel={kernel:<28} onchain={onchain:<28} Δ=0 (byte-exact target)"
    );
    assert_eq!(
        kernel, onchain,
        "{family}: kernel quote != on-chain (NOT byte-exact) for input {input}: kernel={kernel} onchain={onchain}"
    );
}

/// Tier-2 (bounded) assertion: kernel must be within `[onchain - bound, onchain + eps]`. The hard
/// requirement is the kernel does NOT OVER-quote beyond `eps` (over-quoting = phantom profit = the
/// only capital-unsafe direction for a revert-on-shortfall executor). Under-quoting up to `bound`
/// (the un-modelled dynamic-fee / fee-reinvestment slice) is safe and expected.
fn assert_bounded(family: &str, input: U256, kernel: U256, onchain: U256, bound: U256, eps: U256) {
    let delta = if kernel >= onchain {
        kernel - onchain
    } else {
        onchain - kernel
    };
    println!(
        "  {family:<20} in={input:<24} kernel={kernel:<24} onchain={onchain:<24} Δ={delta:<18} ({:.3} bps)  bound={bound} eps={eps}",
        bps(kernel, onchain)
    );
    // Over-quote guard: kernel must not exceed on-chain by more than the rounding epsilon.
    assert!(
        kernel <= onchain + eps,
        "{family}: kernel OVER-quotes (capital-unsafe) for input {input}: kernel={kernel} > onchain={onchain} + eps={eps}"
    );
    // Under-quote guard: the kernel must stay within the documented fee/rounding bound.
    assert!(
        kernel + bound >= onchain,
        "{family}: kernel under-quotes BEYOND the documented bound for input {input}: onchain={onchain} - kernel={kernel} = {delta} > bound={bound}"
    );
}

// ───────────────── canonical Arbitrum token addresses (for sizing/labels) ─────────────────
const WETH: &str = "82af49447d8a07e3bd95bd0d56f35241523fbab1";
#[allow(dead_code)]
const USDC: &str = "af88d065e77c8cc2239327c5edb3a432268e5831";
#[allow(dead_code)]
const USDCE: &str = "ff970a61a04b1ca14834a43f5de4533ebddb5cc8";

// ════════════════════════════════════════════════════════════════════════════════════════════
// 1) Curve StableSwap-NG — RE-VERIFY `get_dy` vs on-chain `get_dy(int128,int128,uint256)`.
//    Pool: 0xdb8e662d646a9c838580e1eaee6e8fc89e55e10d (WETH/USDC NG, from real_pools.json).
//    Tier: byte-exact when balanced + static fee; the NG offpeg dynamic fee (modelled by the
//    kernel via offpeg_fee_multiplier) keeps it byte-exact even off-peg → assert byte-exact, and
//    fall back to a 1-wei rounding epsilon only (Curve rounds dy down by 1).
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn curve_stableswap_ng_quote_parity() {
    let (client, block, bn) = parity_client();
    // From real_pools.json CurveStableSwapNg[0] — but enumerate coins so we pick a 2-coin pool we
    // can drive 0->1 cleanly. This one is WETH/USDC (both 18/6 dp).
    let pool = addr("db8e662d646a9c838580e1eaee6e8fc89e55e10d");
    println!(
        "\n== Curve StableSwap-NG get_dy parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    let view = engine_rpc::fetch_curve_state(&client, &pool, 4, block).expect(
        "fetch_curve_state (NG plain pool) — check pool is a 2..4-coin NG pool at this block",
    );
    let n = view.kernel.n_coins;
    assert!(n >= 2, "need >=2 coins");
    println!(
        "   n_coins={n} A={} fee={} offpeg_mul={} balances={:?}",
        view.kernel.a_param,
        view.kernel.fee,
        view.kernel.offpeg_fee_multiplier,
        &view.kernel.balances[..n]
    );

    // Drive coin 0 -> coin 1. Size the inputs off coin 0's balance so we exercise real curvature.
    let bal0 = view.kernel.balances[0];
    let sizes: [u128; 4] = [bal0 / 5000, bal0 / 1000, bal0 / 200, bal0 / 50];
    let get_dy_sel = sel("get_dy(int128,int128,uint256)");

    let mut checked = 0u32;
    for dx in sizes {
        if dx == 0 {
            continue;
        }
        let kernel = engine_amm::curve::get_dy(&view.kernel, 0, 1, dx).expect("kernel get_dy");
        // on-chain get_dy(int128 i, int128 j, uint256 dx): i=0,j=1 → three 32-byte words.
        let mut cd = get_dy_sel.to_vec();
        cd.extend_from_slice(&u256_word(U256::ZERO)); // i = 0
        cd.extend_from_slice(&u256_word(U256::from(1u64))); // j = 1
        cd.extend_from_slice(&u256_word(U256::from(dx)));
        let onchain = decode_u256(&client.eth_call(&pool, &cd, block).expect("on-chain get_dy"));
        // Curve `get_dy` rounds dy down by 1 wei in xp-space; the kernel models the NG dynamic fee
        // exactly, so the only residual is sub-rate-scale rounding. Allow a tight 2-wei epsilon and
        // a small dynamic-fee bound (full base fee on dy) as the documented fallback.
        let kernel_u = U256::from(kernel);
        let bound = (onchain * U256::from(view.kernel.fee)) / U256::from(10_000_000_000u64)
            + U256::from(2u64);
        assert_bounded(
            "CurveStableSwapNG",
            U256::from(dx),
            kernel_u,
            onchain,
            bound,
            U256::from(2u64),
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "every StableSwap-NG input size must compare");
    println!(
        "   Curve StableSwap-NG: kernel get_dy matches on-chain within dynamic-fee/rounding bound."
    );
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 2) Curve CryptoSwap / Tricrypto-NG — `CryptoSwapState::get_amount_out_exact` vs on-chain
//    `get_dy(uint256,uint256,uint256)` (uint256 indices — the NG/CryptoSwap convention).
//    Pool: 0x82670f35306253222F8a165869B28c64739ac62e (Tricrypto-NG crvUSD/WBTC/WETH).
//    (Fallback: 0x960ea3e3c7fb317332d990873d354e18d7645590 legacy USDT/WBTC/WETH, same get_dy ABI.)
//    Tier-2: the kernel ports Curve's exact integer newton_D/_newton_y + dynamic fee, so it targets
//    byte-exactness; allow a small bound for the held-price_scale / k0 warm-start residual.
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn curve_cryptoswap_tricrypto_ng_quote_parity() {
    let (client, block, bn) = parity_client();
    // Tricrypto-NG crvUSD/WBTC/WETH (3-coin). If this pool ever fails to snapshot at the pinned
    // block, swap in the legacy USDT/WBTC/WETH tricrypto (same uint256 get_dy ABI).
    let pool = addr("82670f35306253222f8a165869b28c64739ac62e");
    println!(
        "\n== Curve CryptoSwap/Tricrypto-NG parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    let (mut state, coins) = engine_rpc::fetch_cryptoswap_state(&client, &pool, block)
        .expect("fetch_cryptoswap_state (3-coin Tricrypto-NG) at pinned block");
    // The kernel needs D solved from current xp before quoting (the snapshot may read a stale D()).
    state
        .recompute_d()
        .expect("recompute_d converges for live Tricrypto state");
    println!(
        "   coins=[0x{} 0x{} 0x{}] D={} A={} gamma={}",
        hexa(&coins[0]),
        hexa(&coins[1]),
        hexa(&coins[2]),
        state.d,
        state.ann,
        state.gamma
    );

    // Quote coin2 (WETH) -> coin0 (crvUSD/USDT): a WETH sell, the natural arb leg. Sizes in WETH.
    let i = 2usize;
    let j = 0usize;
    let sizes: [u128; 4] = [
        10_000_000_000_000_000,    // 0.01 WETH
        100_000_000_000_000_000,   // 0.1 WETH
        500_000_000_000_000_000,   // 0.5 WETH
        2_000_000_000_000_000_000, // 2 WETH
    ];
    let get_dy_sel = sel("get_dy(uint256,uint256,uint256)");

    let mut checked = 0u32;
    for dx in sizes {
        let kernel = match state.get_amount_out_exact(i, j, U256::from(dx)) {
            Ok((dy, _fee, _k0)) => dy,
            Err(e) => {
                println!("   kernel get_amount_out_exact dx={dx} -> Err({e:?}) (skipping size)");
                continue;
            }
        };
        let mut cd = get_dy_sel.to_vec();
        cd.extend_from_slice(&u256_word(U256::from(i as u64)));
        cd.extend_from_slice(&u256_word(U256::from(j as u64)));
        cd.extend_from_slice(&u256_word(U256::from(dx)));
        let onchain = decode_u256(
            &client
                .eth_call(&pool, &cd, block)
                .expect("on-chain tricrypto get_dy"),
        );
        // Bound: held price_scale (no per-swap repeg in the kernel) + 1-wei rounding. Use the
        // out_fee as a generous upper bound on the residual (it can only be a few bps).
        let bound = (onchain * state.out_fee) / U256::from(10_000_000_000u64) + U256::from(2u64);
        assert_bounded(
            "CryptoSwapTricrypto",
            U256::from(dx),
            kernel,
            onchain,
            bound,
            U256::from(2u64),
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "every Tricrypto-NG input size must compare");
    println!("   Tricrypto-NG: kernel get_amount_out_exact matches on-chain get_dy within bound.");
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 3) Curve TwoCrypto-NG — `TwoCryptoSwapState::get_amount_out_exact` vs on-chain
//    `get_dy(uint256,uint256,uint256)`.
//    Pool: 0x98961b846d1a046701af1a56023e29ff55522ad5 (TwoCrypto-NG RSR/WETH).
//    Tier-2: same exact-integer port; small bound for held price_scale / warm-start.
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn curve_twocrypto_ng_quote_parity() {
    let (client, block, bn) = parity_client();
    let pool = addr("98961b846d1a046701af1a56023e29ff55522ad5");
    println!(
        "\n== Curve TwoCrypto-NG parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    let (mut state, coins) = engine_rpc::fetch_twocrypto_state(&client, &pool, block)
        .expect("fetch_twocrypto_state (2-coin TwoCrypto-NG) at pinned block");
    state
        .recompute_d()
        .expect("recompute_d converges for live TwoCrypto state");
    println!(
        "   coins=[0x{} 0x{}] D={} A={} gamma={} price_scale={}",
        hexa(&coins[0]),
        hexa(&coins[1]),
        state.d,
        state.ann,
        state.gamma,
        state.price_scale
    );

    // coins[1] is WETH (18dp). Quote WETH -> coin0 (RSR). Sizes in WETH.
    let i = 1usize;
    let j = 0usize;
    let sizes: [u128; 4] = [
        10_000_000_000_000_000,
        50_000_000_000_000_000,
        200_000_000_000_000_000,
        1_000_000_000_000_000_000,
    ];
    let get_dy_sel = sel("get_dy(uint256,uint256,uint256)");

    let mut checked = 0u32;
    for dx in sizes {
        let kernel = match state.get_amount_out_exact(i, j, U256::from(dx)) {
            Ok((dy, _fee, _k0)) => dy,
            Err(e) => {
                println!("   kernel get_amount_out_exact dx={dx} -> Err({e:?}) (skipping size)");
                continue;
            }
        };
        let mut cd = get_dy_sel.to_vec();
        cd.extend_from_slice(&u256_word(U256::from(i as u64)));
        cd.extend_from_slice(&u256_word(U256::from(j as u64)));
        cd.extend_from_slice(&u256_word(U256::from(dx)));
        let onchain = decode_u256(
            &client
                .eth_call(&pool, &cd, block)
                .expect("on-chain twocrypto get_dy"),
        );
        let bound = (onchain * state.out_fee) / U256::from(10_000_000_000u64) + U256::from(2u64);
        assert_bounded(
            "TwoCryptoNG",
            U256::from(dx),
            kernel,
            onchain,
            bound,
            U256::from(2u64),
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "every TwoCrypto-NG input size must compare");
    println!("   TwoCrypto-NG: kernel get_amount_out_exact matches on-chain get_dy within bound.");
}

// 3b) Curve TwoCrypto-NG DYNAMIC FEE — `TwoCryptoSwapState::dynamic_fee` vs the pool's own
//     `fee_calc(uint256[2])` view (the exact on-chain oracle for the dynamic fee at any xp).
//     This proves the reduction_coefficient form BYTE-EXACT (tighter than the get_dy bound), and
//     locks the deployed v2.1.0 fee regulation. (An audit hypothesis that v2.1.0 changed the fee
//     form was DISPROVEN here: on-chain fee_calc matches the engine's form exactly.)
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn curve_twocrypto_ng_fee_calc_parity() {
    let (client, block, bn) = parity_client();
    let pool = addr("98961b846d1a046701af1a56023e29ff55522ad5");
    println!(
        "\n== Curve TwoCrypto-NG fee_calc parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    let (state, _coins) = engine_rpc::fetch_twocrypto_state(&client, &pool, block)
        .expect("fetch_twocrypto_state at pinned block");
    println!(
        "   mid_fee={} out_fee={} fee_gamma={}",
        state.mid_fee, state.out_fee, state.fee_gamma
    );

    let fee_calc_sel = sel("fee_calc(uint256[2])");
    let e18 = U256::from(1_000_000_000_000_000_000u64);
    // Balanced + a spread of imbalanced xp. fee_calc is scale-invariant in xp.
    let xps: [[U256; 2]; 6] = [
        [e18, e18],
        [e18 * U256::from(2u64), e18],
        [e18 * U256::from(5u64), e18],
        [e18, e18 * U256::from(9u64)],
        [e18 * U256::from(3u64), e18 * U256::from(7u64)],
        state.xp().expect("xp from live state"),
    ];
    for xp in xps {
        let kernel = state.dynamic_fee(&xp);
        let mut cd = fee_calc_sel.to_vec();
        cd.extend_from_slice(&u256_word(xp[0]));
        cd.extend_from_slice(&u256_word(xp[1]));
        let onchain = decode_u256(
            &client
                .eth_call(&pool, &cd, block)
                .expect("on-chain fee_calc"),
        );
        println!(
            "   xp=[{},{}] kernel_fee={} onchain_fee={}",
            xp[0], xp[1], kernel, onchain
        );
        assert_eq!(
            kernel, onchain,
            "TwoCrypto dynamic_fee must byte-match on-chain fee_calc at xp=[{},{}]",
            xp[0], xp[1]
        );
    }
    println!(
        "   TwoCrypto-NG: dynamic_fee BYTE-EXACT vs on-chain fee_calc (v2.1.0 form confirmed)."
    );
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 4) SolidlyV3 (classic Solidly STABLE, x³y+xy³=k) — `calculate_stable_swap_output_normalized` vs
//    the pool's own `getAmountOut(uint256 amountIn, address tokenIn)`.
//    Pool: 0x312702d569fb1b90d79d7ee06584c6feb9126588 (WETH/USDC.e, stable()==true, 5 bps).
//    The engine models this family via V2 `getReserves()` + the Solidly stable curve — exactly what
//    the pool's `getAmountOut` computes. Tier-1 target (same closed-form curve + same fee); the
//    engine's Newton-Raphson solve vs the pool's may differ by the solver's ≤1/1e6 tolerance →
//    small bound.
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn solidly_stable_quote_parity() {
    let (client, block, bn) = parity_client();
    let pool = addr("312702d569fb1b90d79d7ee06584c6feb9126588");
    let weth = addr(WETH);
    println!(
        "\n== SolidlyV3 (classic Solidly stable) parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    // State via the engine's V2-reserves reader (this family is V2-reserves-backed in the engine).
    let snap = engine_rpc::fetch_v2_reserves(&client, &pool, block)
        .expect("fetch_v2_reserves (Solidly stable getReserves) at pinned block");
    // token0/decimals via the pool views the engine relies on.
    let t0 = engine_rpc::fetch_token0(&client, &pool, block).expect("token0");
    let t1 = engine_rpc::fetch_token1(&client, &pool, block).expect("token1");
    let dec0 = engine_rpc::fetch_erc20_decimals(&client, &t0, block).expect("decimals0");
    let dec1 = engine_rpc::fetch_erc20_decimals(&client, &t1, block).expect("decimals1");
    println!(
        "   token0=0x{} ({dec0}dp) token1=0x{} ({dec1}dp) r0={} r1={}",
        hexa(&t0),
        hexa(&t1),
        snap.reserve0,
        snap.reserve1
    );

    // Direction: sell WETH. Identify which reserve is WETH.
    let weth_is_t0 = t0 == weth;
    let (r_in, r_out, dec_in, dec_out, token_in) = if weth_is_t0 {
        (
            U256::from(snap.reserve0),
            U256::from(snap.reserve1),
            dec0,
            dec1,
            weth,
        )
    } else {
        (
            U256::from(snap.reserve1),
            U256::from(snap.reserve0),
            dec1,
            dec0,
            weth,
        )
    };

    // Size off the WETH reserve so we stay in-band (the kernel rejects ratio > SOLIDLY_MAX).
    let weth_reserve: u128 = if weth_is_t0 {
        snap.reserve0
    } else {
        snap.reserve1
    };
    let sizes: [u128; 4] = [
        (weth_reserve / 1000).max(1_000_000_000_000),
        (weth_reserve / 200).max(10_000_000_000_000),
        (weth_reserve / 50).max(100_000_000_000_000),
        (weth_reserve / 10).max(1_000_000_000_000_000),
    ];
    let get_amount_out_sel = sel("getAmountOut(uint256,address)");
    let onchain_of = |amount_in: u128| -> U256 {
        let mut cd = get_amount_out_sel.to_vec();
        cd.extend_from_slice(&u256_word(U256::from(amount_in)));
        let mut tok = [0u8; 32];
        tok[12..32].copy_from_slice(&token_in);
        cd.extend_from_slice(&tok);
        decode_u256(
            &client
                .eth_call(&pool, &cd, block)
                .expect("on-chain getAmountOut"),
        )
    };
    let kernel_of = |amount_in: u128, fee_bps: u16| {
        engine_amm::solidly_stable::calculate_stable_swap_output_normalized(
            r_in,
            r_out,
            U256::from(amount_in),
            fee_bps,
            dec_in,
            dec_out,
        )
    };

    // `fee()` REVERTS on this original-Solidly "StableV1 AMM" fork (its stable fee lives in the
    // factory, 0.04%=4bps, not the 5bps Velodrome default the helper falls back to). Back-solve the
    // fee from the FIRST on-chain sample, then require the kernel to byte-match on-chain at EVERY
    // size with that SINGLE fee — a single fee scalar cannot fit all sizes unless the stable curve
    // (`calculate_stable_swap_output_normalized`) is byte-exact vs the deployed pool.
    let _ = solidly_fee_bps(&client, &pool, block); // documents that fee() reverts here -> None
    let oc0 = onchain_of(sizes[0]);
    let fee_bps = (0u16..=30)
        .find(|&f| kernel_of(sizes[0], f).map(|k| k == oc0).unwrap_or(false))
        .expect("a single fee in 0..=30 must reproduce the pool getAmountOut byte-exact");
    println!("   back-solved pool fee = {fee_bps} bps (fee() reverts; original-Solidly StableV1)");

    let mut checked = 0u32;
    for amount_in in sizes {
        let kernel = kernel_of(amount_in, fee_bps).expect("kernel solidly quote");
        let onchain = onchain_of(amount_in);
        println!("   in={amount_in} kernel={kernel} onchain={onchain}");
        assert_byte_exact("SolidlyStable", U256::from(amount_in), kernel, onchain);
        checked += 1;
    }
    assert_eq!(checked, 4, "every Solidly input size must compare");
    println!("   SolidlyV3 (classic stable): kernel BYTE-EXACT vs on-chain getAmountOut (fee={fee_bps}bps).");
}

/// Best-effort read of a classic Solidly stable pool's fee in bps. Most forks expose neither a
/// per-pool `fee()` nor a uniform getter; the factory holds the stable fee. Return None → caller
/// uses the engine's registered default (5 bps on Arbitrum).
fn solidly_fee_bps(client: &ArbitrumRpcClient, pool: &[u8; 20], block: BlockTag) -> Option<u16> {
    // Some forks: pool.fee() returns the fee in bps or in 1e6. Try it; reject implausible values.
    let resp = client.eth_call(pool, &sel("fee()"), block).ok()?;
    let v = decode_u256(&resp);
    let n: u128 = v.try_into().ok()?;
    if n == 0 {
        None
    } else if n <= 1000 {
        Some(n as u16) // already bps
    } else if n <= 1_000_000 {
        Some((n / 100) as u16) // 1e6 → bps
    } else {
        None
    }
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 5) KyberSwapElastic — `kyber_elastic::get_amount_out` vs KyberSwap Elastic QuoterV2.
//    Pool: resolved from the KyberSwap Elastic V1/LEGACY factory (0x5F1d…D50a =
//    legacy-QuoterV2.factory()) for WETH/USDC.e @ 40 feeUnits — exactly the pool that the
//    QuoterV2 below itself prices.
//    [AUD-012, resolved 2026-07-02; corrected 2026-07-20]: this test previously hardcoded pool
//    0xdb0f89c3…c949, which lives under a DIFFERENT factory (0xC7a59029…8C4A =
//    ElasticFactoryV2, KyberSwap's *current* Arbitrum Elastic deployment). That SPECIFIC pool
//    is a drained corpse — tick -343208 in a 130k-tick liquidity desert, dust liquidity, gutted
//    in the Nov-2023 Elastic exploit — so the kernel's "10^6x under-quote" was pricing the corpse
//    CORRECTLY and being compared against the healthy V1 pool (0x26333Db7…7072, getPool-resolved).
//    CORRECTION (2026-07-20, Dune-verified): 0xC7a59029…8C4A is NOT a dead *deployment* — the
//    factory as a whole still carries live pools (88 active, ~$1.67M vol since 2024 vs V1's
//    ~$0.59M/133 pools); only that one pool was drained. BOTH 0x5F1d…D50a (V1/legacy, created
//    Arbitrum block 14137735) and 0xC7a59029…8C4A (V2, block 92166935) are real Arbitrum Kyber
//    Elastic factories, and engine.json now discovers BOTH. (0x5F1d…D50a is a deterministic
//    CREATE2 address shared across chains incl. Aurora — it is Arbitrum's legacy factory, not
//    "Aurora's". Its deployment_block was previously mis-set to V2's 92166935, which silently
//    discovered ZERO V1 pools; fixed to V1's true deploy block 14137735.)
//    QuoterV2: 0x0D125c15D54cA1F8a813C74A81aEe34ebB508C1f,
//      quoteExactInputSingle((tokenIn,tokenOut,amountIn,uint24 feeUnits,uint160 limitSqrtP))
//      RETURNS (uint256 usedAmount, uint256 returnedAmount, uint160 afterSqrtP, uint32 ticks, uint256 gas)
//      → the OUTPUT is word #1 (returnedAmount), not word #0.
//    Tier-2 (documented): the engine prices Kyber on the V3 kernel and does NOT reinvest the fee
//    delta into liquidity per step, so it slightly UNDER-quotes on a healthy in-range pool (safe).
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn kyber_elastic_quote_parity() {
    let (client, block, bn) = parity_client();
    let quoter = addr("0d125c15d54ca1f8a813c74a81aee34ebb508c1f");
    // Resolve the pool the same way the quoter does: canonical factory getPool(t0, t1, 40).
    let factory = addr("5f1dddbf348ac2fbe22a163e30f99f9ece3dd50a");
    let weth = addr("82af49447d8a07e3bd95bd0d56f35241523fbab1");
    let usdce = addr("ff970a61a04b1ca14834a43f5de4533ebddb5cc8");
    let mut cd = vec![0x16, 0x98, 0xee, 0x82]; // getPool(address,address,uint24)
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(&weth);
    cd.extend_from_slice(&w);
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(&usdce);
    cd.extend_from_slice(&w);
    let mut w = [0u8; 32];
    w[31] = 40;
    cd.extend_from_slice(&w);
    let resp = client
        .eth_call(&factory, &cd, block)
        .expect("factory getPool");
    assert!(resp.len() >= 32, "getPool returns one address word");
    let mut pool = [0u8; 20];
    pool.copy_from_slice(&resp[12..32]);
    assert_ne!(
        pool, [0u8; 20],
        "canonical factory must know WETH/USDC.e@40"
    );
    println!(
        "\n== KyberSwapElastic QuoterV2 parity  pool=0x{} (factory-resolved)  block={bn} ==",
        hexa(&pool)
    );

    // State: getPoolState() (sqrtP+tick) + TOTAL liquidity (baseL+reinvestL — what Kyber prices
    // against), exactly the fields the engine's Kyber path reads.
    let s0 = engine_rpc::fetch_kyber_pool_state(&client, &pool, block).expect("kyber getPoolState");
    let liq_total = engine_rpc::fetch_kyber_liquidity_total(&client, &pool, block)
        .expect("kyber getLiquidityState (baseL+reinvestL)");
    // token0/token1 + fee units.
    let t0 = engine_rpc::fetch_token0(&client, &pool, block).expect("token0");
    let t1 = engine_rpc::fetch_token1(&client, &pool, block).expect("token1");
    let fee_units = kyber_swap_fee_units(&client, &pool, block).expect("kyber swapFeeUnits()");
    // [AUD-012] Kyber swapFeeUnits is denominated in FEE_UNITS = 1e5 (tiers 8/10/40/300/1000);
    // the engine kernel takes ppm (1e6) → 10× conversion. The old `fee_ppm == swapFeeUnits`
    // assumption under-charged the fee 10× (measured +3.6 bps over-quote on the 40-unit pool).
    let fee_ppm = engine_rpc::kyber_fee_units_to_ppm(fee_units);
    println!(
        "   token0=0x{} token1=0x{} sqrtP={} tick={} liquidity(base+reinvest)={} feeUnits={fee_units} (=> fee_ppm={fee_ppm})",
        hexa(&t0), hexa(&t1), s0.sqrt_price_x96, s0.tick, liq_total
    );

    // [AUD-012 FIX 2026-07-02] Kyber Elastic keeps its initialized ticks in an on-chain LINKED
    // LIST (`nearestCurrentTick` → `initializedTicks(t).prev/next`), NOT a spacing-window: on
    // this pool the current tick sits in a 130,240-tick liquidity desert (nearest=-346584,
    // next=-216344), so ANY window scan loads 0 ticks and the kernel under-quoted ~10^6x.
    // Walking the real list loads the exact ticks the Elastic swap loop crosses.
    // 64 per side covers the deepest tested size (1 ETH); an exhausted fetch range only ever
    // UNDER-quotes (kernel clamps at known liquidity — capital-safe), but the parity bound here
    // is strict, so fetch enough of the list for every tested size.
    let list = engine_rpc::fetch_kyber_initialized_ticks(&client, &pool, block, 64)
        .expect("kyber initialized-tick linked list");
    let ticks: Vec<TickInfo> = list
        .iter()
        .map(|(t, net)| TickInfo {
            tick: *t,
            liquidity_net: *net,
        })
        .collect();
    println!(
        "   loaded {} initialized ticks via linked list: {:?}",
        ticks.len(),
        list.iter().map(|(t, _)| *t).collect::<Vec<_>>()
    );

    // Sell token0 (WETH) → token1 (USDC.e): zero_for_one. Sizes in WETH.
    let zero_for_one = true;
    let token_in = t0;
    let token_out = t1;
    let sizes: [u128; 4] = [
        10_000_000_000_000_000,
        50_000_000_000_000_000,
        200_000_000_000_000_000,
        1_000_000_000_000_000_000,
    ];

    let mut checked = 0u32;
    for amount_in in sizes {
        let kernel = match engine_amm::kyber_elastic::get_amount_out(
            amount_in,
            s0.sqrt_price_x96,
            U256::from(liq_total),
            s0.tick,
            fee_ppm,
            zero_for_one,
            &ticks,
        ) {
            Ok((out, _sqrt_after)) => U256::from(out),
            Err(e) => {
                println!("   kernel kyber amount_in={amount_in} -> Err({e:?}) (skipping)");
                continue;
            }
        };
        // QuoterV2.quoteExactInputSingle((tokenIn,tokenOut,amountIn,feeUnits,limitSqrtP)) — single
        // struct arg (5 static words), so calldata = selector + the 5 words inline.
        let qsel = sel("quoteExactInputSingle((address,address,uint256,uint24,uint160))");
        let mut cd = qsel.to_vec();
        let mut w = [0u8; 32];
        w[12..32].copy_from_slice(&token_in);
        cd.extend_from_slice(&w);
        w = [0u8; 32];
        w[12..32].copy_from_slice(&token_out);
        cd.extend_from_slice(&w);
        cd.extend_from_slice(&u256_word(U256::from(amount_in)));
        // feeUnits (uint24, right-aligned).
        let mut fb = [0u8; 32];
        fb[29..32].copy_from_slice(&fee_units.to_be_bytes()[1..4]);
        cd.extend_from_slice(&fb);
        // limitSqrtP = 0 (no limit).
        cd.extend_from_slice(&u256_word(U256::ZERO));

        let ret = client
            .eth_call(&quoter, &cd, block)
            .expect("on-chain Kyber QuoterV2 quoteExactInputSingle");
        // Return tuple: word0 = usedAmount, word1 = returnedAmount (THE OUTPUT), word2 = afterSqrtP …
        let onchain = decode_word_n(&ret, 1);
        // Tier-2 documented bound: the engine omits per-step fee reinvestment → under-quotes by up
        // to the swap fee on the output. Bound = fee on onchain + a few wei. The over-quote eps is
        // tight (must NOT over-quote on a healthy pool).
        let bound = (onchain * U256::from(fee_ppm)) / U256::from(1_000_000u64) + U256::from(4u64);
        assert_bounded(
            "KyberElastic",
            U256::from(amount_in),
            kernel,
            onchain,
            bound,
            U256::from(4u64),
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "every Kyber input size must compare");
    println!("   KyberSwapElastic: kernel within the documented fee-reinvestment bound, no over-quote on a healthy in-range pool.");
}

/// KyberSwap Elastic per-pool fee in 1e6 units via `swapFeeUnits()` (selector 0xc79a590e). Kyber
/// pools expose `swapFeeUnits()`, NOT `fee()`.
fn kyber_swap_fee_units(
    client: &ArbitrumRpcClient,
    pool: &[u8; 20],
    block: BlockTag,
) -> Option<u32> {
    let resp = client.eth_call(pool, &sel("swapFeeUnits()"), block).ok()?;
    let v = decode_u256(&resp);
    let n: u64 = v.try_into().ok()?;
    if n == 0 || n > 1_000_000 {
        None
    } else {
        Some(n as u32)
    }
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 6) UniswapV4 — V3 concentrated-liquidity kernel on V4 state vs the on-chain V4 Quoter.
//    Pool: canonical WETH/USDC fee-500 HOOKLESS V4 pool — the ERC-20-only shape the engine admits.
//    StateView: 0x76fd297e2d437cd7f76d50f01afe6160f86e9990. Quoter: 0x3972c00f7ed4885e145823eb7c655375d275a1c5.
//    The engine registers ONLY hookless ERC-20 V4 pools, so V4 core swap math == V3 → exact target.
//    StateView returning valid state for the derived poolId IS the PoolKey-encoding self-check.
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn uniswap_v4_quote_parity() {
    let (client, block, bn) = parity_client();
    let state_view = addr("76fd297e2d437cd7f76d50f01afe6160f86e9990");
    let quoter = addr("3972c00f7ed4885e145823eb7c655375d275a1c5");
    let weth = addr(WETH);
    let usdc = addr(USDC);
    let hooks = [0u8; 20];
    let fee: u32 = 500;
    let tick_spacing: i32 = 10;

    let pk = encode_pool_key(&weth, &usdc, fee, tick_spacing, &hooks);
    let pool_id: [u8; 32] = keccak256(pk).into();
    println!(
        "\n== UniswapV4 Quoter parity  poolId=0x{}  block={bn} ==",
        hexa32(&pool_id)
    );

    let slot0 = engine_rpc::fetch_v4_slot0(&client, &state_view, &pool_id, block)
        .expect("V4 StateView getSlot0 (poolId encoding self-check) — pool absent or wrong key");
    let liq = engine_rpc::fetch_v4_liquidity(&client, &state_view, &pool_id, block)
        .expect("v4 liquidity");
    let window = engine_rpc::fetch_v4_tick_window(
        &client,
        &state_view,
        &pool_id,
        slot0.tick,
        tick_spacing,
        80,
        block,
    )
    .expect("v4 tick window");
    let ticks: Vec<TickInfo> = window
        .iter()
        .map(|t| TickInfo {
            tick: t.tick,
            liquidity_net: t.liquidity_net,
        })
        .collect();
    println!(
        "   sqrtP={} tick={} lpFee={} liquidity={} ticks={}",
        slot0.sqrt_price_x96,
        slot0.tick,
        slot0.lp_fee,
        liq,
        ticks.len()
    );

    let k_slot0 = V3Slot0 {
        sqrt_price_x96: slot0.sqrt_price_x96,
        tick: slot0.tick,
        fee_pips: slot0.lp_fee,
        liquidity: liq,
        tick_spacing,
    };

    // WETH(18) in (zero_for_one) and USDC(6) in (one_for_zero) — exercise both directions.
    let weth_sizes: [u128; 3] = [
        10_000_000_000_000_000,
        100_000_000_000_000_000,
        1_000_000_000_000_000_000,
    ];
    let usdc_sizes: [u128; 3] = [50_000_000, 1_000_000_000, 10_000_000_000];

    let mut checked = 0u32;
    for (zfo, sizes) in [(true, &weth_sizes), (false, &usdc_sizes)] {
        for &amt in sizes.iter() {
            let kernel = match engine_amm::v3_get_amount_out(k_slot0, U256::from(amt), zfo, &ticks)
            {
                Ok(o) => o.amount_out,
                Err(e) => {
                    println!("   kernel v4 amt={amt} zfo={zfo} -> Err({e:?}) (skipping)");
                    continue;
                }
            };
            let cd = encode_v4_quoter_call(&pk, zfo, amt);
            let onchain = decode_u256(
                &client
                    .eth_call(&quoter, &cd, block)
                    .expect("on-chain V4 Quoter"),
            );
            let dir = if zfo {
                "0->1(WETH in)"
            } else {
                "1->0(USDC in)"
            };
            assert_byte_exact(
                &format!("UniswapV4 {dir}"),
                U256::from(amt),
                kernel,
                onchain,
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 6, "every V4 direction/size must produce a comparison");
    println!(
        "   UniswapV4: V3-kernel-on-V4-state matches the on-chain V4 Quoter byte-exactly."
    );
}

/// abi.encode(PoolKey{currency0,currency1,fee,tickSpacing,hooks}) — 5 static 32-byte words.
fn encode_pool_key(
    c0: &[u8; 20],
    c1: &[u8; 20],
    fee: u32,
    tick_spacing: i32,
    hooks: &[u8; 20],
) -> [u8; 160] {
    let mut b = [0u8; 160];
    b[12..32].copy_from_slice(c0);
    b[44..64].copy_from_slice(c1);
    b[93..96].copy_from_slice(&fee.to_be_bytes()[1..4]); // uint24 fee
    let ts = (tick_spacing as u32).to_be_bytes();
    b[125..128].copy_from_slice(&ts[1..4]); // int24 tickSpacing (positive)
    b[140..160].copy_from_slice(hooks);
    b
}

/// quoteExactInputSingle(QuoteExactSingleParams{poolKey,zeroForOne,exactAmount,hookData=empty}).
fn encode_v4_quoter_call(pool_key: &[u8; 160], zero_for_one: bool, exact_amount: u128) -> Vec<u8> {
    let selector =
        sel("quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))");
    let mut cd = Vec::with_capacity(4 + 32 + 288);
    cd.extend_from_slice(&selector);
    // single dynamic tuple arg → offset 0x20
    let mut w = [0u8; 32];
    w[31] = 0x20;
    cd.extend_from_slice(&w);
    // params head: poolKey(5 words) + zeroForOne + exactAmount + hookData offset(0x100)
    cd.extend_from_slice(pool_key);
    let mut zfo = [0u8; 32];
    zfo[31] = if zero_for_one { 1 } else { 0 };
    cd.extend_from_slice(&zfo);
    let mut amt = [0u8; 32];
    amt[16..32].copy_from_slice(&exact_amount.to_be_bytes());
    cd.extend_from_slice(&amt);
    let mut hoff = [0u8; 32];
    hoff[30] = 0x01; // 0x100 params-relative offset to hookData
    cd.extend_from_slice(&hoff);
    cd.extend_from_slice(&[0u8; 32]); // hookData length 0
    cd
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 7) Fluid v1 — STUB. There is NO engine-rpc one-shot snapshot reader for FluidDex (confirmed: the
//    only Fluid reference in engine-rpc is the RpcError::data doc note for `estimateSwapIn`, which
//    returns its result via a REVERT with ABI-encoded error data). The kernel lives at
//    engine_amm::fluid::fluid_v1.
//
//    LEADER TODO to complete this family:
//      (a) Ground truth: Fluid v1 prices via the resolver, NOT a pool view. Call the Fluid
//          **DexReservesResolver** / **DexT1** `estimateSwapIn(address dex, bool swap0to1,
//          uint256 amountIn, uint256 amountOutMin)`; on Fluid it reverts with a custom error whose
//          ABI-encoded data carries the amountOut — decode `RpcError::Server { data, .. }` (the
//          client already surfaces `error.data`). Confirm the resolver address + the exact custom
//          error selector for the pinned block.
//      (b) State: build the `fluid_v1` kernel state from the same resolver's `getDexState` /
//          `getPoolReserves` views (imaginary reserves + center price + fee + the v1 split params).
//          There is no `fetch_fluid_*` in engine-rpc yet — either add one mirroring
//          cryptoswap_snapshot.rs, or read the raw resolver words inline here.
//      (c) Pool: use fluid_sample[0] from real_pools.json — address bytes
//          [11,26,81,62,226,73,114,218,239,17,43,199,119,165,97,13,67,37,201,231]
//          = 0x0b1a513ee49972daef112bc777a5610d4325c9e7 (wstETH/WETH, created at block 465686728).
//      (d) Assert the kernel quote (engine_amm::fluid::fluid_v1 entrypoint) == the decoded
//          estimateSwapIn amount within Fluid's documented fee/rounding bound (Tier-2: the kernel
//          models v1 as an imaginary-reserve CPMM; time-continuous interest is NOT evented, so a
//          small bound applies — assert NO over-quote).
//    Until (a)-(d) are filled, this test fails loudly so the gap is visible rather than silently
//    green. Remove the `panic!` once wired.
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn fluid_v1_quote_parity() {
    let (client, block, bn) = parity_client();
    // Fluid v1 wstETH/WETH pool + the official DexReservesResolver (Arbitrum 0x05Bd8269…).
    // `estimateSwapIn` is the resolver's swapIn-revert/catch/decode quoter (it catches the pool's
    // FluidDexSwapResult revert and returns amountOut), so it is a clean eth_call ground truth.
    let pool = addr("0b1a513ee24972daef112bc777a5610d4325c9e7");
    let resolver = addr("05bd8269a20c472b148246de20e6852091bf16ff");
    let now = 1_779_501_578u64; // block 465686728 timestamp (kernel limit-expansion uses `now`)
    println!(
        "\n== Fluid v1 estimateSwapIn parity  pool=0x{}  resolver=0x{}  block={bn} ==",
        hexa(&pool),
        hexa(&resolver)
    );

    // STATE READER: the engine's OWN production Fluid state builder, reused verbatim
    // (getPoolReservesAdjusted + getPoolLimits + dexVariables2 + constantsView2).
    let state = engine::fluid_poller::refresh_one_v1_pool_at(&client, &resolver, &pool, block)
        .expect("refresh_one_v1_pool_at (Fluid v1 state) at pinned block");

    let estimate_sel = sel("estimateSwapIn(address,bool,uint256,uint256)");
    let sizes: [u128; 4] = [
        1_000_000_000_000_000,   // 0.001
        5_000_000_000_000_000,   // 0.005
        20_000_000_000_000_000,  // 0.02
        100_000_000_000_000_000, // 0.1
    ];
    let mut checked = 0u32;
    for (swap0to1, dir) in [
        (true, engine_amm::fluid::SwapDirection::ZeroToOne),
        (false, engine_amm::fluid::SwapDirection::OneToZero),
    ] {
        for amount_in in sizes {
            // ORACLE: DexReservesResolver.estimateSwapIn(dex, swap0to1, amountIn, minOut=0).
            let mut cd = estimate_sel.to_vec();
            let mut dex_word = [0u8; 32];
            dex_word[12..32].copy_from_slice(&pool);
            cd.extend_from_slice(&dex_word);
            let mut dir_word = [0u8; 32];
            if swap0to1 {
                dir_word[31] = 1;
            }
            cd.extend_from_slice(&dir_word);
            cd.extend_from_slice(&u256_word(U256::from(amount_in)));
            cd.extend_from_slice(&u256_word(U256::ZERO));
            let onchain = decode_u256(
                &client
                    .eth_call(&resolver, &cd, block)
                    .expect("on-chain estimateSwapIn"),
            );

            let kernel = match state.quote_exact_in(dir, U256::from(amount_in), now) {
                Ok(q) => q.amount_out_native,
                Err(e) => {
                    println!("   swap0to1={swap0to1} in={amount_in}: kernel Err({e:?}) — skipping");
                    continue;
                }
            };
            println!("   swap0to1={swap0to1} in={amount_in} kernel={kernel} onchain={onchain}");
            assert_byte_exact(
                "FluidV1",
                U256::from(amount_in),
                kernel,
                onchain,
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 8, "every Fluid v1 direction/size must compare");
    println!("   Fluid v1: kernel quote_exact_in.amount_out_native == on-chain estimateSwapIn (DexReservesResolver) for {checked} quotes.");
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 8) DODO V2 (PMM) — `DodoState::quote_for_tokens_exact_in` vs the pool's OWN
//    `querySellBase(address trader, uint256 payBaseAmount)` / `querySellQuote(address,uint256)`
//    on-chain preview (the EXACT view `sellBase`/`sellQuote` execute against).
//    Pool: 0x1666EA6E83BCC3473111732Ef086BA47e6EDDeB7 (WETH base / USDC-native quote, DPP).
//      `_BASE_TOKEN_()`=WETH(18dp), `_QUOTE_TOKEN_()`=USDC(6dp), `version()`="DPP Advanced".
//    Oracle ABI: `querySellBase(address,uint256) -> (uint256 receiveQuote, uint256 mtFee)` — word#0
//    `receiveQuote` is the NET amount the trader receives (gross − LP fee − mt fee), i.e. exactly
//    the kernel's `DodoQuote::amount_out_native` (which subtracts BOTH lp and mt). Use trader=0.
//    Tier-1 target: the kernel is a byte-exact port of the deployed PMM (the forge T2
//    `test_T2_Dodo_predictor_matches_executed_sellBase` proves `sellBase` returns this to the wei);
//    route through `assert_bounded` with a tight 2-wei epsilon for any residual divDown rounding.
//    The over-quote guard MUST hold (a DODO over-quote is the phantom-profit class that was fixed).
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn dodo_v2_quote_parity() {
    use engine_amm::dodo_v2::DodoPoolClass;
    let (client, block, bn) = parity_client();
    let pool = addr("1666ea6e83bcc3473111732ef086ba47e6eddeb7"); // WETH/USDC.native DPP
    println!(
        "\n== DODO V2 querySellBase parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    // Discovery class is DPP for this pool; the snapshot reader overrides it from on-chain
    // `version()` anyway, so the passed class is only a fallback.
    let state = engine_rpc::fetch_dodo_state(&client, &pool, DodoPoolClass::Dpp, block, bn)
        .expect("fetch_dodo_state (WETH/USDC DPP) at pinned block");
    let base = state.base_token; // WETH (18dp)  (alloy_primitives::Address)
    let quote = state.quote_token; // USDC (6dp)
    let base_bytes: [u8; 20] = base.into();
    let quote_bytes: [u8; 20] = quote.into();
    println!(
        "   base=0x{} quote=0x{} i={} K={} B={} Q={} R={:?} fee_ppm={}",
        hexa(&base_bytes),
        hexa(&quote_bytes),
        state.i,
        state.k,
        state.base_reserve,
        state.quote_reserve,
        state.r_state,
        state.fee_ppm
    );

    // Sell BASE (WETH) for QUOTE (USDC). Size off the base reserve so we stay realistic + in-band.
    let base_reserve_u: u128 = state.base_reserve.try_into().unwrap_or(u128::MAX);
    let sizes: [u128; 4] = [
        (base_reserve_u / 5000).max(10_000_000_000_000), // ≥ 1e-5 WETH
        (base_reserve_u / 1000).max(50_000_000_000_000),
        (base_reserve_u / 200).max(200_000_000_000_000),
        (base_reserve_u / 50).max(1_000_000_000_000_000),
    ];
    let query_sell_base = sel("querySellBase(address,uint256)");

    let mut checked = 0u32;
    for dx in sizes {
        if dx == 0 {
            continue;
        }
        let q = match state.quote_for_tokens_exact_in(base, quote, U256::from(dx)) {
            Ok(q) => q,
            Err(e) => {
                println!("   kernel dodo dx={dx} -> Err({e:?}) (skipping size)");
                continue;
            }
        };
        let kernel = q.amount_out_native;
        // querySellBase(address(0), dx) → word0 = receiveQuote (NET out), word1 = mtFee.
        let mut cd = query_sell_base.to_vec();
        cd.extend_from_slice(&[0u8; 32]); // trader = address(0)
        cd.extend_from_slice(&u256_word(U256::from(dx)));
        let ret = client
            .eth_call(&pool, &cd, block)
            .expect("on-chain querySellBase");
        let onchain = decode_u256(&ret); // receiveQuote
        assert_byte_exact(
            "DodoV2-sellBase",
            U256::from(dx),
            kernel,
            onchain,
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "every DODO input size must compare");
    println!("   DODO V2: kernel amount_out_native == on-chain querySellBase receiveQuote byte-exactly.");
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 9) Trader Joe Liquidity Book (v2.1) — `trader_joe_lb::simulate_lb_swap` vs the pair's OWN
//    `getSwapOut(uint128 amountIn, bool swapForY) -> (uint128 amountInLeft, uint128 amountOut,
//    uint128 fee)` (the canonical on-chain quote view, identical in v2.1/v2.2).
//    Pair: 0x94d53be52706a155d27440c4a2434bea772a6f7c (tokenX=WETH 18dp, tokenY=USDC.e 6dp, binStep 15).
//    The engine's LB path reads getActiveId/getBinStep/getStatic+VariableFeeParameters + a ±50-bin
//    window and runs the SAME per-bin constant-sum + per-bin variable-fee traversal the contract
//    runs in `swap()`. Tier-1 target (same bin math, same fee recompute). The hard requirements:
//      * on-chain `amountInLeft` must be 0 (a fully-fillable swap) — else the size is out of our
//        ±50-bin snapshot window and we skip it (the kernel returns None / a partial we reject),
//      * kernel `amount_out` == on-chain `amountOut` byte-exact (±1 wei rounding epsilon).
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn trader_joe_lb_quote_parity() {
    let (client, block, bn) = parity_client();
    let pool = addr("94d53be52706a155d27440c4a2434bea772a6f7c"); // WETH/USDC.e LB, binStep 15
    println!(
        "\n== Trader Joe LB getSwapOut parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    let state = engine_rpc::fetch_lb_state(&client, &pool, block, bn)
        .expect("fetch_lb_state (WETH/USDC.e LB) at pinned block");
    // LB pairs expose getTokenX()/getTokenY(), NOT token0()/token1() (which REVERT) — use getTokenX.
    let tx_resp = client
        .eth_call(&pool, &sel("getTokenX()"), block)
        .expect("getTokenX");
    let mut tx = [0u8; 20];
    tx.copy_from_slice(&tx_resp[12..32]);
    println!(
        "   tokenX=0x{} active_id={} bin_step={} reserve_x={} reserve_y={} bins_in_window={}",
        hexa(&tx),
        state.active_id,
        state.bin_step,
        state.reserve_x,
        state.reserve_y,
        state.bin_map.len()
    );

    // Sell tokenX (WETH) → tokenY (USDC.e): in LB, tokenX is the lower-address token, so
    // zero_for_one == swapForY == true. Sizes in WETH (18dp). Keep them ≤ a few bins of depth so
    // the on-chain swap fully fills within our ±50-bin snapshot window (amountInLeft must be 0).
    let zero_for_one = true;
    let swap_for_y = true;
    let sizes: [u128; 4] = [
        10_000_000_000_000_000,  // 0.01 WETH
        50_000_000_000_000_000,  // 0.05 WETH
        200_000_000_000_000_000, // 0.2 WETH
        500_000_000_000_000_000, // 0.5 WETH
    ];
    let get_swap_out = sel("getSwapOut(uint128,bool)");

    let mut checked = 0u32;
    for amount_in in sizes {
        // On-chain getSwapOut(amountIn, swapForY) → (amountInLeft, amountOut, fee).
        let mut cd = get_swap_out.to_vec();
        let mut amt = [0u8; 32];
        amt[16..32].copy_from_slice(&amount_in.to_be_bytes());
        cd.extend_from_slice(&amt);
        let mut b = [0u8; 32];
        b[31] = if swap_for_y { 1 } else { 0 };
        cd.extend_from_slice(&b);
        let ret = client
            .eth_call(&pool, &cd, block)
            .expect("on-chain getSwapOut");
        let amount_in_left = decode_word_n(&ret, 0);
        let onchain_out = decode_word_n(&ret, 1);
        if !amount_in_left.is_zero() {
            // The on-chain pool itself couldn't fully fill this size at this block — out of band
            // for a parity comparison (it would compare a full kernel quote to a partial). Skip.
            println!("   in={amount_in}: on-chain amountInLeft={amount_in_left} (partial) — skipping (out of ±50-bin band)");
            continue;
        }
        let kernel = match engine_amm::trader_joe_lb::simulate_lb_swap(
            &state,
            U256::from(amount_in),
            zero_for_one,
        ) {
            Some((out, _final_id)) => out,
            None => {
                println!(
                    "   in={amount_in}: kernel simulate_lb_swap -> None (partial/empty) — skipping"
                );
                continue;
            }
        };
        assert_byte_exact(
            "TraderJoeLB",
            U256::from(amount_in),
            kernel,
            onchain_out,
        );
        checked += 1;
    }
    assert_eq!(
        checked, 4,
        "every Trader Joe input must fully fill and compare; partial/empty is unresolved evidence"
    );
    println!("   Trader Joe LB: kernel simulate_lb_swap == on-chain getSwapOut amountOut byte-exactly for {checked} sizes.");
}

// ───────────────── shared Balancer V2 ground-truth: BalancerQueries.queryBatchSwap ─────────────────
/// Canonical Balancer V2 Vault on Arbitrum (the asset registry `getPoolTokens` keys into).
const BALANCER_VAULT: &str = "ba12222222228d8ba445958a75a0704d566bf2c8";
/// BalancerQueries standalone helper on Arbitrum (deployment `20220725-balancer-queries`). It is
/// deployed deterministically to the same address on every chain. `queryBatchSwap` is NOT `view`
/// (it reverts internally with the result) but is designed to be simulated via `eth_call`.
const BALANCER_QUERIES: &str = "e39b5e3b6d74016b2f6a9673d7d7493b6df549d5";

/// ABI-encode + `eth_call` `BalancerQueries.queryBatchSwap(SwapKind.GIVEN_IN, [oneStep], [in,out],
/// funds)` for a single-hop exact-in swap on `pool_id`, selling `asset_in`→`asset_out`, and return
/// the OUTPUT magnitude (the delta for `asset_out`, which Balancer returns as a NEGATIVE int256 —
/// pool pays out — so we negate it to a positive `U256`).
///
/// Layout (single dynamic tuple-ish call; `queryBatchSwap` has 4 args, two of them dynamic):
///   selector
///   [head] kind, off(swaps), off(assets), funds(4 inline words)
///   [swaps]  len=1, then BatchSwapStep{poolId, assetInIndex=0, assetOutIndex=1, amount, off(userData)}
///            then userData len=0
///   [assets] len=2, asset_in, asset_out
fn balancer_query_batch_swap_out(
    client: &ArbitrumRpcClient,
    pool_id: &[u8; 32],
    asset_in: &[u8; 20],
    asset_out: &[u8; 20],
    amount_in: U256,
    block: BlockTag,
) -> U256 {
    let queries = addr(BALANCER_QUERIES);
    let selector = sel(
        "queryBatchSwap(uint8,(bytes32,uint256,uint256,uint256,bytes)[],address[],(address,bool,address,bool))",
    );
    // Word helpers.
    let w_u = |x: u64| u256_word(U256::from(x));
    let w_addr = |a: &[u8; 20]| {
        let mut b = [0u8; 32];
        b[12..32].copy_from_slice(a);
        b
    };

    let mut cd = selector.to_vec();
    // ── head (4 top-level args), encoded as 7 words:
    //   [kind][off(swaps)][off(assets)] + funds(4 inline static words: sender,fromInternal,
    //   recipient,toInternal). `funds` is an ALL-STATIC tuple so it is inlined in the head (NOT an
    //   offset). 3 + 4 = 7 words = 0xE0. The two dynamic args (swaps[], assets[]) carry offsets,
    //   relative to the start of the ARGUMENT area (right after the selector).
    cd.extend_from_slice(&w_u(0)); // SwapKind.GIVEN_IN = 0
    let mut swaps_block: Vec<u8> = Vec::new();
    // swaps[] : len=1
    swaps_block.extend_from_slice(&w_u(1));
    // BatchSwapStep tuple is dynamic (contains `bytes userData`), so the array's element is laid
    // out as: [head: off-to-tuple] then the tuple body. For a single dynamic element:
    //   element-head: one word = offset (0x20) to the tuple body, relative to start of element area.
    swaps_block.extend_from_slice(&w_u(0x20));
    // tuple body: poolId, assetInIndex(0), assetOutIndex(1), amount, off(userData)
    swaps_block.extend_from_slice(pool_id);
    swaps_block.extend_from_slice(&w_u(0)); // assetInIndex
    swaps_block.extend_from_slice(&w_u(1)); // assetOutIndex
    swaps_block.extend_from_slice(&u256_word(amount_in)); // amount (exact-in)
    swaps_block.extend_from_slice(&w_u(0xA0)); // off(userData) within tuple body (5 words in)
    swaps_block.extend_from_slice(&w_u(0)); // userData len = 0
                                            // assets[] : len=2, asset_in, asset_out
    let mut assets_block: Vec<u8> = Vec::new();
    assets_block.extend_from_slice(&w_u(2));
    assets_block.extend_from_slice(&w_addr(asset_in));
    assets_block.extend_from_slice(&w_addr(asset_out));

    let off_swaps = 0xE0u64; // after the 7-word head (3 + funds(4))
    let off_assets = off_swaps + swaps_block.len() as u64;
    cd.extend_from_slice(&w_u(off_swaps)); // off(swaps)
    cd.extend_from_slice(&w_u(off_assets)); // off(assets)
                                            // funds = (sender=0, fromInternalBalance=false, recipient=0, toInternalBalance=false)
    cd.extend_from_slice(&[0u8; 32]); // sender
    cd.extend_from_slice(&w_u(0)); // fromInternalBalance
    cd.extend_from_slice(&[0u8; 32]); // recipient
    cd.extend_from_slice(&w_u(0)); // toInternalBalance
                                   // tail
    cd.extend_from_slice(&swaps_block);
    cd.extend_from_slice(&assets_block);

    let ret = client
        .eth_call(&queries, &cd, block)
        .expect("BalancerQueries.queryBatchSwap eth_call");
    // Returns int256[] (one delta per asset). assetDeltas[1] = output asset delta (negative: the
    // Vault pays it out). Magnitude = output amount. Array: [off][len][delta0][delta1].
    // off is word0 → typically 0x20.
    let arr_off: usize = decode_u256(&ret).try_into().unwrap_or(0x20);
    let len = decode_word_n(&ret, arr_off / 32);
    assert!(
        len == U256::from(2u64),
        "queryBatchSwap returned {len} deltas, expected 2"
    );
    let delta_out = decode_word_n(&ret, arr_off / 32 + 2); // assetDeltas[1]
                                                           // delta_out is a two's-complement negative int256 (pool pays out). Negate to a positive U256.
    if delta_out.bit(255) {
        // negative: magnitude = (2^256 - delta_out) = (~delta_out + 1) = -delta_out
        (!delta_out).wrapping_add(U256::from(1u64))
    } else {
        // already positive (would be unusual for the output leg, but handle it)
        delta_out
    }
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 10) Balancer V2 WEIGHTED — `balancer_weighted::calc_out_given_in` (LogExpMath pow) vs the pool's
//     own math via `BalancerQueries.queryBatchSwap` (GIVEN_IN).
//     Pool: 0x0e1cdc10a131d07636fb4cf322f79b8df551dd9e (50/50 BAL/WETH, both 18dp, swapFee 1e16=1%).
//     poolId 0x0e1cdc10a131d07636fb4cf322f79b8df551dd9e00020000000000000000046a.
//     The Vault applies the swap fee on the INPUT (raw), then WeightedMath runs in the upscaled
//     1e18 domain — so we replicate: fee = mulUp(amountIn, swapFee); upscale (amountIn−fee); call
//     `calc_out_given_in`; downscale the output (divDown). Tier-1 target: the port is byte-exact to
//     Balancer's `_calcOutGivenIn`; LogExpMath's `powUp` carries a +1e-14 upward bias, so the kernel
//     may OVER-shoot the query by a few wei → the over-quote epsilon is the LogExpMath bias bound,
//     and the under-quote bound is the same magnitude. (Documented: ~1e-9 relative at most.)
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn balancer_weighted_quote_parity() {
    let (client, block, bn) = parity_client();
    let vault = addr(BALANCER_VAULT);
    let pool = addr("0e1cdc10a131d07636fb4cf322f79b8df551dd9e"); // 50/50 BAL/WETH
    let weth = addr(WETH);
    println!(
        "\n== Balancer V2 Weighted parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    let slot = engine_rpc::fetch_balancer_weighted(&client, &vault, &pool, block)
        .expect("fetch_balancer_weighted (50/50 BAL/WETH) at pinned block");
    let n = slot.n_tokens as usize;
    assert!(n >= 2, "need >=2 tokens");
    // Find WETH's index; sell WETH into the other token.
    let i = (0..n)
        .find(|&k| slot.tokens[k] == weth)
        .expect("WETH must be a pool token");
    let j = (0..n).find(|&k| k != i).expect("a second token");
    println!(
        "   poolId=0x{} tokens[in={i},out={j}] bal_in_d18={} bal_out_d18={} w_in={} w_out={} fee_d18={}",
        hexa32(&slot.pool_id), slot.balances_d18[i], slot.balances_d18[j],
        slot.weights_d18[i], slot.weights_d18[j], slot.swap_fee_d18
    );

    // FixedPoint.mulUp(a,b) = ceil(a*b/1e18) — the Vault's input-fee rounding (rounds the fee UP).
    let one_d18 = U256::from(1_000_000_000_000_000_000u128);
    let mul_up = |a: U256, b: U256| -> U256 {
        if a.is_zero() || b.is_zero() {
            return U256::ZERO;
        }
        (a * b - U256::from(1u64)) / one_d18 + U256::from(1u64)
    };

    // Sizes in WETH (18dp), well under Balancer's 30% MAX_IN_RATIO.
    let sizes: [u128; 4] = [
        10_000_000_000_000_000,    // 0.01 WETH
        100_000_000_000_000_000,   // 0.1 WETH
        500_000_000_000_000_000,   // 0.5 WETH
        2_000_000_000_000_000_000, // 2 WETH
    ];
    let mut checked = 0u32;
    for amount_in in sizes {
        let ain = U256::from(amount_in);
        // 1) fee on raw input, then upscale (amountIn − fee) to the 1e18 domain.
        let fee = mul_up(ain, slot.swap_fee_d18);
        let after_fee = ain - fee;
        let after_fee_d18 = after_fee * slot.scaling_d18[i];
        // 2) WeightedMath in the upscaled domain.
        let out_d18 = match engine_amm::balancer_weighted::calc_out_given_in(
            slot.balances_d18[i],
            slot.weights_d18[i],
            slot.balances_d18[j],
            slot.weights_d18[j],
            after_fee_d18,
        ) {
            Ok(o) => o,
            Err(e) => {
                println!("   kernel weighted amount_in={amount_in} -> Err({e:?}) (skipping)");
                continue;
            }
        };
        // 3) downscale to raw output-token units (divDown = floor).
        let kernel = out_d18 / slot.scaling_d18[j];

        let onchain = balancer_query_batch_swap_out(
            &client,
            &slot.pool_id,
            &slot.tokens[i],
            &slot.tokens[j],
            ain,
            block,
        );
        // LogExpMath powUp bias bound: a handful of wei in the 1e18 domain → ≤ a few wei after the
        // divDown downscale. 8 wei covers it comfortably on an 18-dec pair; assert NO larger drift
        // in EITHER direction (the +1e-14 bias can push the kernel slightly ABOVE the query).
        let eps = U256::from(8u64);
        assert_bounded("BalancerWeighted", ain, kernel, onchain, eps, eps);
        checked += 1;
    }
    assert_eq!(checked, 4, "every Balancer Weighted input size must compare");
    println!(
        "   Balancer Weighted: kernel calc_out_given_in == queryBatchSwap (±LogExpMath bias)."
    );
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// 11) Balancer V2 STABLE — `balancer_stable::calc_out_given_in` (StableMath) vs the pool's own math
//     via `BalancerQueries.queryBatchSwap` (GIVEN_IN).
//     Pool: 0x1533a3278f3f9141d5f820a184ea4b017fce2382 (legacy StablePool "staBAL3":
//     DAI(18) / USD₮0(6) / USDC.e(6), A=1000, swapFee 3e14). It is NOT a ComposableStable: it has
//     NO BPT in the Vault token list and `getScalingFactors()`/`getBptIndex()` REVERT, so the
//     reader derives scaling from each token's decimals (10^(36−d)) and `bpt_index=None`.
//     We build the kernel `BalancerStableState` EXACTLY as engine startup does (skip `bpt_index`),
//     then drive USDC.e→USD₮0 (a stable–stable leg). Tier-1 target: the kernel is an integer-exact
//     port of `StableMath`; route through `assert_bounded` with a tight 2-wei epsilon.
// ════════════════════════════════════════════════════════════════════════════════════════════
#[test]
#[ignore = "RPC-gated: run with ARBITRUM_RPC_URL and --ignored"]
fn balancer_stable_quote_parity() {
    use engine_amm::balancer_stable::{BalancerStableState, MAX_TOKENS};
    let (client, block, bn) = parity_client();
    let vault = addr(BALANCER_VAULT);
    let pool = addr("1533a3278f3f9141d5f820a184ea4b017fce2382"); // staBAL3 DAI/USDT/USDC.e
    println!(
        "\n== Balancer V2 Stable parity  pool=0x{}  block={bn} ==",
        hexa(&pool)
    );

    let view = engine_rpc::fetch_balancer_stable(&client, &vault, &pool, block)
        .expect("fetch_balancer_stable (staBAL3) at pinned block");
    println!(
        "   poolId=0x{} amp={} fee={} bpt_index={:?} tokens={}",
        hexa32(&view.pool_id),
        view.amp,
        view.swap_fee_1e18,
        view.bpt_index,
        view.tokens.len()
    );

    // Build the kernel state EXACTLY as crates/engine/src/startup.rs does: copy every non-BPT
    // token's (balance, scaling, address) in registry order; n = count after BPT exclusion.
    let mut kbal = [0u128; MAX_TOKENS];
    let mut ksf = [U256::ZERO; MAX_TOKENS];
    let mut ktok = [[0u8; 20]; MAX_TOKENS];
    let mut n = 0usize;
    for idx in 0..view.tokens.len() {
        if Some(idx) == view.bpt_index {
            continue;
        }
        if n >= MAX_TOKENS {
            break;
        }
        kbal[n] = view.balances[idx].try_into().unwrap_or(u128::MAX);
        ksf[n] = view.scaling_factors[idx];
        ktok[n] = view.tokens[idx];
        n += 1;
    }
    assert!(n >= 2, "stable pool needs >=2 non-BPT tokens, got {n}");
    let kernel_state = BalancerStableState {
        n,
        balances: kbal,
        scaling_factors: ksf,
        amp: view.amp,
        swap_fee_1e18: view.swap_fee_1e18,
    };
    for k in 0..n {
        println!(
            "   coin[{k}]=0x{} bal={} sf={}",
            hexa(&ktok[k]),
            kbal[k],
            ksf[k]
        );
    }

    // Drive a stable–stable leg between the two 6-dec tokens if present (USD₮0 ↔ USDC.e), else
    // 0→1. Pick i,j as the first two distinct kernel coins (deterministic on this pool: DAI=0,
    // USD₮0=1, USDC.e=2 → use 1→2, both 6dp, the cleanest equal-decimal leg).
    let (i, j) = if n >= 3 {
        (1usize, 2usize)
    } else {
        (0usize, 1usize)
    };
    // Decimals of the input coin: infer from its scaling factor (sf = 10^(36 − dec)).
    // For sizing we just need realistic raw units; use a stablecoin-ish ladder.
    let in_is_6dp = ksf[i] == U256::from(10u64).pow(U256::from(30u64)); // 10^(36-6)
    let unit: u128 = if in_is_6dp {
        1_000_000
    } else {
        1_000_000_000_000_000_000
    };
    let sizes: [u128; 4] = [unit, 100 * unit, 1_000 * unit, 25_000 * unit];

    let mut checked = 0u32;
    for amount_in in sizes {
        let kernel =
            match engine_amm::balancer_stable::calc_out_given_in(&kernel_state, i, j, amount_in) {
                Ok(o) => U256::from(o),
                Err(e) => {
                    println!("   kernel stable amount_in={amount_in} -> Err({e:?}) (skipping)");
                    continue;
                }
            };
        let onchain = balancer_query_batch_swap_out(
            &client,
            &view.pool_id,
            &ktok[i],
            &ktok[j],
            U256::from(amount_in),
            block,
        );
        assert_byte_exact(
            "BalancerStable",
            U256::from(amount_in),
            kernel,
            onchain,
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "every Balancer Stable input size must compare");
    println!(
        "   Balancer Stable: kernel calc_out_given_in == queryBatchSwap byte-exactly."
    );
}

// ───────────────────────────── tiny hex formatters (for logs) ─────────────────────────────
fn hexa(a: &[u8; 20]) -> String {
    let mut s = String::with_capacity(40);
    for b in a {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
fn hexa32(a: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in a {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
