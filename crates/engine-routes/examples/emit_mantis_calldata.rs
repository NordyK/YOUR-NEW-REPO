//! Emit ENGINE-BUILT Mantis calldata for catalog routes, for the on-fork replay test
//! (`engine/contracts/test/MantisEngineCalldata.Fork.t.sol`). The calldata here is produced
//! solely by the engine's `engine_routes::calldata::encode_mantis_mixed` builder — NOT hand
//! written — so the forge test proves the engine's own bytes execute on-chain.
//!
//! Run: cargo run -p engine-routes --example emit_mantis_calldata
//! Writes: engine/contracts/test/fixtures/engine_calldata.json

use std::str::FromStr;

use alloy_primitives::Address;
use engine_routes::calldata::{encode_mantis_mixed, MantisFunding, MantisHop};

const UNIV3: u8 = 0;
const PANCAKE: u8 = 6;
const DODO: u8 = 23;
const BALANCER: u8 = 26;

fn a(s: &str) -> Address {
    Address::from_str(s).expect("addr")
}

fn pool_id(s: &str) -> [u8; 32] {
    let s = s.trim_start_matches("0x");
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
    }
    out
}

fn hop(
    exchange_id: u8,
    token_in: Address,
    token_out: Address,
    fee: u32,
    pool: Address,
) -> MantisHop {
    MantisHop {
        exchange_id,
        token_in,
        token_out,
        fee,
        pool,
        ..Default::default()
    }
}

fn hex_of(b: &[u8]) -> String {
    let mut s = String::from("0x");
    for x in b {
        s.push_str(&format!("{:02x}", x));
    }
    s
}

struct Route {
    id: &'static str,
    funding: MantisFunding,
    amount_in: u128,
    hops: [MantisHop; 3],
}

