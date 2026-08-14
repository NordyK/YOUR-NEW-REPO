//! GATE 3 — detector-route proof at the EXACT pre-transaction state.
//!
//! Companion to the Solidity forced-route harness (`MantisGate3PreState.Fork.t.sol`). That
//! test proves the EXECUTOR realizes positive WETH at the pre-state. THIS harness proves the
//! ENGINE's OWN detection kernels — the V3 swap math (`engine_amm::v3_get_amount_out`) driving
//! the sizing grid (`engine_sim::sizing::grid_search_v3v3`) — independently discover the
//! profitable cyclic route from the real pre-state pool snapshots, with no hand-rolled math.
//!
//! Known historical arb (Dune): block 465686729, tx 0xbd25…a016da (tx index 4).
//!   Pancake V3 (fee 100) WETH->WBTC, then Uniswap V3 (fee 500) WBTC->WETH.
//! Pre-state = parent block 465686728 (verified: txs 0..3 in 465686729 do not touch either pool).
//!
//! Run:
//!   ARBITRUM_RPC_URL=… cargo run -p engine-rpc --example gate3_detector --release
use engine_amm::uniswap_v3::Slot0;
use engine_amm::TickInfo;
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use engine_rpc::{fetch_v3_liquidity, fetch_v3_slot0, fetch_v3_tick_window};
use engine_sim::sizing::grid_search_v3v3;

const PRE_STATE_BLOCK: u64 = 465686728;
// Pancake V3 WETH/WBTC fee 100, spacing 1. token0=WBTC, token1=WETH.
const POOL_PANCAKE: &str = "4bfc22a4da7f31f8a912a79a7e44a822398b4390";
// Uniswap V3 WBTC/WETH fee 500, spacing 10. token0=WBTC, token1=WETH.
const POOL_UNI: &str = "2f5e87c9312fa29aed5c179e456625d79015299c";

