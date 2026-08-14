//! DETECTOR RECALL harness (runbook v1 Phase D2 / Phase G — detector-generated).
//!
//! For each real historical 2-leg arb (Dune-sourced, cached block range), reconstruct the two
//! pools' EXACT pre-state at the parent block and run the engine's OWN kernels
//! (`v3_get_amount_out` + the live `optimize_cycle_amount` golden-section sizer) to see whether
//! the detector independently discovers a profitable cyclic route. Reports detected/total.
//!
//! Scope (honest): this is ROUTE/MATH-layer recall on reconstructable V3/V3 same-pair 2-leg arbs
//! — "given the dirty pools, does enumerate+size find the arb?". Wakeup-layer recall (feed decode)
//! is the separate live question (Gate 8/9).
//!
//! Candidates passed as argv triples: <parent_block> <poolA> <poolB> [<parent_block> <poolA> ...]
//! Run: ARBITRUM_RPC_URL=… cargo run -p engine-rpc --example recall_detector --release -- <triples>
use alloy_primitives::U256;
use engine_amm::uniswap_v3::Slot0;
use engine_amm::TickInfo;
use engine_rpc::client::{ArbitrumRpcClient, BlockTag, RpcError};
use engine_rpc::{fetch_v3_liquidity, fetch_v3_slot0, fetch_v3_tick_window};

const WETH: &str = "82af49447d8a07e3bd95bd0d56f35241523fbab1";

fn addr(s: &str) -> [u8; 20] {
    let s = s.trim_start_matches("0x");
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}
fn call(
    rpc: &ArbitrumRpcClient,
    pool: &[u8; 20],
    sel: [u8; 4],
    block: BlockTag,
) -> Result<Vec<u8>, RpcError> {
    rpc.eth_call(pool, &sel, block)
}
fn word_u32(resp: &[u8]) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&resp[28..32]);
    u32::from_be_bytes(b)
}
fn addr_from_word(resp: &[u8]) -> String {
    faster_hex::hex_string(&resp[12..32]).to_string()
}

struct PoolV3 {
    token0: String,
    token1: String,
    slot0: Slot0,
    ticks: Vec<TickInfo>,
}

fn load(rpc: &ArbitrumRpcClient, pool_hex: &str, block: BlockTag) -> Option<PoolV3> {
    let pool = addr(pool_hex);
    let t0 = call(rpc, &pool, [0x0d, 0xfe, 0x16, 0x81], block).ok()?; // token0()
    let t1 = call(rpc, &pool, [0xd2, 0x12, 0x20, 0xa7], block).ok()?; // token1()
    let fee = call(rpc, &pool, [0xdd, 0xca, 0x3f, 0x43], block)
        .ok()
        .map(|r| word_u32(&r))
        .unwrap_or(0); // fee()
    let spacing = call(rpc, &pool, [0xd0, 0xc9, 0x3a, 0x7c], block)
        .ok()
        .map(|r| word_u32(&r) as i32)
        .unwrap_or(0); // tickSpacing()
    if fee == 0 || spacing <= 0 {
        return None;
    } // not a standard V3 pool (Algebra/V2/V4)
    let s0 = fetch_v3_slot0(rpc, &pool, block).ok()?;
    let liq = fetch_v3_liquidity(rpc, &pool, block).ok()?;
    // window width scaled so 16+ WETH swaps don't run dry: ±300 spacings worth, capped at 400 calls.
    let width = (300.min(2000 / spacing.max(1))).max(40);
    let win = fetch_v3_tick_window(rpc, &pool, s0.tick, spacing, width, block).ok()?;
    let ticks = win
        .iter()
        .map(|t| TickInfo {
            tick: t.tick,
            liquidity_net: t.liquidity_net,
        })
        .collect();
    Some(PoolV3 {
        token0: addr_from_word(&t0),
        token1: addr_from_word(&t1),
        slot0: Slot0 {
            sqrt_price_x96: s0.sqrt_price_x96,
            tick: s0.tick,
            fee_pips: fee,
            liquidity: liq,
            tick_spacing: spacing,
        },
        ticks,
    })
}

/// quote one hop: amount of `tok_out` for `amount_in` of the pool's other token.
fn hop(p: &PoolV3, amount_in: u128, sell_token0: bool) -> Option<u128> {
    // zero_for_one = selling token0 for token1
    let r = engine_amm::v3_get_amount_out(p.slot0, U256::from(amount_in), sell_token0, &p.ticks)
        .ok()?;
    if r.amount_out.is_zero() {
        None
    } else {
        Some(r.amount_out.to::<u128>())
    }
}

/// Evaluate a 2-pool cycle starting from `start` token, going start->mid on p1 then mid->start on p2.
/// Returns a closure quote(amount_in)->amount_out for the engine sizer.
fn make_cycle<'a>(
    p1: &'a PoolV3,
    p2: &'a PoolV3,
    start_is_token0_p1: bool,
    start_is_token0_p2: bool,
) -> impl Fn(u128) -> Option<u128> + 'a {
    move |amount_in: u128| {
        // hop1 on p1: sell `start` -> get `mid`. sell_token0 = start_is_token0_p1
        let mid = hop(p1, amount_in, start_is_token0_p1)?;
        // hop2 on p2: sell `mid` -> get `start`. mid is the *other* token on p2, so sell_token0 = !start_is_token0_p2
        hop(p2, mid, !start_is_token0_p2)
    }
}

