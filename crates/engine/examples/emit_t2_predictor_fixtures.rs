//! T2 fixture generator — the Rust half of `MantisC2Predictor.Fork.t.sol` (TEST_PLAN §7 T2,
//! DEPLOYMENT_READINESS §8 #7: "C2 predictor == real post-swap state").
//!
//! For each C2 family (V2, V3, Curve StableSwap-NG, DODO, Balancer-Stable, Trader Joe LB) this
//! snapshots a live pool at one pinned block via the engine's OWN `engine-rpc` readers, runs the
//! production predictor math (`apply_*` / the V2/V3 kernel swap the predictors use) for a chosen
//! `dx`, and ALSO quotes a second swap `dx2` off the PREDICTED post-state. The forge test then
//! forks the same block, executes the real swaps, and asserts:
//!   received(dx)  == predicted out      → the quote is right,
//!   post-state    == predicted state    → the predictor is right,
//!   received(dx2) == predicted out2     → the predicted state QUOTES right (the property the
//!                                         fast lane actually depends on).
//!
//! Families not covered here and why: CryptoSwap venue is OFF (failed its W6 parity gate —
//! research/dex/curve_cryptoswap/PARITY_CASE.md); Balancer-Weighted has no C2 delta-apply by
//! design (its slot stores pre-scaled d18 balances, so the Vault `Swap` log wakes a targeted
//! resync instead — pipeline.rs `log_pool_family`), and its quote kernel is gated off
//! (research/dex/balancer_v2/WEIGHTED_IMPOSTOR_CASE.md).
//!
//! Run: ARBITRUM_RPC_URL=<alchemy> cargo run --release -p engine --example emit_t2_predictor_fixtures
//! Writes: engine/contracts/test/fixtures/t2_predictor.json (pinned block inside).

use alloy_primitives::U256;
use engine_amm::TickInfo;
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use serde_json::json;

fn hexlit(s: &str) -> [u8; 20] {
    let mut a = [0u8; 20];
    faster_hex::hex_decode(s.as_bytes(), &mut a).expect("hexlit");
    a
}

fn hex0x(a: &[u8; 20]) -> String {
    let mut s = vec![0u8; 40];
    faster_hex::hex_encode(a, &mut s).unwrap();
    format!("0x{}", String::from_utf8(s).unwrap())
}

fn hex0x32(a: &[u8; 32]) -> String {
    let mut s = vec![0u8; 64];
    faster_hex::hex_encode(a, &mut s).unwrap();
    format!("0x{}", String::from_utf8(s).unwrap())
}

// Verified-live pools (2026-06-11). Each is the same pool the engine's other live gates use.
const V2_PAIR: &str = "f64dfe17c8b87f012fcf50fbda1d62bfa148366a"; // UniV2 WETH/USDC, 30 bps
const V3_POOL: &str = "c6962004f452be9203591991d15f6b388e09e8d0"; // UniV3 WETH/USDC, fee 500
const DODO_POOL: &str = "0d32a3bb04e8ff29f0974c9d3f3d85012a0b5d14"; // DODO WBTC/USDC DVM
const BAL_POOL: &str = "1533a3278f3f9141d5f820a184ea4b017fce2382"; // Balancer 3-token stable
const LB_PAIR: &str = "94d53be52706a155d27440c4a2434bea772a6f7c"; // LB v2.1 WETH/USDC.e bin15
const CURVE_FACTORY: &str = "9af14d26075f142eb3f292d5065eb3faa646167b"; // StableSwap-NG factory