fn main() {
    let weth = a("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1");
    let wbtc = a("0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f");
    let usdc = a("0xaf88d065e77c8cC2239327C5EDb3A432268e5831");
    let usdt0 = a("0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9");
    let usdce = a("0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8");

    let p_univ3_weth_wbtc = a("0x2f5e87C9312fa29aed5c179E456625D79015299c"); // fee 500
    let p_pancake_weth_wbtc = a("0x4bfc22A4dA7f31F8a912a79A7e44a822398b4390"); // fee 100
    let p_dodo_wbtc_usdc = a("0x0D32A3bB04e8fF29f0974C9D3f3d85012a0B5D14");
    let p_univ3_weth_usdc = a("0xC6962004f452bE9203591991D15f6b388e09E8D0"); // fee 500
    let p_pancake_weth_usdc = a("0x7fCDC35463E3770c2fB992716Cd070B63540b947"); // fee 100
    let p_bal = a("0x1533a3278f3F9141d5F820a184ea4B017fce2382");
    let p_univ3_usdce_usdt0 = a("0x8c9D230D45d6CfeE39a6680Fb7CB7E8DE7Ea8E71"); // fee 100
    let bal_pool_id = pool_id("0x1533a3278f3f9141d5f820a184ea4b017fce2382000000000000000000000016");

    let amt_weth: u128 = 20_000_000_000_000_000; // 0.02 WETH

    // 5 catalog routes (no Balancer leg) -> Balancer/DEX-flash entrypoint (mixed).
    let mut routes: Vec<Route> = vec![
        Route {
            id: "2665",
            funding: MantisFunding::BalancerOrFlashSwap,
            amount_in: amt_weth,
            hops: [
                hop(UNIV3, weth, wbtc, 500, p_univ3_weth_wbtc),
                hop(DODO, wbtc, usdc, 0, p_dodo_wbtc_usdc),
                hop(UNIV3, usdc, weth, 500, p_univ3_weth_usdc),
            ],
        },
        Route {
            id: "2967",
            funding: MantisFunding::BalancerOrFlashSwap,
            amount_in: amt_weth,
            hops: [
                hop(PANCAKE, weth, wbtc, 100, p_pancake_weth_wbtc),
                hop(DODO, wbtc, usdc, 0, p_dodo_wbtc_usdc),
                hop(UNIV3, usdc, weth, 500, p_univ3_weth_usdc),
            ],
        },
        Route {
            id: "2668",
            funding: MantisFunding::BalancerOrFlashSwap,
            amount_in: amt_weth,
            hops: [
                hop(UNIV3, weth, wbtc, 500, p_univ3_weth_wbtc),
                hop(DODO, wbtc, usdc, 0, p_dodo_wbtc_usdc),
                hop(PANCAKE, usdc, weth, 100, p_pancake_weth_usdc),
            ],
        },
        Route {
            id: "2970",
            funding: MantisFunding::BalancerOrFlashSwap,
            amount_in: amt_weth,
            hops: [
                hop(PANCAKE, weth, wbtc, 100, p_pancake_weth_wbtc),
                hop(DODO, wbtc, usdc, 0, p_dodo_wbtc_usdc),
                hop(PANCAKE, usdc, weth, 100, p_pancake_weth_usdc),
            ],
        },
        Route {
            id: "2621",
            funding: MantisFunding::BalancerOrFlashSwap,
            amount_in: amt_weth,
            hops: [
                hop(UNIV3, weth, wbtc, 500, p_univ3_weth_wbtc),
                hop(DODO, wbtc, usdc, 0, p_dodo_wbtc_usdc),
                hop(UNIV3, usdc, weth, 500, p_univ3_weth_usdc),
            ],
        },
    ];

    // 1 Balancer-leg route -> MUST use the Aave entrypoint (touches_balancer = true).
    // USDt0 -> USDC.e (Balancer stable pool) -> USDt0 (UniV3 0.01%). 2-hop (hop[2] empty).
    let mut bal_hop0 = hop(BALANCER, usdt0, usdce, 0, p_bal);
    bal_hop0.pool_id = bal_pool_id;
    routes.push(Route {
        id: "BAL_USDT0_USDCE",
        funding: MantisFunding::from_touches_balancer(true), // -> Aave
        amount_in: 100_000_000, // 100 USDt0 (6 decimals), Aave-listed borrow token
        hops: [
            bal_hop0,
            hop(UNIV3, usdce, usdt0, 100, p_univ3_usdce_usdt0),
            MantisHop::default(),
        ],
    });

    // Assert in-builder the funding selection matches touches_balancer for every route.
    for r in &routes {
        let touches_bal = r.hops.iter().any(|h| h.exchange_id == BALANCER);
        assert_eq!(
            r.funding,
            MantisFunding::from_touches_balancer(touches_bal),
            "funding mismatch {}",
            r.id
        );
    }

    // Emit as parallel arrays for unambiguous forge `vm.parseJson*Array` consumption.
    let mut route_ids = Vec::new();
    let mut fundings = Vec::new();
    let mut selectors = Vec::new();
    let mut amount_ins = Vec::new();
    let mut calldatas = Vec::new();
    for r in &routes {
        let cd = encode_mantis_mixed(r.funding, r.amount_in, &r.hops, 0);
        route_ids.push(format!("\"{}\"", r.id));
        fundings.push(format!(
            "\"{}\"",
            if r.funding == MantisFunding::Aave {
                "aave"
            } else {
                "mixed"
            }
        ));
        selectors.push(format!("\"{}\"", hex_of(&r.funding.selector())));
        amount_ins.push(format!("\"{}\"", r.amount_in));
        calldatas.push(format!("\"{}\"", hex_of(&cd)));
    }
    let json = format!(
        "{{\n  \"route_ids\": [{}],\n  \"fundings\": [{}],\n  \"selectors\": [{}],\n  \"amount_ins\": [{}],\n  \"calldatas\": [{}]\n}}\n",
        route_ids.join(","),
        fundings.join(","),
        selectors.join(","),
        amount_ins.join(","),
        calldatas.join(",")
    );

    let out_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/test/fixtures/engine_calldata.json"
    );
    std::fs::create_dir_all(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/test/fixtures"
    ))
    .expect("mkdir");
    std::fs::write(out_path, &json).expect("write json");
    eprintln!("wrote {} routes -> {}", routes.len(), out_path);
    println!("{json}");
}
