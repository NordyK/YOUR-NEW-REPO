//! Offline DODO PMM kernel validation against live on-chain state.
//!
//! Fetches a real, liquid DODO pool's state (WETH/USDC DVM `0x1666ea6e…`,
//! ~$1.3M/3d volume) and quotes a known swap, comparing the kernel's implied
//! price to the real market (~$2120/WETH). If the kernel is ~12x off, that
//! reproduces the 1,216% phantom seen when DODO was re-enabled.
//!
//! Run: `ARBITRUM_RPC_URL=… cargo run -p engine-rpc --example repro_dodo`

use alloy_primitives::{Address, U256};
use engine_amm::dodo_v2::DodoPoolClass;
use engine_rpc::client::{ArbitrumRpcClient, BlockTag};

fn main() {
    let rpc = std::env::var("ARBITRUM_RPC_URL").expect("set ARBITRUM_RPC_URL");
    let client = ArbitrumRpcClient::new(&rpc);

    // WETH/USDC DODO DVM pool on Arbitrum.
    let pool: [u8; 20] = hexa("1666ea6e83bcc3473111732ef086ba47e6eddeb7");
    let weth: [u8; 20] = hexa("82AF49447D8A07E3BD95BD0D56F35241523FBAB1");
    let usdc: [u8; 20] = hexa("AF88D065E77C8CC2239327C5EDB3A432268E5831");

    let state =
        match engine_rpc::fetch_dodo_state(&client, &pool, DodoPoolClass::Dvm, BlockTag::Latest, 0)
        {
            Ok(s) => s,
            Err(e) => {
                println!("fetch_dodo_state failed: {e:?}");
                return;
            }
        };
    println!("DODO state:");
    println!("  base_token  = 0x{}", hex(state.base_token.as_slice()));
    println!("  quote_token = 0x{}", hex(state.quote_token.as_slice()));
    println!("  i (price)   = {}", state.i);
    println!("  K           = {}", state.k);
    println!("  B (base)    = {}", state.base_reserve);
    println!("  Q (quote)   = {}", state.quote_reserve);
    println!(
        "  B0/Q0       = {} / {}",
        state.base_target, state.quote_target
    );
    println!("  R           = {:?}", state.r_state);
    println!("  fee_ppm     = {}", state.fee_ppm);

    let weth_a: Address = Address::from(weth);
    let usdc_a: Address = Address::from(usdc);

    // 0.1 WETH in → expect ~212 USDC out (price ~2120, 6-dec).
    let dx_weth = U256::from(100_000_000_000_000_000u128); // 0.1e18
    match state.quote_for_tokens_exact_in(weth_a, usdc_a, dx_weth) {
        Ok(q) => {
            let out = f(q.amount_out_native, 6);
            println!(
                "\n0.1 WETH -> USDC: out={out:.4} USDC  (implied ${:.2}/WETH; expect ~2120)",
                out / 0.1
            );
        }
        Err(e) => println!("\nWETH->USDC quote err: {e:?}"),
    }
    // 200 USDC in → expect ~0.094 WETH out.
    let dx_usdc = U256::from(200_000_000u128); // 200e6
    match state.quote_for_tokens_exact_in(usdc_a, weth_a, dx_usdc) {
        Ok(q) => {
            let out = f(q.amount_out_native, 18);
            println!(
                "200 USDC -> WETH: out={out:.6} WETH  (implied ${:.2}/WETH; expect ~2120)",
                200.0 / out
            );
        }
        Err(e) => println!("USDC->WETH quote err: {e:?}"),
    }
}

fn f(x: U256, dec: u32) -> f64 {
    let s = x.to_string().parse::<f64>().unwrap_or(0.0);
    s / 10f64.powi(dec as i32)
}

fn hexa(s: &str) -> [u8; 20] {
    let mut out = [0u8; 20];
    for i in 0..20 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
