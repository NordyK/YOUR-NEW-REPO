//! Algebra repair probe (2026-07-10): on ONE failing Camelot V1.9 pool at ONE pin, print the
//! kernel quote for a ladder of sizes alongside the pool snapshot (sqrtP, tick, liquidity, fee,
//! tick window) so an offline fit against the on-chain quoter ladder can decide WHICH model the
//! DEPLOYED contract implements (stored-fee delta? effective-liquidity scale? step-fee shape?).
//!
//! Usage: ARBITRUM_RPC_URL=<archive> cargo run -p engine --example algebra_probe -- \
//!          --pool 0xb1026b8e… --block 482074237 [--zfo true]

use alloy_primitives::U256;
use engine_amm::{TickInfo, V3Slot0};
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};

fn arg(name: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| default.to_string())
}

fn addr20(s: &str) -> [u8; 20] {
    let s = s.trim().trim_start_matches("0x");
    let mut a = [0u8; 20];
    for (k, b) in a.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * k..2 * k + 2], 16).expect("hex addr");
    }
    a
}

fn main() {
    let pool = addr20(&arg("--pool", "0xb1026b8e7276e7ac75410f1fcbbe21796e8f7526"));
    let block_num: u64 = arg("--block", "482074237").parse().expect("block");
    let zfo: bool = arg("--zfo", "true").parse().expect("zfo");
    let spacing: i32 = arg("--spacing", "60").parse().expect("spacing");
    let url = std::env::var("ARBITRUM_RPC_URL").expect("ARBITRUM_RPC_URL");
    let client = ArbitrumRpcClient::new(url);
    let block = BlockTag::Number(block_num);

    let gs = engine_rpc::fetch_algebra_global_state(&client, &pool, block).expect("globalState");
    let liq = engine_rpc::fetch_v3_liquidity(&client, &pool, block).expect("liquidity");
    let win = engine_rpc::fetch_v3_tick_window(&client, &pool, gs.tick, spacing.max(1), 120, block)
        .expect("ticks");
    let ticks: Vec<TickInfo> = win
        .iter()
        .map(|t| TickInfo {
            tick: t.tick,
            liquidity_net: t.liquidity_net,
        })
        .collect();
    println!(
        "snapshot pool=0x{} block={} sqrtP={} tick={} fee={} L={} ticks={}",
        pool.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        block_num,
        gs.sqrt_price_x96,
        gs.tick,
        gs.fee_pips,
        liq,
        ticks.len()
    );
    for t in &ticks {
        println!("TICK {} {}", t.tick, t.liquidity_net);
    }
    // Size ladder: 0.05 → 2.0 ETH-ish input (18 dp assumed for zfo=true WETH side).
    let ladder: [u128; 10] = [
        50_000_000_000_000_000,
        100_000_000_000_000_000,
        200_000_000_000_000_000,
        400_000_000_000_000_000,
        600_000_000_000_000_000,
        800_000_000_000_000_000,
        1_000_000_000_000_000_000,
        1_300_000_000_000_000_000,
        1_600_000_000_000_000_000,
        2_000_000_000_000_000_000,
    ];
    // Kernel at the STORED fee and at a grid of alternative fees, so the fit can compare.
    for fee in [gs.fee_pips, 100, 110, 120, 130, 150] {
        let slot0 = V3Slot0 {
            sqrt_price_x96: gs.sqrt_price_x96,
            tick: gs.tick,
            fee_pips: fee,
            liquidity: liq,
            tick_spacing: spacing.max(1),
        };
        for &amt in &ladder {
            match engine_amm::v3_get_amount_out(slot0, U256::from(amt), zfo, &ticks) {
                Ok(o) => println!("KERNEL fee={fee} in={amt} out={}", o.amount_out),
                Err(e) => println!("KERNEL fee={fee} in={amt} err={e:?}"),
            }
        }
    }
}