/// engine's live sizer (pipeline.rs:3386 optimize_cycle_amount): coarse grid + golden-section,
/// extends to grid[bi]*4 at the ceiling. Returns (best_size, best_gross).
fn size<F: Fn(u128) -> Option<u128>>(grid: &[u128], quote: F) -> Option<(u128, u128)> {
    let mut best: Option<(u128, u128)> = None;
    let mut bi = 0usize;
    for (i, &a) in grid.iter().enumerate() {
        if a == 0 {
            continue;
        }
        if let Some(o) = quote(a) {
            if o > a {
                let g = o - a;
                if best.is_none_or(|(_, bg)| g > bg) {
                    best = Some((a, g));
                    bi = i;
                }
            }
        }
    }
    let (_, bg0) = best?;
    let lo = if bi > 0 { grid[bi - 1] } else { grid[bi] / 4 };
    let hi = if bi + 1 < grid.len() {
        grid[bi + 1]
    } else {
        grid[bi].saturating_mul(4)
    };
    let gross = |x: u128| match quote(x) {
        Some(o) if o > x => o - x,
        _ => 0,
    };
    let (mut a, mut b) = (lo, hi);
    let span = |a: u128, b: u128| b.saturating_sub(a);
    let mut c = b.saturating_sub(span(a, b) * 618 / 1000);
    let mut d = a.saturating_add(span(a, b) * 618 / 1000);
    let (mut fc, mut fd) = (gross(c), gross(d));
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
            fd = gross(d);
        } else {
            b = d;
            d = c;
            fd = fc;
            c = b.saturating_sub(span(a, b) * 618 / 1000);
            fc = gross(c);
        }
    }
    let mid = (a + b) / 2;
    let gm = gross(mid);
    Some(if gm > bg0 { (mid, gm) } else { best.unwrap() })
}

fn main() {
    let rpc = ArbitrumRpcClient::new(std::env::var("ARBITRUM_RPC_URL").expect("ARBITRUM_RPC_URL"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let grid: Vec<u128> = vec![
        5e14 as u128,
        1e15 as u128,
        5e15 as u128,
        1e16 as u128,
        25e15 as u128,
        5e16 as u128,
        1e17 as u128,
        25e16 as u128,
        5e17 as u128,
        1e18 as u128,
        5e18 as u128,
        1e19 as u128,
    ];
    let mut total = 0;
    let mut hits = 0;
    println!("# DETECTOR RECALL — engine kernels on real pre-state V3/V3 2-leg arbs");
    for tri in args.chunks(3) {
        if tri.len() < 3 {
            break;
        }
        total += 1;
        let pb: u64 = tri[0].parse().unwrap();
        let blk = BlockTag::Number(pb);
        let (pa, pbb) = (&tri[1], &tri[2]);
        let (Some(p1), Some(p2)) = (load(&rpc, pa, blk), load(&rpc, pbb, blk)) else {
            println!(
                "case parent={pb} {}/{}: SKIP (non-V3 pool / load fail)",
                &pa[..10],
                &pbb[..10]
            );
            continue;
        };
        // require same pair
        let same = (p1.token0 == p2.token0 && p1.token1 == p2.token1)
            || (p1.token0 == p2.token1 && p1.token1 == p2.token0);
        if !same {
            println!(
                "case parent={pb}: SKIP (pools not same pair: {}/{} vs {}/{})",
                p1.token0, p1.token1, p2.token0, p2.token1
            );
            continue;
        }
        // pick start token = WETH if present else token0 of p1
        let start = if p1.token0 == WETH || p1.token1 == WETH {
            WETH.to_string()
        } else {
            p1.token0.clone()
        };
        let s_is_t0_p1 = p1.token0 == start;
        let s_is_t0_p2 = p2.token0 == start;
        // two routes: p1-first and p2-first; take best positive
        let r1 = size(&grid, make_cycle(&p1, &p2, s_is_t0_p1, s_is_t0_p2));
        let r2 = size(&grid, make_cycle(&p2, &p1, s_is_t0_p2, s_is_t0_p1));
        let best = match (r1, r2) {
            (Some(a), Some(b)) => Some(if a.1 >= b.1 { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            _ => None,
        };
        match best {
            Some((sz, g)) if g > 0 => {
                hits += 1;
                println!("case parent={pb} pair start=0x{}: DETECTED size={:.4} gross={:.6} ETH-units (1e18)", &start[..8], sz as f64/1e18, g as f64/1e18);
            }
            _ => println!(
                "case parent={pb} pair start=0x{}: no-profit (detector found no positive cycle)",
                &start[..8]
            ),
        }
    }
    println!("\nRECALL: {hits}/{total} reconstructable V3/V3 cases — engine kernels discovered a profitable cycle at exact pre-state");
}