fn addr(s: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

fn load_pool(
    rpc: &ArbitrumRpcClient,
    pool_hex: &str,
    fee_pips: u32,
    tick_spacing: i32,
    width: i32,
    block: BlockTag,
) -> (Slot0, Vec<TickInfo>) {
    let pool = addr(pool_hex);
    let s0 = fetch_v3_slot0(rpc, &pool, block).expect("slot0");
    let liq = fetch_v3_liquidity(rpc, &pool, block).expect("liquidity");
    let win =
        fetch_v3_tick_window(rpc, &pool, s0.tick, tick_spacing, width, block).expect("tick window");
    let ticks: Vec<TickInfo> = win
        .iter()
        .map(|t| TickInfo {
            tick: t.tick,
            liquidity_net: t.liquidity_net,
        })
        .collect();
    let slot0 = Slot0 {
        sqrt_price_x96: s0.sqrt_price_x96,
        tick: s0.tick,
        fee_pips,
        liquidity: liq,
        tick_spacing,
    };
    println!(
        "  pool 0x{pool_hex}: sqrtP={} tick={} liq={} fee_pips={} spacing={} warm_ticks={}",
        slot0.sqrt_price_x96,
        slot0.tick,
        slot0.liquidity,
        fee_pips,
        tick_spacing,
        ticks.len()
    );
    (slot0, ticks)
}

fn main() {
    let rpc_url = std::env::var("ARBITRUM_RPC_URL").expect("set ARBITRUM_RPC_URL");
    let rpc = ArbitrumRpcClient::new(&rpc_url);
    let block = BlockTag::Number(PRE_STATE_BLOCK);

    println!("GATE3 DETECTOR — pre-state block {PRE_STATE_BLOCK}");
    println!("Hop A: Pancake V3 (fee100) WETH->WBTC  (token1->token0 => zero_for_one=false)");
    let (slot_a, ticks_a) = load_pool(&rpc, POOL_PANCAKE, 100, 1, 400, block);
    println!("Hop B: Uniswap V3 (fee500) WBTC->WETH  (token0->token1 => zero_for_one=true)");
    let (slot_b, ticks_b) = load_pool(&rpc, POOL_UNI, 500, 10, 120, block);

    // EXACT production live-pipeline grid (engine/src/pipeline.rs:1715) — 12 buckets capped at
    // 10 ETH. The coarse scan picks the TOP bucket here (the optimum is beyond 10 ETH), then the
    // live refiner brackets [grid[bi-1], grid[bi]*4] = [5,40] WETH and must climb to the optimum.
    let grid: Vec<u128> = vec![
        500_000_000_000_000,
        1_000_000_000_000_000,
        5_000_000_000_000_000,
        10_000_000_000_000_000,
        25_000_000_000_000_000,
        50_000_000_000_000_000,
        100_000_000_000_000_000,
        250_000_000_000_000_000,
        500_000_000_000_000_000,
        1_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
    ];

    // zero_for_one_a = false (WETH=token1 -> WBTC=token0); zero_for_one_b = true (WBTC=token0 -> WETH=token1).
    let best = grid_search_v3v3(&grid, slot_a, &ticks_a, false, slot_b, &ticks_b, true);

    println!("\n--- detector grid (engine kernels) ---");
    for &g in &grid {
        // Re-evaluate per bucket for a full curve print.
        let mid =
            engine_amm::v3_get_amount_out(slot_a, alloy_primitives::U256::from(g), false, &ticks_a);
        let prof = match mid {
            Ok(m) if !m.amount_out.is_zero() => {
                match engine_amm::v3_get_amount_out(slot_b, m.amount_out, true, &ticks_b) {
                    Ok(o) if o.amount_out > alloy_primitives::U256::from(g) => {
                        (o.amount_out - alloy_primitives::U256::from(g)).to::<u128>()
                    }
                    _ => 0,
                }
            }
            _ => 0,
        };
        println!(
            "  size {:>6.2} WETH -> profit {} wei ({} x1e-4 ETH)",
            g as f64 / 1e18,
            prof,
            prof / 100_000_000_000_000
        );
    }

    let (idx, o) = best.expect("GATE3 DETECTOR FAIL: grid_search_v3v3 returned None");
    println!("\nGATE3 DETECTOR coarse-grid best bucket idx={idx}");
    println!(
        "  grid best size   = {} wei ({:.4} WETH)",
        o.amount_in,
        o.amount_in as f64 / 1e18
    );
    println!(
        "  grid gross profit= {} wei ({:.6} WETH)",
        o.gross_profit,
        o.gross_profit as f64 / 1e18
    );
    assert!(
        o.gross_profit > 0,
        "GATE3 DETECTOR FAIL: kernels found no profit at pre-state"
    );

    // --- GATE 5: reproduce the LIVE sizer (pipeline.rs:3386 optimize_cycle_amount) on the same
    // engine V3 quote kernel. evaluate_route() uses THIS refiner (not the grid-only kernel):
    // coarse grid -> golden-section in [grid[bi-1], grid[bi+1]] (or grid[bi]*4 at the ceiling).
    let quote = |amount_in: u128| -> Option<u128> {
        let m = engine_amm::v3_get_amount_out(
            slot_a,
            alloy_primitives::U256::from(amount_in),
            false,
            &ticks_a,
        )
        .ok()?;
        if m.amount_out.is_zero() {
            return None;
        }
        let out = engine_amm::v3_get_amount_out(slot_b, m.amount_out, true, &ticks_b).ok()?;
        Some(out.amount_out.to::<u128>())
    };
    let gross_at = |x: u128| -> u128 {
        match quote(x) {
            Some(out) if out > x => out - x,
            _ => 0,
        }
    };
    // top bucket won (idx == grid.len()-1): lo = grid[bi-1], hi = grid[bi]*4 (== 40 WETH)
    let lo = if idx > 0 {
        grid[idx - 1]
    } else {
        grid[idx] / 4
    };
    let hi = if idx + 1 < grid.len() {
        grid[idx + 1]
    } else {
        grid[idx].saturating_mul(4)
    };
    let (mut a, mut b) = (lo, hi);
    let span = |a: u128, b: u128| b.saturating_sub(a);
    let mut c = b.saturating_sub(span(a, b) * 618 / 1000);
    let mut d = a.saturating_add(span(a, b) * 618 / 1000);
    let (mut fc, mut fd) = (gross_at(c), gross_at(d));
    for _ in 0..16 {
        let tol = (a.max(b) / 100).max(2);
        if span(a, b) <= tol {
            break;
        }
        if fc < fd {
            a = c;
            c = d;
            fc = fd;
            d = a.saturating_add(span(a, b) * 618 / 1000);
            fd = gross_at(d);
        } else {
            b = d;
            d = c;
            fd = fc;
            c = b.saturating_sub(span(a, b) * 618 / 1000);
            fc = gross_at(c);
        }
    }
    let mid = (a + b) / 2;
    let refined_gross = gross_at(mid);
    // sizer returns max(coarse, refined)
    let (final_size, final_gross) = if refined_gross > o.gross_profit {
        (mid, refined_gross)
    } else {
        (o.amount_in, o.gross_profit)
    };
    println!(
        "\nGATE5 LIVE sizer (optimize_cycle_amount) bracket=[{:.1},{:.1}] WETH",
        lo as f64 / 1e18,
        hi as f64 / 1e18
    );
    println!("  refined size  = {:.4} WETH", final_size as f64 / 1e18);
    println!(
        "  refined gross = {} wei ({:.6} WETH)",
        final_gross,
        final_gross as f64 / 1e18
    );
    // Oracle optimum established by the Solidity forced harness = 49388179306558003 wei @ 16 WETH.
    let oracle = 49_388_179_306_558_003u128;
    let ratio = final_gross as f64 / oracle as f64 * 100.0;
    println!(
        "  oracle optimum= {} wei ({:.6} WETH)",
        oracle,
        oracle as f64 / 1e18
    );
    println!("  profit_ratio  = {:.2}% of oracle optimum", ratio);
    assert!(
        ratio >= 95.0,
        "GATE5 FAIL: live sizer captured <95% of oracle optimum"
    );
    println!("\nGATE3 DETECTOR: PASS — engine kernels discover the WETH->WBTC->WETH cycle at exact pre-state.");
    println!("GATE5 SIZER: PASS — live refiner captures {ratio:.2}% of oracle optimum (>=95%).");
}
