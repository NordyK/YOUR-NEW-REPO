//! Live validation of the Uniswap V4 StateView snapshot path against a known poolId.
//!
//! Run: `ARBITRUM_RPC_URL=… cargo run -p engine-rpc --example repro_v4`

use engine_rpc::client::{ArbitrumRpcClient, BlockTag};
use engine_rpc::{fetch_v4_liquidity, fetch_v4_slot0, fetch_v4_tick_window};

fn main() {
    let rpc = std::env::var("ARBITRUM_RPC_URL").expect("set ARBITRUM_RPC_URL");
    let client = ArbitrumRpcClient::new(&rpc);

    let state_view: [u8; 20] = hexa("76fd297e2d437cd7f76d50f01afe6160f86e9990");
    let pool_id = hex32("ab05003a63d2f34ac7eec4670bca3319f0e3d2f62af5c2b9cbd69d03fd804fd2");

    let slot0 = fetch_v4_slot0(&client, &state_view, &pool_id, BlockTag::Latest).expect("slot0");
    let liq = fetch_v4_liquidity(&client, &state_view, &pool_id, BlockTag::Latest).expect("liq");
    println!(
        "getSlot0: sqrtPriceX96={} tick={} lpFee={}",
        slot0.sqrt_price_x96, slot0.tick, slot0.lp_fee
    );
    println!("getLiquidity: {liq}");

    // tickSpacing for this pool is 1 (set bits are contiguous compressed ticks); use 1.
    let ticks = fetch_v4_tick_window(
        &client,
        &state_view,
        &pool_id,
        slot0.tick,
        1,
        20,
        BlockTag::Latest,
    )
    .expect("tick window");
    println!(
        "tick window ({} initialised ticks near tick {}):",
        ticks.len(),
        slot0.tick
    );
    for t in ticks.iter().take(6) {
        println!(
            "  tick={} liquidityGross={} liquidityNet={}",
            t.tick, t.liquidity_gross, t.liquidity_net
        );
    }
}

fn hexa(s: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

fn hex32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}