fn main() {
    let url = std::env::var("ARBITRUM_RPC_URL").expect("ARBITRUM_RPC_URL");
    let client = ArbitrumRpcClient::new(url);
    let bn = client.block_number().expect("block_number");
    let block = BlockTag::Number(bn);
    println!("T2 fixture generation @block {bn}");
    let mut out = serde_json::Map::new();
    out.insert("block".into(), json!(bn));

    // ---- V2: kernel get_amount_out + reserve deltas (the V2 predictor's exact model) ----
    {
        let pair = hexlit(V2_PAIR);
        let snap = engine_rpc::fetch_v2_reserves(&client, &pair, block).expect("v2 reserves");
        let dx: u128 = 50_000_000_000_000_000; // 0.05 WETH (token0)
        let dx2: u128 = 20_000_000_000_000_000;
        let fee_bps = 30;
        let out1 =
            engine_amm::uniswap_v2::get_amount_out(dx, snap.reserve0, snap.reserve1, fee_bps)
                .expect("v2 quote");
        let r0_after = snap.reserve0 + dx;
        let r1_after = snap.reserve1 - out1;
        let out2 = engine_amm::uniswap_v2::get_amount_out(dx2, r0_after, r1_after, fee_bps)
            .expect("v2 quote2");
        println!("V2 {} dx={dx} out={out1} out2={out2}", hex0x(&pair));
        out.insert("v2_pair".into(), json!(hex0x(&pair)));
        out.insert("v2_dx".into(), json!(dx.to_string()));
        out.insert("v2_out".into(), json!(out1.to_string()));
        out.insert("v2_r0_after".into(), json!(r0_after.to_string()));
        out.insert("v2_r1_after".into(), json!(r1_after.to_string()));
        out.insert("v2_dx2".into(), json!(dx2.to_string()));
        out.insert("v2_out2".into(), json!(out2.to_string()));
    }

    // ---- V3: kernel multi-tick swap → out + post sqrtP/tick/liquidity (the V3 predictor) ----
    {
        let pool = hexlit(V3_POOL);
        let s0 = engine_rpc::fetch_v3_slot0(&client, &pool, block).expect("v3 slot0");
        let liq = engine_rpc::fetch_v3_liquidity(&client, &pool, block).expect("v3 liquidity");
        let spacing = 10i32;
        let window = engine_rpc::fetch_v3_tick_window(&client, &pool, s0.tick, spacing, 100, block)
            .expect("v3 window");
        let ticks: Vec<TickInfo> = window
            .iter()
            .map(|t| TickInfo {
                tick: t.tick,
                liquidity_net: t.liquidity_net,
            })
            .collect();
        let slot0 = engine_amm::uniswap_v3::Slot0 {
            sqrt_price_x96: s0.sqrt_price_x96,
            tick: s0.tick,
            fee_pips: 500,
            liquidity: liq,
            tick_spacing: spacing,
        };
        let dx = U256::from(50_000_000_000_000_000u128); // 0.05 WETH in (zero_for_one)
        let dx2 = U256::from(20_000_000_000_000_000u128);
        let r1 = engine_amm::uniswap_v3::get_amount_out_with_final_tick(slot0, dx, true, &ticks)
            .expect("v3 swap");
        assert_eq!(r1.amount_in_consumed, dx, "v3 window must cover dx");
        let slot0_after = engine_amm::uniswap_v3::Slot0 {
            sqrt_price_x96: r1.sqrt_price_x96_after,
            tick: r1.tick_after,
            fee_pips: 500,
            liquidity: r1.liquidity_after,
            tick_spacing: spacing,
        };
        let r2 =
            engine_amm::uniswap_v3::get_amount_out_with_final_tick(slot0_after, dx2, true, &ticks)
                .expect("v3 swap2");
        println!(
            "V3 {} dx={dx} out={} sqrt_after={} tick_after={} out2={}",
            hex0x(&pool),
            r1.amount_out,
            r1.sqrt_price_x96_after,
            r1.tick_after,
            r2.amount_out
        );
        out.insert("v3_pool".into(), json!(hex0x(&pool)));
        out.insert("v3_dx".into(), json!(dx.to_string()));
        out.insert("v3_out".into(), json!(r1.amount_out.to_string()));
        out.insert(
            "v3_sqrt_after".into(),
            json!(r1.sqrt_price_x96_after.to_string()),
        );
        out.insert("v3_tick_after".into(), json!(r1.tick_after));
        out.insert("v3_liq_after".into(), json!(r1.liquidity_after.to_string()));
        out.insert("v3_dx2".into(), json!(dx2.to_string()));
        out.insert("v3_out2".into(), json!(r2.amount_out.to_string()));
    }

    // ---- Curve StableSwap-NG: apply_exchange (the C2 curve predictor) ----
    {
        // Enumerate the NG factory and take the first pool that (a) snapshots, (b) has all
        // balances live, (c) is roughly balanced — the static-fee kernel does not model the
        // offpeg dynamic fee, which only bites on imbalanced pools (documented in startup.rs).
        let factory = hexlit(CURVE_FACTORY);
        let sel = |sig: &[u8]| -> [u8; 4] {
            use tiny_keccak::{Hasher, Keccak};
            let mut k = Keccak::v256();
            k.update(sig);
            let mut o = [0u8; 32];
            k.finalize(&mut o);
            [o[0], o[1], o[2], o[3]]
        };
        let count_resp = client
            .eth_call(&factory, &sel(b"pool_count()"), block)
            .expect("pool_count");
        let n_pools: u64 = U256::from_be_slice(&count_resp[..32]).try_into().unwrap();
        let mut chosen = None;
        for idx in 0..n_pools {
            let mut d = sel(b"pool_list(uint256)").to_vec();
            d.extend_from_slice(&U256::from(idx).to_be_bytes::<32>());
            let Ok(resp) = client.eth_call(&factory, &d, block) else {
                continue;
            };
            let pool: [u8; 20] = resp[12..32].try_into().unwrap();
            let Ok(view) = engine_rpc::fetch_curve_state(&client, &pool, 3, block) else {
                continue;
            };
            let n = view.kernel.n_coins as usize;
            if n < 2 {
                continue;
            }
            let bals = &view.kernel.balances[..n];
            if bals.iter().any(|b| *b == 0) {
                continue;
            }
            // Balance ratio guard (in 1e18-normalized terms via rates).
            let xp: Vec<u128> = (0..n)
                .map(|i| {
                    ((U256::from(view.kernel.balances[i]) * U256::from(view.kernel.rates[i]))
                        / U256::from(10u64).pow(U256::from(18)))
                    .try_into()
                    .unwrap_or(u128::MAX)
                })
                .collect();
            let mx = *xp.iter().max().unwrap();
            let mn = *xp.iter().min().unwrap();
            if mn == 0 || mx / mn > 2 || mn < 1_000_000_000_000_000_000_000 {
                continue; // imbalanced or < 1000 units of liquidity
            }
            chosen = Some((pool, view));
            break;
        }
        let (pool, view) = chosen.expect("no balanced live NG pool found");
        let dx = view.kernel.balances[0] / 500;
        let dx2 = view.kernel.balances[0] / 1000;
        let (dy, st1) =
            engine_amm::curve::apply_exchange(&view.kernel, 0, 1, dx).expect("curve apply");
        let (dy2, _) = engine_amm::curve::apply_exchange(&st1, 0, 1, dx2).expect("curve apply2");
        // Documented tolerance: the pool keeps the LP share of the fee in balances and books the
        // admin share out; bound the j-balance drift by the full fee on dy (fee is 1e10-scaled).
        let fee_tol = (U256::from(dy) * U256::from(view.kernel.fee))
            / U256::from(10_000_000_000u64)
            + U256::from(1u8);
        println!(
            "CURVE {} coins0={} coins1={} dx={dx} dy={dy} dy2={dy2} tol={fee_tol}",
            hex0x(&pool),
            hex0x(&view.tokens[0]),
            hex0x(&view.tokens[1])
        );
        out.insert("curve_pool".into(), json!(hex0x(&pool)));
        out.insert("curve_coin_in".into(), json!(hex0x(&view.tokens[0])));
        out.insert("curve_coin_out".into(), json!(hex0x(&view.tokens[1])));
        out.insert("curve_dx".into(), json!(dx.to_string()));
        out.insert("curve_dy".into(), json!(dy.to_string()));
        out.insert(
            "curve_bal_i_after".into(),
            json!(st1.balances[0].to_string()),
        );
        out.insert(
            "curve_bal_j_after".into(),
            json!(st1.balances[1].to_string()),
        );
        out.insert("curve_fee_tol".into(), json!(fee_tol.to_string()));
        out.insert("curve_dx2".into(), json!(dx2.to_string()));
        out.insert("curve_dy2".into(), json!(dy2.to_string()));
    }

    // ---- DODO V2: DodoState::apply_swap (the C2 DODO predictor) ----
    {
        use engine_amm::dodo_v2::{DodoPoolClass, SwapDirection};
        let pool = hexlit(DODO_POOL);
        let state = [DodoPoolClass::Dvm, DodoPoolClass::Dpp, DodoPoolClass::Dsp]
            .into_iter()
            .find_map(|cls| engine_rpc::fetch_dodo_state(&client, &pool, cls, block, bn).ok())
            .expect("dodo snapshot");
        let dx = state.base_reserve / U256::from(500u64);
        let dx2 = state.base_reserve / U256::from(1000u64);
        let (out1, st1) = state
            .apply_swap(dx, SwapDirection::BaseToQuote)
            .expect("dodo apply");
        let q2 = st1
            .quote_exact_in_local(dx2, SwapDirection::BaseToQuote)
            .expect("dodo quote2");
        // apply_swap omits only the maintainer-fee slice that also leaves the pool; bound the
        // quote-reserve drift by the full fee on out1 (fee_ppm covers lp+mt).
        let q_tol = (out1 * (state.lp_fee_rate_d18 + state.mt_fee_rate_d18))
            / U256::from(10u64).pow(U256::from(18))
            + U256::from(1u8);
        println!(
            "DODO {} base={} quote={} dx={dx} out={out1} out2={} qtol={q_tol}",
            hex0x(&pool),
            hex0x(&state.base_token),
            hex0x(&state.quote_token),
            q2.amount_out_native
        );
        out.insert("dodo_pool".into(), json!(hex0x(&pool)));
        out.insert("dodo_base".into(), json!(hex0x(&state.base_token)));
        out.insert("dodo_quote".into(), json!(hex0x(&state.quote_token)));
        out.insert("dodo_dx".into(), json!(dx.to_string()));
        out.insert("dodo_out".into(), json!(out1.to_string()));
        out.insert("dodo_b_after".into(), json!(st1.base_reserve.to_string()));
        out.insert("dodo_q_after".into(), json!(st1.quote_reserve.to_string()));
        out.insert("dodo_q_tol".into(), json!(q_tol.to_string()));
        out.insert("dodo_dx2".into(), json!(dx2.to_string()));
        out.insert("dodo_out2".into(), json!(q2.amount_out_native.to_string()));
    }

    // ---- Balancer Stable: balancer_stable::apply_swap (the C2 family-6 predictor) ----
    {
        let pool = hexlit(BAL_POOL);
        let vault = engine_feed::BALANCER_VAULT_ARBITRUM;
        let view = engine_rpc::fetch_balancer_stable(&client, &vault, &pool, block)
            .expect("balancer snapshot");
        use engine_amm::balancer_stable::{BalancerStableState, MAX_TOKENS};
        let mut kbal = [0u128; MAX_TOKENS];
        let mut ksf = [U256::ZERO; MAX_TOKENS];
        let mut ktok = [[0u8; 20]; MAX_TOKENS];
        let mut n = 0usize;
        for idx in 0..view.tokens.len() {
            if Some(idx) == view.bpt_index {
                continue;
            }
            kbal[n] = view.balances[idx].try_into().expect("balance fits u128");
            ksf[n] = view.scaling_factors[idx];
            ktok[n] = view.tokens[idx];
            n += 1;
        }
        assert!(n >= 2, "need >=2 live tokens");
        let kernel = BalancerStableState {
            n,
            balances: kbal,
            scaling_factors: ksf,
            amp: view.amp,
            swap_fee_1e18: view.swap_fee_1e18,
        };
        let dx = kbal[0] / 500;
        let dx2 = kbal[0] / 1000;
        let (out1, st1) =
            engine_amm::balancer_stable::apply_swap(&kernel, 0, 1, dx).expect("bal apply");
        let (out2, _) =
            engine_amm::balancer_stable::apply_swap(&st1, 0, 1, dx2).expect("bal apply2");
        // Tolerance: the LP fee share stays in pool balances (the kernel's flat delta omits it);
        // bound by the full swap fee on dx (fee is 1e18-scaled, taken on the input side).
        let fee_tol: U256 = (U256::from(dx) * U256::from(view.swap_fee_1e18))
            / U256::from(10u64).pow(U256::from(18))
            + U256::from(1u8);
        println!(
            "BAL {} tin={} tout={} dx={dx} out={out1} out2={out2} tol={fee_tol}",
            hex0x(&pool),
            hex0x(&ktok[0]),
            hex0x(&ktok[1])
        );
        out.insert("bal_pool".into(), json!(hex0x(&pool)));
        out.insert("bal_pool_id".into(), json!(hex0x32(&view.pool_id)));
        out.insert("bal_token_in".into(), json!(hex0x(&ktok[0])));
        out.insert("bal_token_out".into(), json!(hex0x(&ktok[1])));
        out.insert("bal_dx".into(), json!(dx.to_string()));
        out.insert("bal_out".into(), json!(out1.to_string()));
        out.insert("bal_in_after".into(), json!(st1.balances[0].to_string()));
        out.insert("bal_out_after".into(), json!(st1.balances[1].to_string()));
        out.insert("bal_fee_tol".into(), json!(fee_tol.to_string()));
        out.insert("bal_dx2".into(), json!(dx2.to_string()));
        out.insert("bal_out2".into(), json!(out2.to_string()));
    }

    // ---- Trader Joe LB: trader_joe_lb::apply_swap_at (the C2 LB predictor, evaluated at the
    // pinned block's timestamp — the fee references decay to the EXECUTION time on-chain) ----
    {
        let pair = hexlit(LB_PAIR);
        let state = engine_rpc::lb_snapshot::fetch_lb_state(&client, &pair, block, bn)
            .expect("lb snapshot");
        // Exact pinned-block timestamp via Multicall3.getCurrentBlockTimestamp() at `block`.
        let multicall3 = hexlit("ca11bde05977b3631167028862be2a173976ca11");
        let ts_resp = client
            .eth_call(&multicall3, &[0x0f, 0x28, 0xc9, 0x7d], block)
            .expect("block timestamp");
        let ts: u64 = U256::from_be_slice(&ts_resp[..32]).try_into().unwrap();
        let dx = U256::from(50_000_000_000_000_000u128); // 0.05 WETH (tokenX) → USDC.e
        let dx2 = U256::from(20_000_000_000_000_000u128);
        let (out1, st1) =
            engine_amm::trader_joe_lb::apply_swap_at(&state, dx, true, ts).expect("lb apply");
        let (out2, _) =
            engine_amm::trader_joe_lb::apply_swap_at(&st1, dx2, true, ts).expect("lb apply2");
        println!(
            "LB {} dx={dx} out={out1} active_after={} out2={out2}",
            hex0x(&pair),
            st1.active_id
        );
        out.insert("lb_pair".into(), json!(hex0x(&pair)));
        out.insert("lb_dx".into(), json!(dx.to_string()));
        out.insert("lb_out".into(), json!(out1.to_string()));
        out.insert("lb_active_id_after".into(), json!(st1.active_id));
        out.insert("lb_dx2".into(), json!(dx2.to_string()));
        out.insert("lb_out2".into(), json!(out2.to_string()));
    }

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/test/fixtures/t2_predictor.json"
    );
    std::fs::write(
        path,
        serde_json::to_string_pretty(&serde_json::Value::Object(out)).unwrap(),
    )
    .expect("write fixture");
    println!("WROTE {path}");
}
